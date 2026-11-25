use std::fs;

use syn::visit::{self, Visit};
use syn::{Attribute, Expr, Lit, Macro};
use zerovdd_checks::{collect_rust_files, relative_display, repo_root};

struct SuppressionVisitor<'a> {
    path_label: &'a str,
    violations: Vec<String>,
}

impl<'a> SuppressionVisitor<'a> {
    fn check_attr(&mut self, attr: &Attribute) {
        let attr_name = if attr.path().is_ident("allow") {
            "allow"
        } else if attr.path().is_ident("expect") {
            "expect"
        } else {
            return;
        };

        let mut has_non_empty_reason = false;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("reason") {
                if let Ok(value) = meta.value() {
                    if let Ok(Expr::Lit(expr_lit)) = value.parse::<Expr>() {
                        if let Lit::Str(lit_str) = expr_lit.lit {
                            if !lit_str.value().trim().is_empty() {
                                has_non_empty_reason = true;
                            }
                        }
                    }
                }
            } else if meta.input.peek(syn::Token![=]) {
                let _ = meta.value()?.parse::<Expr>()?;
            } else if meta.input.peek(syn::token::Paren) {
                let content;
                syn::parenthesized!(content in meta.input);
                let _ = content.parse::<SkipTokenList>();
            }
            Ok(())
        });

        if !has_non_empty_reason {
            self.violations.push(format!(
                "{}: `#[{attr_name}(...)]` attribute requires a non-empty `reason = \"...\"`",
                self.path_label
            ));
        }
    }
}

struct SkipTokenList;

impl syn::parse::Parse for SkipTokenList {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        while !input.is_empty() {
            let _ = input.parse::<Expr>();
            if input.peek(syn::Token![,]) {
                let _ = input.parse::<syn::Token![,]>();
            } else {
                break;
            }
        }
        Ok(Self)
    }
}

impl<'ast> Visit<'ast> for SuppressionVisitor<'_> {
    fn visit_attribute(&mut self, attr: &'ast Attribute) {
        self.check_attr(attr);
        visit::visit_attribute(self, attr);
    }

    fn visit_macro(&mut self, mac: &'ast Macro) {
        if let Some(last) = mac.path.segments.last() {
            let name = last.ident.to_string();
            if name == "todo" || name == "unimplemented" {
                self.violations.push(format!(
                    "{}: forbidden `{name}!()` macro in source",
                    self.path_label
                ));
            }
        }
        visit::visit_macro(self, mac);
    }
}

fn check_source(path_label: &str, source: &str) -> Vec<String> {
    let mut visitor = SuppressionVisitor {
        path_label,
        violations: Vec::new(),
    };

    if let Ok(parsed) = syn::parse_file(source) {
        visitor.visit_file(&parsed);
    } else {
        visitor.violations.push(format!(
            "{path_label}: failed to parse Rust source for suppression/todo audit"
        ));
    }

    let todo_marker = format!("{}{}:", "TO", "DO");
    let fixme_marker = format!("{}{}:", "FIX", "ME");
    for (idx, raw_line) in source.lines().enumerate() {
        let trimmed = raw_line.trim();
        if trimmed.starts_with("//")
            && (trimmed.contains(&todo_marker) || trimmed.contains(&fixme_marker))
        {
            visitor.violations.push(format!(
                "{path_label}:{}: unresolved deferred-work marker in comment",
                idx + 1
            ));
        }
    }

    visitor.violations
}

#[test]
fn repo_has_no_unreasoned_allows_or_todo_macros() {
    let repo = repo_root();
    let mut violations = Vec::new();

    for file in collect_rust_files(&repo) {
        let rel = relative_display(&repo, &file);
        let source = fs::read_to_string(&file).expect("failed to read rust source file");
        violations.extend(check_source(&rel, &source));
    }

    assert!(
        violations.is_empty(),
        "suppression_and_todo_lint violations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn rejects_todo_unimplemented_and_qualified_macros() {
    assert!(!check_source("todo_fixture.rs", "fn f() { todo!(); }\n").is_empty());
    assert!(!check_source("unimpl_fixture.rs", "fn f() { unimplemented!(); }\n").is_empty());
    assert!(!check_source("qual_todo.rs", "fn f() { core::todo!(); }\n").is_empty());
    assert!(!check_source("qual_unimpl.rs", "fn f() { std::unimplemented!(); }\n").is_empty());
}

#[test]
fn rejects_bare_or_empty_reason_allow_and_expect() {
    assert!(!check_source("allow.rs", "#[allow(dead_code)]\nfn f() {}\n").is_empty());
    assert!(!check_source("expect.rs", "#[expect(dead_code)]\nfn f() {}\n").is_empty());
    assert!(
        !check_source(
            "empty_reason.rs",
            "#[allow(dead_code, reason = \"   \")]\nfn f() {}\n"
        )
        .is_empty(),
        "expected empty reason string to be rejected"
    );
}

#[test]
fn allows_reasoned_allow_and_expect_attributes() {
    let good = concat!(
        "#[allow(dead_code, reason = \"exercised on Windows target\")]\n",
        "fn win_only() {}\n",
        "#[expect(dead_code, reason = \"used behind cfg feature\")]\n",
        "fn feature_only() {}\n",
    );
    let violations = check_source("good_fixture.rs", good);
    assert!(
        violations.is_empty(),
        "expected reasoned allow/expect to pass, got: {violations:?}"
    );
}
