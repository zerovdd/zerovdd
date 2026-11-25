//! Rust source lexer extracting comments and `unsafe` keyword occurrences without false positives.

/// A Rust line or block comment extracted by [`lex_rust_source`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentToken {
    /// 1-based line number where the comment begins.
    pub line: usize,
    /// Whether this token is a block comment (`/* ... */`).
    pub is_block: bool,
    /// True if only whitespace preceded this comment on its starting line.
    pub is_standalone_line: bool,
    /// Raw text of the comment including delimiters.
    pub text: String,
}

/// An occurrence of the `unsafe` keyword extracted by [`lex_rust_source`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsafeOccurrence {
    /// 1-based line number of the `unsafe` keyword.
    pub line: usize,
    /// True when `unsafe` opens a `{` block on the same line that is not closed on that line.
    pub opens_multiline_block: bool,
}

/// Combined output of [`lex_rust_source`].
#[derive(Debug, Default)]
pub struct RustLexResult {
    /// All line and block comments found in source order.
    pub comments: Vec<CommentToken>,
    /// All `unsafe` keyword tokens outside comments and string/char literals.
    pub unsafe_occurrences: Vec<UnsafeOccurrence>,
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Lexes Rust source into comments and `unsafe` keyword occurrences while accurately skipping
/// string literals (including backslash line continuations), raw strings, byte/char literals
/// (including escaped quotes like `'\''`), and nested block comments.
pub fn lex_rust_source(source: &str) -> RustLexResult {
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;
    let mut line = 1usize;
    let mut saw_code_on_line = false;
    let mut result = RustLexResult::default();

    // WHY: Track an `unsafe` token encountered on the current line. Only `{` appearing as the
    // immediate next code token opens an `unsafe { ...` block (excluding `unsafe fn ... {`),
    // and closing that block on the same line immediately clears tracking so a trailing `{`
    // (such as `if unsafe { *p } != 0 {`) can never reopen it.
    let mut pending_unsafe_idx: Option<usize> = None;
    let mut pending_unsafe_expect_brace = false;
    let mut pending_unsafe_saw_open_brace = false;
    let mut pending_unsafe_brace_depth: i32 = 0;

    while i < len {
        let b = bytes[i];
        if b == b'\n' {
            if let Some(idx) = pending_unsafe_idx.take() {
                result.unsafe_occurrences[idx].opens_multiline_block =
                    pending_unsafe_saw_open_brace && pending_unsafe_brace_depth > 0;
            }
            pending_unsafe_expect_brace = false;
            pending_unsafe_saw_open_brace = false;
            pending_unsafe_brace_depth = 0;
            line += 1;
            saw_code_on_line = false;
            i += 1;
            continue;
        }

        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }

        // WHY: Skip raw string literals (r"...", r#"..."#, br#"..."#, cr#"..."#) so embedded
        // comment delimiters or `unsafe` tokens inside string fixtures never trigger false positives.
        if matches!(b, b'r' | b'b' | b'c') {
            let mut j = i;
            if matches!(b, b'b' | b'c') && j + 1 < len && bytes[j + 1] == b'r' {
                j += 1;
            }
            if j < len && bytes[j] == b'r' {
                let mut hashes = 0usize;
                let mut k = j + 1;
                while k < len && bytes[k] == b'#' {
                    hashes += 1;
                    k += 1;
                }
                if k < len && bytes[k] == b'"' {
                    saw_code_on_line = true;
                    if pending_unsafe_expect_brace {
                        pending_unsafe_idx = None;
                        pending_unsafe_expect_brace = false;
                    }
                    i = k + 1;
                    while i < len {
                        if bytes[i] == b'\n' {
                            if let Some(idx) = pending_unsafe_idx.take() {
                                result.unsafe_occurrences[idx].opens_multiline_block =
                                    pending_unsafe_saw_open_brace && pending_unsafe_brace_depth > 0;
                            }
                            pending_unsafe_expect_brace = false;
                            pending_unsafe_saw_open_brace = false;
                            pending_unsafe_brace_depth = 0;
                            line += 1;
                            saw_code_on_line = true;
                            i += 1;
                            continue;
                        }
                        if bytes[i] == b'"' {
                            let mut close_hashes = 0usize;
                            let mut m = i + 1;
                            while close_hashes < hashes && m < len && bytes[m] == b'#' {
                                close_hashes += 1;
                                m += 1;
                            }
                            if close_hashes == hashes {
                                i = m;
                                break;
                            }
                        }
                        i += 1;
                    }
                    continue;
                }
            }
        }

        // WHY: Skip byte/c-string prefixes (`b"..."`, `c"..."`) alongside normal string literals
        // and count newlines consumed by backslash line continuations (`\` + `\n` or `\r\n`).
        if b == b'"' || (matches!(b, b'b' | b'c') && i + 1 < len && bytes[i + 1] == b'"') {
            saw_code_on_line = true;
            if pending_unsafe_expect_brace {
                pending_unsafe_idx = None;
                pending_unsafe_expect_brace = false;
            }
            i += if b == b'"' { 1 } else { 2 };
            while i < len {
                if bytes[i] == b'\\' {
                    if i + 1 < len && bytes[i + 1] == b'\n' {
                        line += 1;
                        i += 2;
                        continue;
                    }
                    if i + 2 < len && bytes[i + 1] == b'\r' && bytes[i + 2] == b'\n' {
                        line += 1;
                        i += 3;
                        continue;
                    }
                    i = (i + 2).min(len);
                    continue;
                }
                if bytes[i] == b'\n' {
                    if let Some(idx) = pending_unsafe_idx.take() {
                        result.unsafe_occurrences[idx].opens_multiline_block =
                            pending_unsafe_saw_open_brace && pending_unsafe_brace_depth > 0;
                    }
                    pending_unsafe_expect_brace = false;
                    pending_unsafe_saw_open_brace = false;
                    pending_unsafe_brace_depth = 0;
                    line += 1;
                }
                if bytes[i] == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }

        // WHY: Handle char and byte-char literals including escaped single quotes (`'\''`) by
        // starting the closing-quote search at `quote_pos + 3` after a backslash escape.
        if b == b'\'' || (b == b'b' && i + 1 < len && bytes[i + 1] == b'\'') {
            saw_code_on_line = true;
            if pending_unsafe_expect_brace {
                pending_unsafe_idx = None;
                pending_unsafe_expect_brace = false;
            }
            let quote_pos = if b == b'\'' { i } else { i + 1 };
            if quote_pos + 2 < len && bytes[quote_pos + 1] == b'\\' {
                let mut k = quote_pos + 3;
                while k < len && bytes[k] != b'\'' && bytes[k] != b'\n' {
                    k += 1;
                }
                if k < len && bytes[k] == b'\'' {
                    i = k + 1;
                    continue;
                }
            } else if quote_pos + 1 < len {
                if let Some(ch) = source[quote_pos + 1..].chars().next() {
                    let close_pos = quote_pos + 1 + ch.len_utf8();
                    if ch != '\n' && close_pos < len && bytes[close_pos] == b'\'' {
                        i = close_pos + 1;
                        continue;
                    }
                }
            }
            i += 1;
            continue;
        }

        if b == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
            let start = i;
            let start_line = line;
            let standalone = !saw_code_on_line;
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            let text = source[start..i].trim_end_matches('\r').to_string();
            result.comments.push(CommentToken {
                line: start_line,
                is_block: false,
                is_standalone_line: standalone,
                text,
            });
            continue;
        }

        if b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            let start = i;
            let start_line = line;
            let standalone = !saw_code_on_line;
            let mut depth = 1usize;
            i += 2;
            while i < len && depth > 0 {
                if bytes[i] == b'\n' {
                    if let Some(idx) = pending_unsafe_idx.take() {
                        result.unsafe_occurrences[idx].opens_multiline_block =
                            pending_unsafe_saw_open_brace && pending_unsafe_brace_depth > 0;
                    }
                    pending_unsafe_expect_brace = false;
                    pending_unsafe_saw_open_brace = false;
                    pending_unsafe_brace_depth = 0;
                    line += 1;
                    saw_code_on_line = false;
                    i += 1;
                } else if bytes[i] == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if bytes[i] == b'*' && i + 1 < len && bytes[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            let text = source[start..i].to_string();
            result.comments.push(CommentToken {
                line: start_line,
                is_block: true,
                is_standalone_line: standalone,
                text,
            });
            continue;
        }

        saw_code_on_line = true;

        if pending_unsafe_expect_brace {
            pending_unsafe_expect_brace = false;
            if b == b'{' {
                pending_unsafe_saw_open_brace = true;
                pending_unsafe_brace_depth = 1;
                i += 1;
                continue;
            }
            pending_unsafe_idx = None;
            pending_unsafe_saw_open_brace = false;
            pending_unsafe_brace_depth = 0;
        } else if let Some(idx) = pending_unsafe_idx {
            if b == b'{' {
                pending_unsafe_brace_depth += 1;
                i += 1;
                continue;
            }
            if b == b'}' && pending_unsafe_saw_open_brace {
                pending_unsafe_brace_depth -= 1;
                if pending_unsafe_brace_depth == 0 {
                    result.unsafe_occurrences[idx].opens_multiline_block = false;
                    pending_unsafe_idx = None;
                    pending_unsafe_saw_open_brace = false;
                }
                i += 1;
                continue;
            }
        }

        if is_ident_char(b) {
            let start = i;
            let kw_line = line;
            while i < len && is_ident_char(bytes[i]) {
                i += 1;
            }
            if &source[start..i] == "unsafe" {
                let is_raw_ident = start >= 2 && &bytes[start - 2..start] == b"r#";
                if !is_raw_ident {
                    let idx = result.unsafe_occurrences.len();
                    result.unsafe_occurrences.push(UnsafeOccurrence {
                        line: kw_line,
                        opens_multiline_block: false,
                    });
                    pending_unsafe_idx = Some(idx);
                    pending_unsafe_expect_brace = true;
                    pending_unsafe_saw_open_brace = false;
                    pending_unsafe_brace_depth = 0;
                }
            }
            continue;
        }

        i += 1;
    }

    if let Some(idx) = pending_unsafe_idx.take() {
        result.unsafe_occurrences[idx].opens_multiline_block =
            pending_unsafe_saw_open_brace && pending_unsafe_brace_depth > 0;
    }

    result
}
