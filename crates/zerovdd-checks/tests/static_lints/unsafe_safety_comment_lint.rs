use std::collections::BTreeMap;
use std::fs;

use zerovdd_checks::{
    collect_rust_files, lex_rust_source, relative_display, repo_root, CommentToken,
};

fn is_valid_safety_comment(token: &CommentToken) -> bool {
    if token.is_block {
        return false;
    }
    let body = token
        .text
        .trim()
        .strip_prefix("//")
        .unwrap_or("")
        .trim_start();
    body.strip_prefix("SAFETY:")
        .is_some_and(|rationale| !rationale.trim().is_empty())
}

fn is_wrapped_expression_prefix(trimmed: &str) -> bool {
    trimmed.ends_with('=')
        || trimmed == "return"
        || trimmed == "else"
        || trimmed.ends_with("=>")
        || trimmed.ends_with('(')
}

fn has_adjacent_safety_comment(
    lines: &[&str],
    comments_by_line: &BTreeMap<usize, Vec<&CommentToken>>,
    line_number_1based: usize,
    opens_multiline_block: bool,
) -> bool {
    if comments_by_line
        .get(&line_number_1based)
        .is_some_and(|list| list.iter().any(|tok| is_valid_safety_comment(tok)))
    {
        return true;
    }

    // WHY: Walk upward across standalone comments, balanced single- or multi-line `#[...]`
    // attributes, and a single `rustfmt`-wrapped expression prefix (such as `let x =` or `return`).
    let idx = line_number_1based.saturating_sub(1);
    let mut cursor = idx;
    let mut attr_bracket_depth: i32 = 0;
    let mut crossed_expr_prefix = false;

    while cursor > 0 {
        cursor -= 1;
        let cur_line_1based = cursor + 1;
        let trimmed = lines[cursor].trim();
        if trimmed.is_empty() {
            break;
        }

        if comments_by_line.get(&cur_line_1based).is_some_and(|list| {
            list.iter()
                .any(|tok| tok.is_standalone_line && is_valid_safety_comment(tok))
        }) {
            return true;
        }

        if comments_by_line
            .get(&cur_line_1based)
            .is_some_and(|list| list.iter().all(|tok| tok.is_standalone_line))
        {
            continue;
        }

        if attr_bracket_depth > 0 {
            for b in trimmed.bytes() {
                if b == b']' {
                    attr_bracket_depth += 1;
                } else if b == b'[' {
                    attr_bracket_depth -= 1;
                }
            }
            if attr_bracket_depth <= 0 {
                attr_bracket_depth = 0;
                if trimmed.starts_with('#') {
                    continue;
                }
                break;
            }
            continue;
        }

        if trimmed.starts_with('#') {
            continue;
        }

        if trimmed.ends_with(']') {
            let mut delta: i32 = 0;
            for b in trimmed.bytes() {
                if b == b']' {
                    delta += 1;
                } else if b == b'[' {
                    delta -= 1;
                }
            }
            if delta > 0 {
                attr_bracket_depth = delta;
                continue;
            }
        }

        if !crossed_expr_prefix && is_wrapped_expression_prefix(trimmed) {
            crossed_expr_prefix = true;
            continue;
        }

        break;
    }

    // WHY: Only inspect the line immediately inside `unsafe {` when this `unsafe` actually opens
    // a multi-line block; single-line `unsafe { ub(); }` must never borrow the next statement's comment.
    if opens_multiline_block {
        let next_line_1based = line_number_1based + 1;
        if comments_by_line.get(&next_line_1based).is_some_and(|list| {
            list.iter()
                .any(|tok| tok.is_standalone_line && is_valid_safety_comment(tok))
        }) {
            return true;
        }
    }

    false
}

fn check_source(path_label: &str, source: &str) -> Vec<String> {
    let mut violations = Vec::new();
    let lines: Vec<&str> = source.lines().collect();
    let lexed = lex_rust_source(source);

    let mut comments_by_line: BTreeMap<usize, Vec<&CommentToken>> = BTreeMap::new();
    for tok in &lexed.comments {
        comments_by_line.entry(tok.line).or_default().push(tok);
    }

    for occ in &lexed.unsafe_occurrences {
        if !has_adjacent_safety_comment(
            &lines,
            &comments_by_line,
            occ.line,
            occ.opens_multiline_block,
        ) {
            violations.push(format!(
                "{path_label}:{}: `unsafe` block/fn/impl requires an adjacent non-empty `// SAFETY:` comment",
                occ.line
            ));
        }
    }

    violations
}

