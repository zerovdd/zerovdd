use std::fs;

use regex::Regex;
use serde_yaml_ng::Value;
use zerovdd_checks::{collect_files, relative_display, repo_root};

fn parse_pinned_toolchain(toml_content: &str) -> Result<String, String> {
    let parsed: toml::Value = toml::from_str(toml_content)
        .map_err(|err| format!("failed to parse rust-toolchain.toml: {err}"))?;
    let channel = parsed
        .get("toolchain")
        .and_then(|t| t.get("channel"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| "rust-toolchain.toml missing [toolchain] channel".to_string())?;
    let exact_semver = Regex::new(r"^[0-9]+\.[0-9]+\.[0-9]+$").expect("valid semver regex");
    if !exact_semver.is_match(channel) {
        return Err(format!(
            "rust-toolchain.toml channel `{channel}` must be an exact X.Y.Z version"
        ));
    }
    Ok(channel.to_string())
}

fn check_clippy_msrv_lockstep(clippy_toml: &str, pinned_toolchain: &str) -> Vec<String> {
    let mut violations = Vec::new();
    let parsed: toml::Value = match toml::from_str(clippy_toml) {
        Ok(v) => v,
        Err(err) => {
            violations.push(format!("clippy.toml: failed to parse TOML: {err}"));
            return violations;
        }
    };

    match parsed.get("msrv").and_then(toml::Value::as_str) {
        Some(msrv) if msrv == pinned_toolchain => {}
        Some(msrv) => violations.push(format!(
            "clippy.toml: msrv `{msrv}` drifted from rust-toolchain.toml `{pinned_toolchain}`"
        )),
        None => violations
            .push("clippy.toml: missing `msrv` matching rust-toolchain.toml channel".to_string()),
    }

    violations
}

fn collect_yaml_uses_and_toolchains(
    val: &Value,
    uses_out: &mut Vec<String>,
    toolchains_out: &mut Vec<String>,
) {
    match val {
        Value::Mapping(map) => {
            if let Some(uses_val) = map.get(Value::String("uses".to_string())) {
                if let Some(uses_str) = uses_val.as_str() {
                    uses_out.push(uses_str.to_string());
                }
            }
            if let Some(with_map) = map
                .get(Value::String("with".to_string()))
                .and_then(Value::as_mapping)
            {
                if let Some(tc) = with_map
                    .get(Value::String("toolchain".to_string()))
                    .and_then(Value::as_str)
                {
                    toolchains_out.push(tc.to_string());
                }
            }
            if let Some(inputs_map) = map
                .get(Value::String("inputs".to_string()))
                .and_then(Value::as_mapping)
            {
                if let Some(tc_map) = inputs_map
                    .get(Value::String("toolchain".to_string()))
                    .and_then(Value::as_mapping)
                {
                    if let Some(default_tc) = tc_map
                        .get(Value::String("default".to_string()))
                        .and_then(Value::as_str)
                    {
                        toolchains_out.push(default_tc.to_string());
                    }
                }
            }
            for (_, v) in map {
                collect_yaml_uses_and_toolchains(v, uses_out, toolchains_out);
            }
        }
        Value::Sequence(seq) => {
            for item in seq {
                collect_yaml_uses_and_toolchains(item, uses_out, toolchains_out);
            }
        }
        _ => {}
    }
}

fn check_github_yaml(
    path_label: &str,
    content: &str,
    pinned_toolchain: &str,
    allow_nightly: bool,
) -> Vec<String> {
    let mut violations = Vec::new();
    let doc: Value = match serde_yaml_ng::from_str(content) {
        Ok(val) => val,
        Err(err) => {
            violations.push(format!("{path_label}: invalid YAML: {err}"));
            return violations;
        }
    };

    let sha_re = Regex::new(r"^[^/\s]+/[^@\s]+@[0-9a-f]{40}$").expect("valid sha pin regex");
    let docker_digest_re =
        Regex::new(r"^docker://[^@\s]+@sha256:[0-9a-f]{64}$").expect("valid docker digest regex");
    let comment_re =
        Regex::new(r"uses:\s*[^/\s]+/[^@\s]+@[0-9a-f]{40}\s+#\s*v[0-9]+(\.[0-9]+)*(\s|$)")
            .expect("valid version comment regex");
    let raw_uses_re = Regex::new(r"^\s*-?\s*uses:\s*(\S+)").expect("valid uses line regex");

    let mut uses_list = Vec::new();
    let mut toolchains = Vec::new();
    collect_yaml_uses_and_toolchains(&doc, &mut uses_list, &mut toolchains);

    for uses in &uses_list {
        if uses.starts_with("./") {
            continue;
        }
        if uses.starts_with("docker://") {
            if !docker_digest_re.is_match(uses) {
                violations.push(format!(
                    "{path_label}: docker action `{uses}` must pin an immutable `@sha256:<64-hex>` digest"
                ));
            }
            continue;
        }
        if !sha_re.is_match(uses) {
            violations.push(format!(
                "{path_label}: unpinned GitHub Action `{uses}`; must pin to 40-char commit SHA"
            ));
        }
    }

    for (idx, line) in content.lines().enumerate() {
        if let Some(caps) = raw_uses_re.captures(line) {
            let target = &caps[1];
            if target.starts_with("./") || target.starts_with("docker://") {
                continue;
            }
            if !comment_re.is_match(line) {
                violations.push(format!(
                    "{path_label}:{}: pinned action line must carry a `# vX.Y.Z` version comment",
                    idx + 1
                ));
            }
        }
    }

    for tc in toolchains {
        if tc.starts_with("${{") {
            continue;
        }
        if allow_nightly && tc == "nightly" {
            continue;
        }
        if tc != pinned_toolchain {
            violations.push(format!(
                "{path_label}: toolchain `{tc}` drifted from rust-toolchain.toml `{pinned_toolchain}`"
            ));
        }
    }

    if path_label.starts_with(".github/workflows/") {
        if let Some(map) = doc.as_mapping() {
            if !map.contains_key(Value::String("permissions".to_string())) {
                violations.push(format!(
                    "{path_label}: workflow must declare top-level `permissions:`"
                ));
            }
        }
    }

    violations
}

#[test]
fn repo_github_workflows_and_actions_are_sha_pinned_and_toolchain_locked() {
    let repo = repo_root();
    let toolchain_toml =
        fs::read_to_string(repo.join("rust-toolchain.toml")).expect("read rust-toolchain.toml");
    let pinned_toolchain = parse_pinned_toolchain(&toolchain_toml)
        .expect("rust-toolchain.toml must pin exact X.Y.Z channel");

    let clippy_toml = fs::read_to_string(repo.join("clippy.toml")).expect("read clippy.toml");
    let mut violations = check_clippy_msrv_lockstep(&clippy_toml, &pinned_toolchain);

    let mut yaml_files = Vec::new();
    for subdir in [".github/workflows", ".github/actions"] {
        yaml_files.extend(collect_files(&repo.join(subdir), &|p| {
            p.extension()
                .is_some_and(|ext| ext == "yml" || ext == "yaml")
        }));
    }

    assert!(
        !yaml_files.is_empty(),
        "expected .github/workflows and .github/actions YAML files to exist"
    );

    for file in yaml_files {
        let rel = relative_display(&repo, &file);
        let content = fs::read_to_string(&file).expect("failed to read github yaml");
        let allow_nightly = rel.ends_with("/fuzz.yml");
        violations.extend(check_github_yaml(
            &rel,
            &content,
            &pinned_toolchain,
            allow_nightly,
        ));
    }

    assert!(
        violations.is_empty(),
        "ci_pins_lint violations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn rejects_unpinned_tag_action_missing_comment_and_unpinned_docker() {
    let bad_tag = concat!(
        "name: Bad\n",
        "on: [push]\n",
        "permissions:\n",
        "  contents: read\n",
        "jobs:\n",
        "  build:\n",
        "    runs-on: ubuntu-latest\n",
        "    steps:\n",
        "      - uses: actions/checkout@v4\n",
    );
    assert!(!check_github_yaml(".github/workflows/bad.yml", bad_tag, "1.100.0", false).is_empty());

    let bad_no_comment = concat!(
        "name: BadComment\n",
        "on: [push]\n",
        "permissions:\n",
        "  contents: read\n",
        "jobs:\n",
        "  build:\n",
        "    runs-on: ubuntu-latest\n",
        "    steps:\n",
        "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1\n",
    );
    assert!(!check_github_yaml(
        ".github/workflows/bad.yml",
        bad_no_comment,
        "1.100.0",
        false
    )
    .is_empty());

    let bad_docker = concat!(
        "name: BadDocker\n",
        "on: [push]\n",
        "permissions:\n",
        "  contents: read\n",
        "jobs:\n",
        "  build:\n",
        "    runs-on: ubuntu-latest\n",
        "    steps:\n",
        "      - uses: docker://alpine:3.20\n",
    );
    assert!(
        !check_github_yaml(".github/workflows/docker.yml", bad_docker, "1.100.0", false).is_empty(),
        "expected docker:// action without @sha256: digest to be rejected"
    );
}

#[test]
fn rejects_drifted_rust_toolchain_on_direct_or_composite_action_and_clippy_msrv() {
    let bad_direct = concat!(
        "name: Drift\n",
        "on: [push]\n",
        "permissions:\n",
        "  contents: read\n",
        "jobs:\n",
        "  build:\n",
        "    runs-on: ubuntu-latest\n",
        "    steps:\n",
        "      - uses: dtolnay/rust-toolchain@683b588c487a69d01f6c6ddf40f036ff682cc2c8 # v1\n",
        "        with:\n",
        "          toolchain: \"1.80.0\"\n",
    );
    assert!(
        !check_github_yaml(".github/workflows/drift.yml", bad_direct, "1.100.0", false).is_empty()
    );

    let bad_composite = concat!(
        "name: CompositeDrift\n",
        "on: [push]\n",
        "permissions:\n",
        "  contents: read\n",
        "jobs:\n",
        "  build:\n",
        "    runs-on: ubuntu-latest\n",
        "    steps:\n",
        "      - uses: ./.github/actions/setup-rust\n",
        "        with:\n",
        "          toolchain: \"1.80.0\"\n",
    );
    assert!(
        !check_github_yaml(
            ".github/workflows/composite_drift.yml",
            bad_composite,
            "1.100.0",
            false
        )
        .is_empty(),
        "expected drifted toolchain on ./.github/actions/setup-rust to be rejected"
    );

    let bad_clippy = "msrv = \"1.80.0\"\n";
    assert!(
        !check_clippy_msrv_lockstep(bad_clippy, "1.100.0").is_empty(),
        "expected drifted clippy.toml msrv to be rejected"
    );
}
