use std::fs;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

fn zerovdd_bin() -> &'static str {
    env!("CARGO_BIN_EXE_zerovdd")
}

#[test]
fn sunshine_help_lists_all_subcommands() {
    let top_out = Command::new(zerovdd_bin())
        .arg("--help")
        .output()
        .expect("run zerovdd --help");
    assert!(top_out.status.success());
    let top_stdout = String::from_utf8_lossy(&top_out.stdout);
    assert!(
        top_stdout.contains("sunshine"),
        "expected `sunshine` in top-level help:\n{top_stdout}"
    );

    let sub_out = Command::new(zerovdd_bin())
        .args(["sunshine", "--help"])
        .output()
        .expect("run zerovdd sunshine --help");
    assert!(sub_out.status.success());
    let sub_stdout = String::from_utf8_lossy(&sub_out.stdout);
    for verb in ["start", "stop", "status", "configure"] {
        assert!(
            sub_stdout.contains(verb),
            "expected `{verb}` in `zerovdd sunshine --help`:\n{sub_stdout}"
        );
    }
}

#[test]
fn sunshine_start_rejects_missing_or_invalid_env_and_missing_owner() {
    let missing_env = Command::new(zerovdd_bin())
        .args(["sunshine", "start"])
        .env_remove("SUNSHINE_CLIENT_WIDTH")
        .env_remove("SUNSHINE_CLIENT_HEIGHT")
        .env_remove("SUNSHINE_CLIENT_FPS")
        .env_remove("SUNSHINE_CLIENT_HDR")
        .env_remove("SUNSHINE_CLIENT_UUID")
        .env_remove("SUNSHINE_CLIENT_NAME")
        .output()
        .expect("run sunshine start without env");
    assert!(!missing_env.status.success());
    let missing_stderr = String::from_utf8_lossy(&missing_env.stderr);
    assert!(
        missing_stderr.contains("SUNSHINE_CLIENT_WIDTH") && missing_stderr.contains("--width"),
        "expected actionable error mentioning SUNSHINE_CLIENT_WIDTH and --width, got:\n{missing_stderr}"
    );

    let bad_fps = Command::new(zerovdd_bin())
        .args(["sunshine", "start", "--owner-pid", "1234"])
        .env("SUNSHINE_CLIENT_WIDTH", "1920")
        .env("SUNSHINE_CLIENT_HEIGHT", "1080")
        .env("SUNSHINE_CLIENT_FPS", "0")
        .output()
        .expect("run sunshine start with 0 fps");
    assert!(!bad_fps.status.success());

    let bad_hdr = Command::new(zerovdd_bin())
        .args(["sunshine", "start", "--owner-pid", "1234"])
        .env("SUNSHINE_CLIENT_WIDTH", "1920")
        .env("SUNSHINE_CLIENT_HEIGHT", "1080")
        .env("SUNSHINE_CLIENT_FPS", "59.94")
        .env("SUNSHINE_CLIENT_HDR", "maybe")
        .output()
        .expect("run sunshine start with invalid hdr");
    assert!(!bad_hdr.status.success());

    let no_sunshine_ancestor = Command::new(zerovdd_bin())
        .args(["sunshine", "start"])
        .env("SUNSHINE_CLIENT_WIDTH", "2560")
        .env("SUNSHINE_CLIENT_HEIGHT", "1600")
        .env("SUNSHINE_CLIENT_FPS", "119.88")
        .env("SUNSHINE_CLIENT_HDR", "true")
        .env("SUNSHINE_CLIENT_NAME", "MoonlightTest")
        .env_remove("SUNSHINE_CLIENT_UUID")
        .output()
        .expect("run sunshine start without sunshine.exe ancestor");
    assert!(!no_sunshine_ancestor.status.success());
    let owner_stderr = String::from_utf8_lossy(&no_sunshine_ancestor.stderr);
    assert!(
        owner_stderr.contains("sunshine.exe") && owner_stderr.contains("--owner-pid"),
        "expected missing sunshine.exe ancestor error, got:\n{owner_stderr}"
    );
}