#[test]
fn repo_requires_safety_comment_on_every_unsafe_item_or_block() {
    let repo = repo_root();
    let mut violations = Vec::new();

    for file in collect_rust_files(&repo) {
        let rel = relative_display(&repo, &file);
        let source = fs::read_to_string(&file).expect("failed to read rust source file");
        violations.extend(check_source(&rel, &source));
    }

    assert!(
        violations.is_empty(),
        "unsafe_safety_comment_lint violations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn rejects_unsafe_block_fn_empty_safety_string_fake_and_stolen_comment() {
    let bad_block = "fn bad(p: *const u8) -> u8 {\n    unsafe { *p }\n}\n";
    assert!(!check_source("bad_block.rs", bad_block).is_empty());

    let bad_fn = "pub unsafe fn raw_call() {}\n";
    assert!(!check_source("bad_fn.rs", bad_fn).is_empty());

    let bad_empty = "fn bad_empty(p: *const u8) -> u8 {\n    // SAFETY:   \n    unsafe { *p }\n}\n";
    assert!(
        !check_source("bad_empty.rs", bad_empty).is_empty(),
        "expected empty // SAFETY: comment to be rejected"
    );

    let bad_string_fake =
        "fn bad_str(p: *const u8) -> u8 {\n    let _s = \"// SAFETY: not a real comment\";\n    unsafe { *p }\n}\n";
    assert!(
        !check_source("bad_str.rs", bad_string_fake).is_empty(),
        "expected string literal containing // SAFETY: to be rejected"
    );

    let bad_stolen = concat!(
        "fn bad_steal(p: *const u8) -> u8 {\n",
        "    let a = unsafe { *p };\n",
        "    // SAFETY: applies only to second block\n",
        "    let b = unsafe { *p };\n",
        "    a + b\n",
        "}\n",
    );
    let stolen_violations = check_source("bad_steal.rs", bad_stolen);
    assert_eq!(
        stolen_violations.len(),
        1,
        "expected first single-line unsafe to be rejected without stealing next line's // SAFETY:, got: {stolen_violations:?}"
    );

    let bad_if_brace = concat!(
        "fn bad_if(p: *const u8, q: *const u8) -> u8 {\n",
        "    if unsafe { *p } != 0 {\n",
        "        // SAFETY: applies only to q\n",
        "        return unsafe { *q };\n",
        "    }\n",
        "    0\n",
        "}\n",
    );
    let if_violations = check_source("bad_if.rs", bad_if_brace);
    assert_eq!(
        if_violations.len(),
        1,
        "expected `if unsafe {{ *p }} != 0 {{` not to steal inner // SAFETY:, got: {if_violations:?}"
    );

    let bad_unsafe_fn_inner = concat!(
        "pub unsafe fn foo(p: *const u8) -> u8 {\n",
        "    // SAFETY: inner block only\n",
        "    unsafe { *p }\n",
        "}\n",
    );
    let fn_violations = check_source("bad_fn_inner.rs", bad_unsafe_fn_inner);
    assert_eq!(
        fn_violations.len(),
        1,
        "expected `pub unsafe fn foo() {{` not to steal inner // SAFETY:, got: {fn_violations:?}"
    );

    let bad_trailing_steal = concat!(
        "fn bad_trail(p: *const u8, q: *const u8) -> u8 {\n",
        "    let a = unsafe { *p }; // SAFETY: a is valid\n",
        "    let b = unsafe { *q };\n",
        "    a + b\n",
        "}\n",
    );
    let trail_violations = check_source("bad_trail.rs", bad_trailing_steal);
    assert_eq!(
        trail_violations.len(),
        1,
        "expected second unsafe not to steal previous line's trailing // SAFETY:, got: {trail_violations:?}"
    );
}

#[test]
fn handles_escaped_single_quote_and_backslash_string_continuation_before_unsafe() {
    let escaped_quote_bad = concat!(
        "fn bad_after_quote(p: *const u8) -> u8 {\n",
        "    let _chars = ['\\'', '\"'];\n",
        "    unsafe { *p }\n",
        "}\n",
    );
    assert!(
        !check_source("escaped_quote.rs", escaped_quote_bad).is_empty(),
        "expected lexer not to lose track after ['\\'', '\"'] before unsafe"
    );

    let line_cont_bad = concat!(
        "fn bad_after_cont(p: *const u8) -> u8 {\n",
        "    let _s = \"hello \\\n",
        "        world\";\n",
        "    unsafe { *p }\n",
        "}\n",
    );
    let cont_violations = check_source("line_cont.rs", line_cont_bad);
    assert_eq!(
        cont_violations.len(),
        1,
        "expected unsafe after backslash-newline string continuation to be caught on exact line"
    );
    assert!(
        cont_violations[0].contains("line_cont.rs:4:"),
        "expected exact line number 4 after backslash line continuation, got: {}",
        cont_violations[0]
    );
}

#[test]
fn allows_unsafe_with_safety_comment_and_rustfmt_wrapped_attributes() {
    let good = concat!(
        "/// Dereferences a verified pointer.\n",
        "// SAFETY: Caller guarantees `p` is non-null, aligned, and initialized.\n",
        "#[allow(\n",
        "    clippy::not_unsafe_ptr_arg_deref,\n",
        "    reason = \"demonstrates multi-line attribute\"\n",
        ")]\n",
        "pub unsafe fn read_byte(p: *const u8) -> u8 {\n",
        "    // SAFETY: Precondition upheld by caller contract above.\n",
        "    let val =\n",
        "        unsafe { *p };\n",
        "    unsafe {\n",
        "        // SAFETY: Pointer remains valid inside multi-line block.\n",
        "        val.wrapping_add(*p)\n",
        "    }\n",
        "}\n",
    );
    let violations = check_source("good.rs", good);
    assert!(
        violations.is_empty(),
        "expected documented and rustfmt-wrapped unsafe fixture to pass, got: {violations:?}"
    );
}
