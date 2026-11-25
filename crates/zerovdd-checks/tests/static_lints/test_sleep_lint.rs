use std::fs;

use crate::cfg_helpers::{cfg_mentions_test, has_test_attr};
use syn::visit::{self, Visit};
use syn::{Expr, ExprCall, ExprMethodCall, ImplItemFn, ItemFn, ItemImpl, ItemMod};
use zerovdd_checks::{collect_rust_files, relative_display, repo_root};

struct SleepVisitor<'a> {
    path_label: &'a str,
    in_test_scope: bool,
    violations: Vec<String>,
}

impl<'ast> Visit<'ast> for SleepVisitor<'_> {
    fn visit_item_mod(&mut self, item_mod: &'ast ItemMod) {
        let prev = self.in_test_scope;
        if cfg_mentions_test(&item_mod.attrs) {
            self.in_test_scope = true;
        }
        visit::visit_item_mod(self, item_mod);
        self.in_test_scope = prev;
    }

    fn visit_item_impl(&mut self, item_impl: &'ast ItemImpl) {
        let prev = self.in_test_scope;
        if cfg_mentions_test(&item_impl.attrs) {
            self.in_test_scope = true;
        }
        visit::visit_item_impl(self, item_impl);
        self.in_test_scope = prev;
    }

    fn visit_item_fn(&mut self, item_fn: &'ast ItemFn) {
        let prev = self.in_test_scope;
        if has_test_attr(&item_fn.attrs) || cfg_mentions_test(&item_fn.attrs) {
            self.in_test_scope = true;
        }
        visit::visit_item_fn(self, item_fn);
        self.in_test_scope = prev;
    }

    fn visit_impl_item_fn(&mut self, impl_fn: &'ast ImplItemFn) {
        let prev = self.in_test_scope;
        if has_test_attr(&impl_fn.attrs) || cfg_mentions_test(&impl_fn.attrs) {
            self.in_test_scope = true;
        }
        visit::visit_impl_item_fn(self, impl_fn);
        self.in_test_scope = prev;
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if self.in_test_scope {
            if let Expr::Path(expr_path) = call.func.as_ref() {
                if let Some(last) = expr_path.path.segments.last() {
                    let ident = last.ident.to_string();
                    if matches!(
                        ident.as_str(),
                        "sleep" | "sleep_ms" | "sleep_until" | "Sleep" | "SleepEx"
                    ) {
                        self.violations.push(format!(
                            "{}: forbidden `{ident}(...)` call in test code; use barriers, channels, or simulated clocks",
                            self.path_label
                        ));
                    }
                }
            }
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        if self.in_test_scope {
            let ident = call.method.to_string();
            if matches!(ident.as_str(), "sleep" | "sleep_ms" | "sleep_until") {
                self.violations.push(format!(
                    "{}: forbidden `.{ident}(...)` call in test code; use barriers, channels, or simulated clocks",
                    self.path_label
                ));
            }
        }
        visit::visit_expr_method_call(self, call);
    }
}

fn check_source(path_label: &str, source: &str, whole_file_is_test: bool) -> Vec<String> {
    let Ok(parsed) = syn::parse_file(source) else {
        return vec![format!("{path_label}: failed to parse Rust source")];
    };
    let mut visitor = SleepVisitor {
        path_label,
        in_test_scope: whole_file_is_test,
        violations: Vec::new(),
    };
    visitor.visit_file(&parsed);
    visitor.violations
}

#[test]
fn repo_tests_never_call_sleep() {
    let repo = repo_root();
    let mut violations = Vec::new();

    for file in collect_rust_files(&repo) {
        let rel = relative_display(&repo, &file);
        let source = fs::read_to_string(&file).expect("failed to read rust source file");
        let whole_file_is_test = rel.contains("/tests/");
        violations.extend(check_source(&rel, &source, whole_file_is_test));
    }

    assert!(
        violations.is_empty(),
        "test_sleep_lint violations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn rejects_std_async_win32_compound_cfg_and_impl_sleeps() {
    let bad_std = concat!(
        "#[test]\n",
        "fn flaky() {\n",
        "    std::thread::sleep(std::time::Duration::from_millis(10));\n",
        "}\n",
    );
    assert!(!check_source("flaky.rs", bad_std, false).is_empty());

    let bad_feature_test = concat!(
        "#[cfg(all(feature = \"std\", test))]\n",
        "mod tests {\n",
        "    async fn wait() {\n",
        "        tokio::time::sleep(std::time::Duration::from_secs(1)).await;\n",
        "    }\n",
        "}\n",
    );
    assert!(
        !check_source("feature_test.rs", bad_feature_test, false).is_empty(),
        "expected cfg(all(feature = \"std\", test)) sleep to be rejected"
    );

    let bad_target_os_impl = concat!(
        "#[cfg(all(target_os = \"windows\", not(miri), test))]\n",
        "impl FakeRig {\n",
        "    fn wait(&self) { SleepEx(10, 0); }\n",
        "}\n",
    );
    assert!(
        !check_source("target_os_impl.rs", bad_target_os_impl, false).is_empty(),
        "expected compound cfg impl calling SleepEx to be rejected"
    );

    let bad_any_test = concat!(
        "#[cfg(any(test, windows))]\n",
        "fn helper_wait() {\n",
        "    std::thread::sleep(std::time::Duration::from_millis(5));\n",
        "}\n",
    );
    assert!(
        !check_source("any_test.rs", bad_any_test, false).is_empty(),
        "expected #[cfg(any(test, windows))] helper sleep to be rejected"
    );
}

#[test]
fn allows_barrier_synchronization_and_not_test_production_code() {
    let good = concat!(
        "#[test]\n",
        "fn deterministic_sync() {\n",
        "    let barrier = std::sync::Barrier::new(1);\n",
        "    barrier.wait();\n",
        "}\n",
        "#[cfg(all(not(test), windows))]\n",
        "fn prod_backoff() {\n",
        "    std::thread::sleep(std::time::Duration::from_millis(1));\n",
        "}\n",
    );
    let violations = check_source("good.rs", good, false);
    assert!(
        violations.is_empty(),
        "expected barrier-based test and cfg(not(test)) production code to pass, got: {violations:?}"
    );
}
