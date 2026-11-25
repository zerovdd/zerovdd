use std::fs;
use std::path::Path;

use crate::cfg_helpers::cfg_is_test_only;
use syn::{Attribute, ImplItem, Item, Visibility};
use zerovdd_checks::{collect_rust_files, lex_rust_source, relative_display, repo_root};

fn has_doc_attr(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| attr.path().is_ident("doc"))
}

fn is_public(vis: &Visibility) -> bool {
    matches!(vis, Visibility::Public(_))
}

fn check_items_public_docs(path_label: &str, items: &[Item], violations: &mut Vec<String>) {
    for item in items {
        match item {
            Item::Mod(item_mod) => {
                if cfg_is_test_only(&item_mod.attrs) {
                    continue;
                }
                if is_public(&item_mod.vis) && !has_doc_attr(&item_mod.attrs) {
                    violations.push(format!(
                        "{path_label}: public mod `{}` is missing a doc comment",
                        item_mod.ident
                    ));
                }
                if let Some((_, inner_items)) = &item_mod.content {
                    check_items_public_docs(path_label, inner_items, violations);
                }
            }
            Item::Impl(item_impl) => {
                if cfg_is_test_only(&item_impl.attrs) || item_impl.trait_.is_some() {
                    continue;
                }
                for impl_item in &item_impl.items {
                    match impl_item {
                        ImplItem::Fn(method)
                            if is_public(&method.vis) && !has_doc_attr(&method.attrs) =>
                        {
                            violations.push(format!(
                                "{path_label}: public method `{}` is missing a doc comment",
                                method.sig.ident
                            ));
                        }
                        ImplItem::Const(c) if is_public(&c.vis) && !has_doc_attr(&c.attrs) => {
                            violations.push(format!(
                                "{path_label}: public associated const `{}` is missing a doc comment",
                                c.ident
                            ));
                        }
                        ImplItem::Type(t) if is_public(&t.vis) && !has_doc_attr(&t.attrs) => {
                            violations.push(format!(
                                "{path_label}: public associated type `{}` is missing a doc comment",
                                t.ident
                            ));
                        }
                        _ => {}
                    }
                }
            }
            Item::Fn(item_fn) if is_public(&item_fn.vis) && !has_doc_attr(&item_fn.attrs) => {
                violations.push(format!(
                    "{path_label}: public fn `{}` is missing a doc comment",
                    item_fn.sig.ident
                ));
            }
            Item::Struct(item_struct)
                if is_public(&item_struct.vis) && !has_doc_attr(&item_struct.attrs) =>
            {
                violations.push(format!(
                    "{path_label}: public struct `{}` is missing a doc comment",
                    item_struct.ident
                ));
            }
            Item::Enum(item_enum)
                if is_public(&item_enum.vis) && !has_doc_attr(&item_enum.attrs) =>
            {
                violations.push(format!(
                    "{path_label}: public enum `{}` is missing a doc comment",
                    item_enum.ident
                ));
            }
            Item::Trait(item_trait)
                if is_public(&item_trait.vis) && !has_doc_attr(&item_trait.attrs) =>
            {
                violations.push(format!(
                    "{path_label}: public trait `{}` is missing a doc comment",
                    item_trait.ident
                ));
            }
            Item::Type(item_type)
                if is_public(&item_type.vis) && !has_doc_attr(&item_type.attrs) =>
            {
                violations.push(format!(
                    "{path_label}: public type `{}` is missing a doc comment",
                    item_type.ident
                ));
            }
            Item::Const(item_const)
                if is_public(&item_const.vis) && !has_doc_attr(&item_const.attrs) =>
            {
                violations.push(format!(
                    "{path_label}: public const `{}` is missing a doc comment",
                    item_const.ident
                ));
            }
            Item::Static(item_static)
                if is_public(&item_static.vis) && !has_doc_attr(&item_static.attrs) =>
            {
                violations.push(format!(
                    "{path_label}: public static `{}` is missing a doc comment",
                    item_static.ident
                ));
            }
            _ => {}
        }
    }
}

fn check_public_api_docs(path_label: &str, source: &str) -> Vec<String> {
    let mut violations = Vec::new();
    match syn::parse_file(source) {
        Ok(parsed) => check_items_public_docs(path_label, &parsed.items, &mut violations),
        Err(err) => violations.push(format!(
            "{path_label}: failed to parse Rust source for public-API doc audit: {err}"
        )),
    }
    violations
}

fn check_comment_policy(path_label: &str, source: &str) -> Vec<String> {
    let mut violations = Vec::new();
    let comments = lex_rust_source(source).comments;
    let mut active_structured_line: Option<usize> = None;

    for token in comments {
        if token.is_block {
            active_structured_line = None;
            let is_doc_block = (token.text.starts_with("/**") && !token.text.starts_with("/***"))
                || token.text.starts_with("/*!");
            if !is_doc_block {
                violations.push(format!(
                    "{path_label}:{}: non-doc block comment found; use doc comments or `// WHY:` / `// SAFETY:`",
                    token.line
                ));
            }
            continue;
        }

        let trimmed = token.text.trim();
        let is_doc_line = (trimmed.starts_with("///") && !trimmed.starts_with("////"))
            || trimmed.starts_with("//!");
        if is_doc_line {
            active_structured_line = None;
            continue;
        }

        let body = trimmed.strip_prefix("//").unwrap_or("").trim_start();
        if body.starts_with("WHY:") || body.starts_with("SAFETY:") {
            active_structured_line = if token.is_standalone_line {
                Some(token.line)
            } else {
                None
            };
            continue;
        }

        if token.is_standalone_line {
            if let Some(prev_line) = active_structured_line {
                if token.line == prev_line + 1 {
                    active_structured_line = Some(token.line);
                    continue;
                }
            }
        }

        active_structured_line = None;
        violations.push(format!(
            "{path_label}:{}: narrative comment `{trimmed}` must be a doc comment (`///`) or a `// WHY:` / `// SAFETY:` block",
            token.line
        ));
    }

    violations
}