#[test]
fn sunshine_configure_dry_run_and_idempotent_apply_preserve_user_settings() {
    let dir = TempDir::new().unwrap();
    let conf_path = dir.path().join("sunshine.conf");
    let initial = concat!(
        "# Existing Sunshine config\n",
        "fec_percentage = 20\n",
        "global_prep_cmd = [{\"do\":\"echo pre\",\"undo\":\"echo post\",\"elevated\":\"false\"}]\n",
        "output_name = {9acddf6d-1234-5678-9abc-def012345678}\n",
    );
    fs::write(&conf_path, initial).unwrap();

    let dry = Command::new(zerovdd_bin())
        .args(["sunshine", "configure", "--config"])
        .arg(&conf_path)
        .output()
        .expect("run sunshine configure dry-run");
    assert!(
        dry.status.success(),
        "dry-run failed: {}",
        String::from_utf8_lossy(&dry.stderr)
    );
    let dry_stdout = String::from_utf8_lossy(&dry.stdout);
    let dry_stderr = String::from_utf8_lossy(&dry.stderr);
    assert!(
        dry_stdout.contains("--apply"),
        "expected --apply hint:\n{dry_stdout}"
    );
    assert!(
        dry_stdout.contains("would add zerovdd")
            && dry_stdout.contains("would add `dd_configuration_option = disabled`")
            && !dry_stdout.contains("would added")
            && !dry_stdout.contains("would updated"),
        "expected clean dry-run verb grammar:\n{dry_stdout}"
    );
    assert!(
        dry_stderr.contains("output_name"),
        "expected pinned output_name warning:\n{dry_stderr}"
    );
    assert_eq!(fs::read_to_string(&conf_path).unwrap(), initial);

    let apply1 = Command::new(zerovdd_bin())
        .args(["sunshine", "configure", "--config"])
        .arg(&conf_path)
        .arg("--apply")
        .output()
        .expect("run sunshine configure --apply first time");
    assert!(
        apply1.status.success(),
        "first --apply failed: {}",
        String::from_utf8_lossy(&apply1.stderr)
    );
    assert!(
        String::from_utf8_lossy(&apply1.stderr).contains("output_name"),
        "expected output_name warning without --clear-output-name"
    );

    let after1 = fs::read_to_string(&conf_path).unwrap();
    assert!(after1.contains("# Existing Sunshine config"));
    assert!(after1.contains("fec_percentage = 20"));
    assert!(after1.contains("dd_configuration_option = disabled"));
    assert!(after1.contains("output_name = {9acddf6d-1234-5678-9abc-def012345678}"));

    let bak_count_1 = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("sunshine.conf.bak.")
        })
        .count();
    assert_eq!(bak_count_1, 1, "expected exactly one backup file");

    let apply2 = Command::new(zerovdd_bin())
        .args(["sunshine", "configure", "--config"])
        .arg(&conf_path)
        .arg("--apply")
        .output()
        .expect("run sunshine configure --apply second time");
    assert!(apply2.status.success());
    let after2 = fs::read_to_string(&conf_path).unwrap();
    assert_eq!(after2, after1, "second --apply must be a complete no-op");

    let prep_line = after2
        .lines()
        .find(|l| l.starts_with("global_prep_cmd = "))
        .expect("global_prep_cmd line present");
    let prep_json: Value =
        serde_json::from_str(prep_line.strip_prefix("global_prep_cmd = ").unwrap()).unwrap();
    let arr = prep_json.as_array().unwrap();
    assert_eq!(
        arr.len(),
        2,
        "must not duplicate zerovdd prep entry: {arr:?}"
    );
    assert_eq!(arr[0]["do"], "echo pre");
    assert_eq!(arr[1]["elevated"], "false");

    let clear = Command::new(zerovdd_bin())
        .args(["sunshine", "configure", "--config"])
        .arg(&conf_path)
        .args(["--apply", "--clear-output-name"])
        .output()
        .expect("run sunshine configure --apply --clear-output-name");
    assert!(clear.status.success());
    let after_clear = fs::read_to_string(&conf_path).unwrap();
    assert!(!after_clear.contains("{9acddf6d-1234-5678-9abc-def012345678}"));
    assert!(after_clear.contains("fec_percentage = 20"));
}

#[test]
fn sunshine_configure_handles_bare_relative_config_filename() {
    let dir = TempDir::new().unwrap();
    let conf_path = dir.path().join("sunshine.conf");
    fs::write(&conf_path, "fec_percentage = 20\n").unwrap();

    let out = Command::new(zerovdd_bin())
        .current_dir(dir.path())
        .args([
            "sunshine",
            "configure",
            "--config",
            "sunshine.conf",
            "--apply",
        ])
        .output()
        .expect("run sunshine configure with bare relative filename");
    assert!(
        out.status.success(),
        "bare relative --config sunshine.conf failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let updated = fs::read_to_string(&conf_path).unwrap();
    assert!(updated.contains("fec_percentage = 20"));
    assert!(updated.contains("dd_configuration_option = disabled"));
}

#[test]
fn sunshine_status_json_emits_valid_json() {
    let out = Command::new(zerovdd_bin())
        .args(["sunshine", "status", "--json"])
        .output()
        .expect("run zerovdd sunshine status --json");
    assert!(
        out.status.success(),
        "sunshine status --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: Value = serde_json::from_slice(&out.stdout).expect("valid JSON from status --json");
    assert!(
        parsed
            .get("service_running")
            .and_then(Value::as_bool)
            .is_some(),
        "expected boolean service_running field in:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
}
