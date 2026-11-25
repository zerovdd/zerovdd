use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

mod sunshine_tests;

fn zerovdd_bin() -> &'static str {
    env!("CARGO_BIN_EXE_zerovdd")
}

#[test]
fn top_level_help_lists_core_subcommands_and_hides_internal_ccd() {
    let out = Command::new(zerovdd_bin())
        .arg("--help")
        .output()
        .expect("run zerovdd --help");
    assert!(out.status.success(), "zerovdd --help failed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    for cmd in ["setup", "display", "doctor"] {
        assert!(
            stdout.contains(cmd),
            "expected `{cmd}` in top-level help:\n{stdout}"
        );
    }
    assert!(
        !stdout.contains("internal-ccd"),
        "internal-ccd must be hidden from top-level --help:\n{stdout}"
    );
}

#[test]
fn subcommand_help_surfaces_required_verbs_and_args() {
    let setup_help = Command::new(zerovdd_bin())
        .args(["setup", "--help"])
        .output()
        .expect("run setup --help");
    assert!(setup_help.status.success());
    let setup_stdout = String::from_utf8_lossy(&setup_help.stdout);
    for sub in ["install", "upgrade", "uninstall", "status"] {
        assert!(setup_stdout.contains(sub), "missing `{sub}` in setup help");
    }

    let disp_help = Command::new(zerovdd_bin())
        .args(["display", "--help"])
        .output()
        .expect("run display --help");
    assert!(disp_help.status.success());
    let disp_stdout = String::from_utf8_lossy(&disp_help.stdout);
    for sub in ["add", "remove", "set-mode", "list", "switch-gpu", "restore"] {
        assert!(disp_stdout.contains(sub), "missing `{sub}` in display help");
    }

    let ccd_help = Command::new(zerovdd_bin())
        .args(["internal-ccd", "--help"])
        .output()
        .expect("run internal-ccd --help");
    assert!(ccd_help.status.success());
    let ccd_stdout = String::from_utf8_lossy(&ccd_help.stdout);
    for sub in ["snapshot", "apply", "set-mode", "restore"] {
        assert!(
            ccd_stdout.contains(sub),
            "missing `{sub}` in internal-ccd help"
        );
    }
}

#[test]
fn display_set_mode_validation_rejects_conflicting_flags_and_invalid_args() {
    let hdr_conflict = Command::new(zerovdd_bin())
        .args(["display", "set-mode", "1920x1080@60", "--hdr", "--sdr"])
        .output()
        .expect("run display set-mode --hdr --sdr");
    assert!(
        !hdr_conflict.status.success(),
        "expected conflicting --hdr and --sdr flags to fail"
    );

    let zero_hz = Command::new(zerovdd_bin())
        .args(["display", "set-mode", "1920x1080@0"])
        .output()
        .expect("run display set-mode with 0Hz");
    assert!(!zero_hz.status.success(), "expected 0Hz to be rejected");

    let zero_target = Command::new(zerovdd_bin())
        .args(["display", "set-mode", "1920x1080@60", "--target-id", "0"])
        .output()
        .expect("run display set-mode --target-id 0");
    assert!(
        !zero_target.status.success(),
        "expected --target-id 0 to be rejected"
    );

    let selector_conflict = Command::new(zerovdd_bin())
        .args([
            "display",
            "set-mode",
            "1920x1080@60",
            "--name",
            "alpha",
            "--target-id",
            "100",
        ])
        .output()
        .expect("run display set-mode with --name and --target-id");
    assert!(
        !selector_conflict.status.success(),
        "expected --name and --target-id conflict to fail"
    );

    let guid_selector_conflict = Command::new(zerovdd_bin())
        .args([
            "display",
            "set-mode",
            "1920x1080@60",
            "--guid",
            "11223344-5566-7788-99aa-bbccddeeff00",
            "--target-id",
            "100",
        ])
        .output()
        .expect("run display set-mode with --guid and --target-id");
    assert!(
        !guid_selector_conflict.status.success(),
        "expected --guid and --target-id conflict to fail"
    );

    let negative_luid_high = Command::new(zerovdd_bin())
        .args([
            "internal-ccd",
            "set-mode",
            "--luid-low",
            "1",
            "--luid-high",
            "-1",
            "--target-id",
            "1",
            "--width",
            "0",
            "--height",
            "1080",
            "--refresh-num",
            "60",
            "--refresh-den",
            "1",
        ])
        .output()
        .expect("run internal-ccd set-mode with negative luid-high");
    assert!(!negative_luid_high.status.success());
    let neg_stderr = String::from_utf8_lossy(&negative_luid_high.stderr);
    assert!(
        !neg_stderr.contains("unexpected argument '-1'"),
        "clap should accept negative --luid-high values, stderr was: {neg_stderr}"
    );
}

