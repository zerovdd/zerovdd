use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use zerovdd_checks::{collect_crate_manifests, relative_display, repo_root};

#[derive(Debug, Clone)]
struct CrateManifestInfo {
    name: String,
    rel_path: String,
    all_deps: BTreeSet<String>,
    unconditional_deps: BTreeSet<String>,
}

fn collect_dep_table(val: &toml::Value, out: &mut BTreeSet<String>) {
    if let Some(table) = val.as_table() {
        for (dep_name, spec) in table {
            let real_name = spec
                .as_table()
                .and_then(|t| t.get("package"))
                .and_then(toml::Value::as_str)
                .unwrap_or(dep_name.as_str());
            out.insert(real_name.to_string());
        }
    }
}

fn parse_manifest_info(rel_path: &str, toml_content: &str) -> Result<CrateManifestInfo, String> {
    let parsed: toml::Value = toml::from_str(toml_content)
        .map_err(|err| format!("{rel_path}: failed to parse Cargo.toml: {err}"))?;

    let name = parsed
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| format!("{rel_path}: missing [package] name"))?
        .to_string();

    let mut unconditional_deps = BTreeSet::new();
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(sec) = parsed.get(section) {
            collect_dep_table(sec, &mut unconditional_deps);
        }
    }

    let mut all_deps = unconditional_deps.clone();
    if let Some(targets) = parsed.get("target").and_then(toml::Value::as_table) {
        for (_, target_cfg) in targets {
            for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
                if let Some(sec) = target_cfg.get(section) {
                    collect_dep_table(sec, &mut all_deps);
                }
            }
        }
    }

    Ok(CrateManifestInfo {
        name,
        rel_path: rel_path.to_string(),
        all_deps,
        unconditional_deps,
    })
}

fn is_windows_dep(dep: &str) -> bool {
    dep == "windows" || dep.starts_with("windows-") || dep == "winapi" || dep.starts_with("winapi-")
}

fn is_wdk_dep(dep: &str) -> bool {
    dep == "wdk" || dep.starts_with("wdk-")
}

fn is_windows_or_wdk_dep(dep: &str) -> bool {
    is_windows_dep(dep) || is_wdk_dep(dep)
}

fn allowed_internal_deps(crate_name: &str) -> Option<&'static [&'static str]> {
    match crate_name {
        "zerovdd-checks" => Some(&[]),
        "zerovdd-proto" | "zerovdd-edid" | "zerovdd-modes" | "zerovdd-config" => Some(&[]),
        "zerovdd-engine" => Some(&["zerovdd-proto", "zerovdd-edid", "zerovdd-modes"]),
        "zerovdd-iddcx-sys" => Some(&[]),
        "zerovdd-driver" => Some(&[
            "zerovdd-engine",
            "zerovdd-proto",
            "zerovdd-edid",
            "zerovdd-modes",
            "zerovdd-iddcx-sys",
        ]),
        "zerovdd-client" | "zerovdd-installer" => Some(&["zerovdd-proto", "zerovdd-config"]),
        "zerovdd-cli" => Some(&[
            "zerovdd-client",
            "zerovdd-installer",
            "zerovdd-config",
            "zerovdd-proto",
            "zerovdd-edid",
        ]),
        "zerovdd-svc" => Some(&[
            "zerovdd-client",
            "zerovdd-config",
            "zerovdd-proto",
            "zerovdd-edid",
        ]),
        "zerovdd-tray" => Some(&["zerovdd-client", "zerovdd-config", "zerovdd-proto"]),
        _ => None,
    }
}

