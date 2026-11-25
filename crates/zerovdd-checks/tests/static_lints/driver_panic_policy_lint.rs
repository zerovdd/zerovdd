use std::fs;

use crate::cfg_helpers::{cfg_is_test_only, has_test_attr};
use syn::visit::{self, Visit};
use syn::{ExprIndex, ExprMethodCall, ImplItemFn, ItemFn, ItemImpl, ItemMod, Macro};
use zerovdd_checks::{collect_files, relative_display, repo_root};

struct NoPanicVisitor<'a> {
    path_label: &'a str,
    violations: Vec<String>,
}

impl<'ast> Visit<'ast> for NoPanicVisitor<'_> {
    fn visit_item_mod(&mut self, item_mod: &'ast ItemMod) {
        if cfg_is_test_only(&item_mod.attrs) {
            return;
        }
        visit::visit_item_mod(self, item_mod);
    }

    fn visit_item_impl(&mut self, item_impl: &'ast ItemImpl) {
        if cfg_is_test_only(&item_impl.attrs) {
            return;
        }
        visit::visit_item_impl(self, item_impl);
    }

    fn visit_item_fn(&mut self, item_fn: &'ast ItemFn) {
        if cfg_is_test_only(&item_fn.attrs) || has_test_attr(&item_fn.attrs) {
            return;
        }
        visit::visit_item_fn(self, item_fn);
    }

    fn visit_impl_item_fn(&mut self, impl_fn: &'ast ImplItemFn) {
        if cfg_is_test_only(&impl_fn.attrs) || has_test_attr(&impl_fn.attrs) {
            return;
        }
        visit::visit_impl_item_fn(self, impl_fn);
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        let method = call.method.to_string();
        if matches!(
            method.as_str(),
            "unwrap" | "expect" | "unwrap_err" | "expect_err"
        ) {
            self.violations.push(format!(
                "{}: `.{method}(...)` is forbidden in driver-wdk/zerovdd-driver under NoPanic policy",
                self.path_label
            ));
        }
        visit::visit_expr_method_call(self, call);
    }

    fn visit_macro(&mut self, mac: &'ast Macro) {
        if let Some(last) = mac.path.segments.last() {
            let name = last.ident.to_string();
            if matches!(
                name.as_str(),
                "panic"
                    | "unreachable"
                    | "todo"
                    | "unimplemented"
                    | "assert"
                    | "assert_eq"
                    | "assert_ne"
            ) {
                self.violations.push(format!(
                    "{}: `{name}!(...)` is forbidden in driver-wdk/zerovdd-driver under NoPanic policy",
                    self.path_label
                ));
            }
        }
        visit::visit_macro(self, mac);
    }

    fn visit_expr_index(&mut self, expr_index: &'ast ExprIndex) {
        self.violations.push(format!(
            "{}: unchecked slice/index operator `expr[idx]` is forbidden in driver-wdk/zerovdd-driver; use `.get(...)`",
            self.path_label
        ));
        visit::visit_expr_index(self, expr_index);
    }
}

fn check_driver_source(path_label: &str, source: &str) -> Vec<String> {
    let Ok(parsed) = syn::parse_file(source) else {
        return vec![format!("{path_label}: failed to parse driver Rust source")];
    };

    // WHY: `driver-wdk/zerovdd-driver` builds as `#![no_std]` with `panic = "abort"` so
    // `zerovdd.dll` remains ApiValidator-clean and never unwinds across WDF/IddCx callbacks.
    let mut visitor = NoPanicVisitor {
        path_label,
        violations: Vec::new(),
    };
    visitor.visit_file(&parsed);
    visitor.violations
}

#[test]
fn driver_crate_obeys_no_panic_policy() {
    let repo = repo_root();
    let driver_src = repo.join("driver-wdk/zerovdd-driver/src");
    if !driver_src.exists() {
        return;
    }

    let mut violations = Vec::new();
    for file in collect_files(&driver_src, &|p| {
        p.extension().is_some_and(|ext| ext == "rs")
    }) {
        let rel = relative_display(&repo, &file);
        let source = fs::read_to_string(&file).expect("failed to read driver source file");
        violations.extend(check_driver_source(&rel, &source));
    }

    assert!(
        violations.is_empty(),
        "driver_panic_policy_lint violations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn no_panic_policy_rejects_unwrap_expect_qualified_panics_asserts_indexing_and_any_cfg() {
    let bad = concat!(
        "fn handle(buf: &[u8], opt: Option<u8>, res: Result<u8, u8>) -> u8 {\n",
        "    let a = opt.unwrap();\n",
        "    let b = res.expect(\"err\");\n",
        "    let _c = res.unwrap_err();\n",
        "    let _d = res.expect_err(\"ok\");\n",
        "    core::assert_eq!(a, b);\n",
        "    if a == 0 { core::panic!(\"zero\"); }\n",
        "    a + b + buf[0]\n",
        "}\n",
    );
    let violations = check_driver_source("driver.rs", bad);
    assert!(
        violations.len() >= 7,
        "expected unwrap, expect, unwrap_err, expect_err, assert_eq!, core::panic!, and indexing violations, got: {violations:?}"
    );

    let bad_any_cfg = concat!(
        "#[cfg(any(test, windows))]\n",
        "impl Driver {\n",
        "    fn run(&self, opt: Option<u8>) -> u8 { opt.unwrap() }\n",
        "}\n",
    );
    assert!(
        !check_driver_source("any_cfg.rs", bad_any_cfg).is_empty(),
        "expected #[cfg(any(test, windows))] impl not to bypass NoPanic policy"
    );
}

#[test]
fn no_panic_policy_allows_checked_access_and_compound_test_only_cfgs() {
    let good = concat!(
        "fn handle(buf: &[u8], opt: Option<u8>) -> Option<u8> {\n",
        "    let first = buf.first().copied()?;\n",
        "    Some(first.saturating_add(opt.unwrap_or(0)))\n",
        "}\n",
        "#[test]\n",
        "fn unit_check() {\n",
        "    assert_eq!(handle(&[1], Some(2)).unwrap(), 3);\n",
        "}\n",
        "#[cfg(all(feature = \"std\", test))]\n",
        "impl Helper {\n",
        "    fn assert_ok(&self, v: Option<u8>) { assert_eq!(v.unwrap(), 1); }\n",
        "}\n",
        "impl Helper {\n",
        "    #[cfg(all(target_os = \"windows\", not(miri), test))]\n",
        "    fn win_test_only(&self, v: Option<u8>) { v.unwrap(); }\n",
        "}\n",
    );
    let violations = check_driver_source("driver.rs", good);
    assert!(
        violations.is_empty(),
        "expected checked driver code and compound test-only cfgs to pass, got: {violations:?}"
    );
}