#[test]
fn cli_validation_rejects_missing_thumbprint_conflicting_flags_and_zero_hz() {
    let dev_cert_only = Command::new(zerovdd_bin())
        .args(["setup", "install", "--dev-cert", "dev.cer"])
        .output()
        .expect("run setup install --dev-cert without thumbprint");
    assert!(
        !dev_cert_only.status.success(),
        "expected --dev-cert without --expected-sha256 to fail"
    );

    let zero_hz = Command::new(zerovdd_bin())
        .args(["display", "add", "1920x1080@0", "--hold"])
        .output()
        .expect("run display add with 0Hz");
    assert!(!zero_hz.status.success(), "expected 0Hz to be rejected");

    let conflict_hold_lease = Command::new(zerovdd_bin())
        .args(["display", "add", "1920x1080@60", "--hold", "--lease", "30"])
        .output()
        .expect("run display add with conflicting --hold and --lease");
    assert!(
        !conflict_hold_lease.status.success(),
        "expected --hold and --lease conflict to fail"
    );

    let remove_missing_selector = Command::new(zerovdd_bin())
        .args(["display", "remove"])
        .output()
        .expect("run display remove without selector");
    assert!(
        !remove_missing_selector.status.success(),
        "expected display remove without --guid or --target-id to fail"
    );

    let standalone_no_lifetime = Command::new(zerovdd_bin())
        .env("ZEROVDD_NO_SERVICE_IPC", "1")
        .args(["display", "add", "3840x2160@59.94"])
        .output()
        .expect("run standalone display add without --hold or --lease");
    assert!(
        !standalone_no_lifetime.status.success(),
        "expected standalone display add without --hold or --lease to fail fast"
    );
    let stderr = String::from_utf8_lossy(&standalone_no_lifetime.stderr);
    assert!(
        stderr.contains("--hold") && stderr.contains("--lease"),
        "expected error mentioning --hold and --lease, got:\n{stderr}"
    );

    let multibyte_hz = Command::new(zerovdd_bin())
        .args(["display", "add", "1920x1080@60é", "--hold"])
        .output()
        .expect("run display add with multibyte UTF-8 Hz");
    assert!(
        !multibyte_hz.status.success(),
        "expected non-ASCII refresh rate to be rejected cleanly without panic"
    );

    let delegated_with_gpu = Command::new(zerovdd_bin())
        .env("ZEROVDD_NO_SERVICE_IPC", "1")
        .args(["display", "add", "1920x1080@60", "--gpu", "auto"])
        .output()
        .expect("run display add with --gpu and no --hold/--lease");
    assert!(
        !delegated_with_gpu.status.success(),
        "expected display add with --gpu without --hold/--lease to fail fast"
    );

    let delegated_with_guid = Command::new(zerovdd_bin())
        .env("ZEROVDD_NO_SERVICE_IPC", "1")
        .args([
            "display",
            "add",
            "1920x1080@60",
            "--guid",
            "11111111-2222-3333-4444-555555555555",
        ])
        .output()
        .expect("run display add with --guid and no --hold/--lease");
    assert!(
        !delegated_with_guid.status.success(),
        "expected display add with --guid without --hold/--lease to fail fast"
    );
}