fn validate_crate_dag(manifests: &[CrateManifestInfo]) -> Vec<String> {
    let mut violations = Vec::new();
    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for info in manifests {
        let internal_deps: Vec<String> = info
            .all_deps
            .iter()
            .filter(|d| d.starts_with("zerovdd-"))
            .cloned()
            .collect();

        if let Some(allowed) = allowed_internal_deps(&info.name) {
            for dep in &internal_deps {
                if !allowed.contains(&dep.as_str()) {
                    violations.push(format!(
                        "{} ({}): forbidden internal dependency `{dep}` (allowed: {allowed:?})",
                        info.name, info.rel_path
                    ));
                }
            }
        } else {
            violations.push(format!(
                "{} ({}): unregistered crate must be added to crate_dag_lint",
                info.name, info.rel_path
            ));
        }

        let is_layer0_or_layer1_or_checks = matches!(
            info.name.as_str(),
            "zerovdd-checks"
                | "zerovdd-proto"
                | "zerovdd-edid"
                | "zerovdd-modes"
                | "zerovdd-config"
                | "zerovdd-engine"
        );
        if is_layer0_or_layer1_or_checks {
            for dep in &info.all_deps {
                if is_windows_or_wdk_dep(dep) {
                    violations.push(format!(
                        "{} ({}): portable Layer 0/1 crate must not depend on `{dep}`",
                        info.name, info.rel_path
                    ));
                }
            }
        }

        let is_usermode_win_crate = matches!(
            info.name.as_str(),
            "zerovdd-client" | "zerovdd-installer" | "zerovdd-cli" | "zerovdd-svc" | "zerovdd-tray"
        );
        if is_usermode_win_crate {
            for dep in &info.unconditional_deps {
                if is_windows_or_wdk_dep(dep) {
                    violations.push(format!(
                        "{} ({}): Windows crate `{dep}` must be gated under [target.'cfg(windows)'.dependencies]",
                        info.name, info.rel_path
                    ));
                }
            }
            for dep in &info.all_deps {
                if is_wdk_dep(dep) {
                    violations.push(format!(
                        "{} ({}): user-mode crate must not depend on WDK crate `{dep}`",
                        info.name, info.rel_path
                    ));
                }
            }
        }

        if info.rel_path.starts_with("driver-wdk/") {
            for forbidden in [
                "zerovdd-client",
                "zerovdd-installer",
                "zerovdd-cli",
                "zerovdd-svc",
                "zerovdd-tray",
                "zerovdd-config",
            ] {
                if info.all_deps.contains(forbidden) {
                    violations.push(format!(
                        "{} ({}): driver-wdk crate must never depend on user-mode crate `{forbidden}`",
                        info.name, info.rel_path
                    ));
                }
            }
        }

        graph.insert(info.name.clone(), internal_deps);
    }

    // WHY: Standard 3-color DFS detects cycles even if a newly introduced crate is miswired.
    let mut state: BTreeMap<String, u8> = BTreeMap::new();
    fn dfs(
        node: &str,
        graph: &BTreeMap<String, Vec<String>>,
        state: &mut BTreeMap<String, u8>,
        stack: &mut Vec<String>,
        violations: &mut Vec<String>,
    ) {
        state.insert(node.to_string(), 1);
        stack.push(node.to_string());
        if let Some(neighbors) = graph.get(node) {
            for next in neighbors {
                match state.get(next).copied().unwrap_or(0) {
                    1 => {
                        violations.push(format!(
                            "cyclic crate dependency detected: {} -> {next}",
                            stack.join(" -> ")
                        ));
                    }
                    0 => dfs(next, graph, state, stack, violations),
                    _ => {}
                }
            }
        }
        stack.pop();
        state.insert(node.to_string(), 2);
    }

    let nodes: Vec<String> = graph.keys().cloned().collect();
    let mut stack = Vec::new();
    for node in nodes {
        if state.get(&node).copied().unwrap_or(0) == 0 {
            dfs(&node, &graph, &mut state, &mut stack, &mut violations);
        }
    }

    violations
}

#[test]
fn repo_crates_obey_acyclic_layer_dag() {
    let repo = repo_root();
    let mut infos = Vec::new();
    let mut violations = Vec::new();

    for manifest in collect_crate_manifests(&repo) {
        let rel = relative_display(&repo, &manifest);
        let content = fs::read_to_string(&manifest).expect("failed to read crate Cargo.toml");
        match parse_manifest_info(&rel, &content) {
            Ok(info) => infos.push(info),
            Err(err) => violations.push(err),
        }
    }

    violations.extend(validate_crate_dag(&infos));

    assert!(
        violations.is_empty(),
        "crate_dag_lint violations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn rejects_layer0_crate_importing_windows_or_winapi() {
    let bad_proto = concat!(
        "[package]\n",
        "name = \"zerovdd-proto\"\n",
        "version = \"0.1.0\"\n",
        "[dependencies]\n",
        "windows-strings = \"0.4\"\n",
    );
    let info = parse_manifest_info("crates/zerovdd-proto/Cargo.toml", bad_proto).unwrap();
    let violations = validate_crate_dag(&[info]);
    assert!(
        !violations.is_empty(),
        "expected Layer 0 crate importing windows-* to be rejected"
    );
}

#[test]
fn rejects_usermode_crate_importing_wdk_under_windows_target() {
    let bad_client = concat!(
        "[package]\n",
        "name = \"zerovdd-client\"\n",
        "version = \"0.1.0\"\n",
        "[target.'cfg(windows)'.dependencies]\n",
        "wdk-sys = \"0.5\"\n",
    );
    let info = parse_manifest_info("crates/zerovdd-client/Cargo.toml", bad_client).unwrap();
    let violations = validate_crate_dag(&[info]);
    assert!(
        !violations.is_empty(),
        "expected user-mode crate importing wdk-sys under cfg(windows) to be rejected"
    );
}

#[test]
fn rejects_driver_wdk_importing_zerovdd_cli() {
    let bad_driver = concat!(
        "[package]\n",
        "name = \"zerovdd-driver\"\n",
        "version = \"0.1.0\"\n",
        "[dependencies]\n",
        "zerovdd-cli = { path = \"../../crates/zerovdd-cli\" }\n",
    );
    let info = parse_manifest_info("driver-wdk/zerovdd-driver/Cargo.toml", bad_driver).unwrap();
    let violations = validate_crate_dag(&[info]);
    assert!(
        !violations.is_empty(),
        "expected driver-wdk crate importing zerovdd-cli to be rejected"
    );
}

#[test]
fn rejects_checks_crate_importing_workspace_crate() {
    let bad_checks = concat!(
        "[package]\n",
        "name = \"zerovdd-checks\"\n",
        "version = \"0.1.0\"\n",
        "[dependencies]\n",
        "zerovdd-proto = { path = \"../zerovdd-proto\" }\n",
    );
    let info = parse_manifest_info("crates/zerovdd-checks/Cargo.toml", bad_checks).unwrap();
    let violations = validate_crate_dag(&[info]);
    assert!(
        !violations.is_empty(),
        "expected zerovdd-checks importing internal crate to be rejected"
    );
}