fn check_source(path_label: &str, source: &str, check_pub_docs: bool) -> Vec<String> {
    let mut violations = check_comment_policy(path_label, source);
    if check_pub_docs {
        violations.extend(check_public_api_docs(path_label, source));
    }
    violations
}

#[test]
fn repo_enforces_doc_comments_and_why_safety_comments_only() {
    let repo = repo_root();
    let mut violations = Vec::new();

    for file in collect_rust_files(&repo) {
        let rel = relative_display(&repo, &file);
        if file.file_name().is_some_and(|name| name == "mod.rs") {
            violations.push(format!(
                "{rel}: legacy mod.rs layout is forbidden; use Rust 2018+ foo.rs + foo/"
            ));
        }
        let source = fs::read_to_string(&file).expect("failed to read rust source file");
        let is_src_file = rel.contains("/src/") && !Path::new(&rel).ends_with("main.rs");
        violations.extend(check_source(&rel, &source, is_src_file));
    }

    assert!(
        violations.is_empty(),
        "doc_comments_only_lint violations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn rejects_narrative_comment_and_trailing_comment_after_why() {
    let bad =
        "/// Adds one.\npub fn inc(x: u32) -> u32 {\n    // increment counter\n    x + 1\n}\n";
    assert!(
        !check_source("fixture.rs", bad, true).is_empty(),
        "expected narrative comment fixture to be rejected"
    );

    let trailing = concat!(
        "/// Adds one.\n",
        "pub fn inc(x: u32) -> u32 {\n",
        "    // WHY: prevents wrap\n",
        "    x.saturating_add(1) // trailing narrative comment\n",
        "}\n",
    );
    assert!(
        !check_source("trailing.rs", trailing, true).is_empty(),
        "expected trailing narrative comment immediately after // WHY: to be rejected"
    );
}

#[test]
fn rejects_undocumented_public_fn_mod_impl_method_and_parse_errors() {
    let bad_fn = "pub fn undocumented_api() -> u32 {\n    42\n}\n";
    assert!(!check_source("fn.rs", bad_fn, true).is_empty());

    let bad_mod = "pub mod inner {\n    pub fn nested() {}\n}\n";
    let mod_violations = check_source("mod.rs", bad_mod, true);
    assert!(
        mod_violations.len() >= 2,
        "expected both pub mod and nested pub fn violations, got: {mod_violations:?}"
    );

    let bad_impl =
        "/// Struct doc.\npub struct S;\nimpl S {\n    pub fn undocumented_method(&self) {}\n}\n";
    assert!(
        !check_source("impl.rs", bad_impl, true).is_empty(),
        "expected undocumented inherent pub method to be rejected"
    );

    let unparseable = "pub fn broken( {\n";
    assert!(
        !check_source("broken.rs", unparseable, true).is_empty(),
        "expected syn::parse_file failure to emit violation instead of failing open"
    );

    let bad_any_cfg = concat!(
        "#[cfg(any(test, windows))]\n",
        "pub mod win_or_test {\n",
        "    pub fn exposed() {}\n",
        "}\n",
    );
    assert!(
        !check_source("any_cfg.rs", bad_any_cfg, true).is_empty(),
        "expected #[cfg(any(test, windows))] pub mod to still require doc comments"
    );

    let ok_compound_test_cfg = concat!(
        "#[cfg(all(feature = \"std\", test))]\n",
        "mod test_helpers {\n",
        "    pub fn helper() {}\n",
        "}\n",
        "#[cfg(all(not(miri), test))]\n",
        "impl Helper {\n",
        "    pub fn build() {}\n",
        "}\n",
    );
    assert!(
        check_source("compound_test.rs", ok_compound_test_cfg, true).is_empty(),
        "expected compound test-only cfg blocks to be exempt from public API doc checks"
    );
}

#[test]
fn allows_doc_comments_why_safety_escaped_quotes_and_trait_impls() {
    let good = concat!(
        "//! Module doc.\n",
        "/// Public module.\n",
        "pub mod api {\n",
        "    /// Public struct.\n",
        "    pub struct Endpoint;\n",
        "    impl Endpoint {\n",
        "        /// Public method.\n",
        "        pub fn quotes(&self) -> [char; 2] {\n",
        "            // WHY: explains a non-obvious invariant\n",
        "            // across multiple standalone continuation lines.\n",
        "            ['\\'', '\"']\n",
        "        }\n",
        "    }\n",
        "    impl Default for Endpoint {\n",
        "        fn default() -> Self { Self }\n",
        "    }\n",
        "}\n",
    );
    let violations = check_source("fixture.rs", good, true);
    assert!(
        violations.is_empty(),
        "expected valid fixture to pass, got: {violations:?}"
    );
}