#[test]
fn doctor_json_emits_valid_structured_report() {
    let out = Command::new(zerovdd_bin())
        .args(["doctor", "--json"])
        .output()
        .expect("run zerovdd doctor --json");
    assert!(
        out.status.success(),
        "zerovdd doctor --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8(out.stdout).expect("valid utf8 stdout");
    let parsed: Value = serde_json::from_str(&stdout).expect("doctor --json must emit valid JSON");
    for field in [
        "cli_version",
        "arch",
        "wudf_host_processes",
        "os",
        "gpus",
        "control_device",
        "active_displays",
        "service",
    ] {
        assert!(
            parsed.get(field).is_some(),
            "missing `{field}` in doctor JSON output:\n{stdout}"
        );
    }
}

#[test]
fn setup_install_fails_cleanly_when_explicit_inf_missing() {
    let dir = TempDir::new().unwrap();
    let missing_inf = dir.path().join("missing.inf");
    let out = Command::new(zerovdd_bin())
        .args(["setup", "install", "--inf"])
        .arg(&missing_inf)
        .output()
        .expect("run setup install with missing inf");
    assert!(!out.status.success());
}

#[test]
fn edid_inspect_valid_corrupt_and_oversized_files() {
    let dir = TempDir::new().unwrap();
    let p128 = dir.path().join("base128.edid");
    let p256 = dir.path().join("hdr256.edid");
    let pcorrupt = dir.path().join("corrupt.edid");
    let poversized = dir.path().join("oversized.edid");

    let b128 = zerovdd_edid::synthesize_block0(&zerovdd_edid::Block0Params {
        monitor_guid: [0x12; 16],
        client_name: "CliEdid128",
        preferred_width: 1920,
        preferred_height: 1080,
        preferred_refresh_millihz: 60_000,
        target_ppi: None,
        color_profile: zerovdd_edid::ColorProfile::SdrBt709,
        bit_depth_bpc: None,
        extension_count: 0,
    })
    .unwrap();
    let mut spec256 = zerovdd_edid::EdidSpec::new([0x34; 16], "CliHdr256", 3840, 2160, 120_000);
    spec256.color_profile = zerovdd_edid::ColorProfile::HdrDciP3;
    let b256 = zerovdd_edid::synthesize_edid(&spec256).unwrap();

    std::fs::write(&p128, b128).unwrap();
    std::fs::write(&p256, b256).unwrap();
    let mut bad = b128;
    bad[127] ^= 0x55;
    std::fs::write(&pcorrupt, bad).unwrap();
    std::fs::write(&poversized, vec![0u8; 640]).unwrap();

    for (path, expected_blocks) in [(&p128, 1usize), (&p256, 2usize)] {
        let out = Command::new(zerovdd_bin())
            .args(["edid", "inspect"])
            .arg(path)
            .arg("--json")
            .output()
            .expect("run edid inspect --json");
        assert!(
            out.status.success(),
            "edid inspect failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let parsed: Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
        assert_eq!(parsed["block_count"], expected_blocks);
        assert_eq!(parsed["checksum_valid"], true);
        assert_eq!(parsed["manufacturer"], "ZVD");
    }

    for bad_path in [&pcorrupt, &poversized] {
        let out = Command::new(zerovdd_bin())
            .args(["edid", "inspect"])
            .arg(bad_path)
            .output()
            .expect("run edid inspect on invalid file");
        assert!(
            !out.status.success(),
            "expected failure for {}",
            bad_path.display()
        );
    }
}

#[test]
fn display_add_help_surfaces_abi_edid_and_layout_flags() {
    let out = Command::new(zerovdd_bin())
        .args(["display", "add", "--help"])
        .output()
        .expect("run display add --help");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    for flag in [
        "--edid",
        "--color-space",
        "--bpc",
        "--ppi",
        "--max-nits",
        "--orientation",
        "--position",
        "--primary",
    ] {
        assert!(
            stdout.contains(flag),
            "missing `{flag}` in display add help"
        );
    }

    let neg_pos = Command::new(zerovdd_bin())
        .args([
            "display",
            "add",
            "1920x1080@60",
            "--orientation",
            "90",
            "--position",
            "badpos",
            "--primary",
            "--hold",
        ])
        .output()
        .expect("run display add with invalid position");
    assert!(!neg_pos.status.success());
    let stderr = String::from_utf8_lossy(&neg_pos.stderr);
    assert!(stderr.contains("invalid --position"));

    let neg_coords = Command::new(zerovdd_bin())
        .args([
            "display",
            "add",
            "1920x1080@60",
            "--orientation",
            "45",
            "--position",
            "-1080,0",
            "--hold",
        ])
        .output()
        .expect("run display add with negative position and bad orientation");
    assert!(!neg_coords.status.success());
    let neg_stderr = String::from_utf8_lossy(&neg_coords.stderr);
    assert!(
        neg_stderr.contains("invalid --orientation 45"),
        "expected negative --position -1080,0 to pass clap parsing: {neg_stderr}"
    );
}
