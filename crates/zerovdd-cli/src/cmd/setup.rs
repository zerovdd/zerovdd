//! Driver installation, upgrade, uninstall, and status subcommands (`zerovdd setup`).

#[cfg(any(windows, test))]
use std::path::Path;
use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::{Args, Subcommand};
#[cfg(any(windows, test))]
use zerovdd_installer::driver_store::DRIVER_INF_FILENAME;
use zerovdd_installer::{DevCertOption, InstallOptions, UninstallOptions};

/// Arguments for `zerovdd setup`.
#[derive(Debug, Args)]
pub struct SetupArgs {
    /// Setup action to execute.
    #[command(subcommand)]
    pub action: SetupCommand,
}

/// Supported driver setup subcommands.
#[derive(Debug, Subcommand)]
pub enum SetupCommand {
    /// Install the Universal zerovdd driver package and create a virtual adapter device node if absent.
    Install(SetupPackageArgs),
    /// Upgrade the installed zerovdd driver package without duplicating device nodes.
    Upgrade(SetupPackageArgs),
    /// Stop zerovdd-svc, remove virtual adapter device nodes, uninstall OEM INFs, and remove tracked dev certs.
    Uninstall {
        /// Output uninstall result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show Driver Store INF isolation status, PnP device nodes, and tracked dev certificates.
    Status {
        /// Output status report as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// Arguments shared by `setup install` and `setup upgrade`.
#[derive(Debug, Args)]
pub struct SetupPackageArgs {
    /// Path to `zerovdd.inf` (defaults to executable-relative or `dist/<arch>/zerovdd.inf`).
    #[arg(long, value_name = "PATH")]
    pub inf: Option<PathBuf>,
    /// Optional path to a development test-signing certificate (`.cer` / `.pem`) to import into LocalMachine stores.
    #[arg(long, value_name = "PATH", requires = "expected_sha256")]
    pub dev_cert: Option<PathBuf>,
    /// Expected 64-character SHA-256 hex thumbprint required when `--dev-cert` is supplied.
    #[arg(long, value_name = "HEX", requires = "dev_cert")]
    pub expected_sha256: Option<String>,
    /// Output install/upgrade result as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Returns the architecture subdirectory name under `dist/` for the compiled binary target.
#[cfg(any(windows, test))]
pub const fn target_dist_arch_dir() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "ARM64"
    } else {
        "x64"
    }
}

/// Resolves `--inf <path>` or falls back to executable-relative and `dist/<arch>/zerovdd.inf` locations.
#[cfg(any(windows, test))]
pub fn resolve_inf_path_from(
    explicit: Option<&Path>,
    exe_path: Option<&Path>,
    cwd: Option<&Path>,
) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        bail!("driver INF file not found at {}", path.display());
    }

    let arch = target_dist_arch_dir();
    let mut candidates = Vec::new();
    if let Some(exe_dir) = exe_path.and_then(Path::parent) {
        candidates.push(exe_dir.join(DRIVER_INF_FILENAME));
        candidates.push(exe_dir.join("driver").join(DRIVER_INF_FILENAME));
        candidates.push(exe_dir.join("..").join("driver").join(DRIVER_INF_FILENAME));
        candidates.push(
            exe_dir
                .join("..")
                .join("dist")
                .join(arch)
                .join(DRIVER_INF_FILENAME),
        );
        candidates.push(
            exe_dir
                .join("..")
                .join("..")
                .join("dist")
                .join(arch)
                .join(DRIVER_INF_FILENAME),
        );
    }
    if let Some(dir) = cwd {
        candidates.push(dir.join("dist").join(arch).join(DRIVER_INF_FILENAME));
        candidates.push(dir.join(DRIVER_INF_FILENAME));
    }

    for candidate in &candidates {
        if candidate.is_file() {
            return Ok(candidate.clone());
        }
    }

    let searched = candidates
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    bail!("could not locate {DRIVER_INF_FILENAME}; pass --inf <path> (searched: {searched})");
}

fn build_install_options(args: &SetupPackageArgs) -> Result<InstallOptions> {
    let dev_cert = match (&args.dev_cert, &args.expected_sha256) {
        (Some(cert_path), Some(expected_sha256)) => Some(DevCertOption {
            cert_path: cert_path.clone(),
            expected_sha256: expected_sha256.clone(),
        }),
        (None, None) => None,
        _ => bail!("both --dev-cert and --expected-sha256 must be provided together"),
    };
    Ok(InstallOptions {
        dev_cert,
        state_path: None,
    })
}

/// Executes `zerovdd setup`.
pub fn execute(args: SetupArgs) -> Result<()> {
    match args.action {
        SetupCommand::Install(pkg) => run_install(pkg),
        SetupCommand::Upgrade(pkg) => run_upgrade(pkg),
        SetupCommand::Uninstall { json } => run_uninstall(json),
        SetupCommand::Status { json } => run_status(json),
    }
}

#[cfg(windows)]
fn run_install(pkg: SetupPackageArgs) -> Result<()> {
    let exe = std::env::current_exe().ok();
    let cwd = std::env::current_dir().ok();
    let inf_path = resolve_inf_path_from(pkg.inf.as_deref(), exe.as_deref(), cwd.as_deref())?;
    let opts = build_install_options(&pkg)?;
    let outcome = zerovdd_installer::install(&inf_path, &opts)?;
    print_install_outcome("install", &outcome, pkg.json)
}

#[cfg(not(windows))]
fn run_install(pkg: SetupPackageArgs) -> Result<()> {
    let _ = build_install_options(&pkg)?;
    bail!("zerovdd setup install is only supported on Windows")
}

#[cfg(windows)]
fn run_upgrade(pkg: SetupPackageArgs) -> Result<()> {
    let exe = std::env::current_exe().ok();
    let cwd = std::env::current_dir().ok();
    let inf_path = resolve_inf_path_from(pkg.inf.as_deref(), exe.as_deref(), cwd.as_deref())?;
    let opts = build_install_options(&pkg)?;
    let outcome = zerovdd_installer::upgrade(&inf_path, &opts)?;
    print_install_outcome("upgrade", &outcome, pkg.json)
}

#[cfg(not(windows))]
fn run_upgrade(pkg: SetupPackageArgs) -> Result<()> {
    let _ = build_install_options(&pkg)?;
    bail!("zerovdd setup upgrade is only supported on Windows")
}

#[cfg(windows)]
fn run_uninstall(json: bool) -> Result<()> {
    let outcome = zerovdd_installer::uninstall(&UninstallOptions::default())?;
    if json {
        println!("{}", serde_json::to_string_pretty(&outcome)?);
    } else {
        println!("Driver uninstall completed.");
        println!("  Service stop: {:?}", outcome.service_stop);
        println!("  Removed device nodes: {:?}", outcome.removed_device_nodes);
        println!("  Removed OEM INFs: {:?}", outcome.removed_oem_infs);
        println!(
            "  Removed tracked dev certs: {}",
            outcome.removed_dev_cert_count
        );
        if outcome.reboot_required {
            println!("  WARNING: Windows reported a reboot is required to complete removal.");
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn run_uninstall(_json: bool) -> Result<()> {
    let _ = UninstallOptions::default();
    bail!("zerovdd setup uninstall is only supported on Windows")
}

#[cfg(windows)]
fn run_status(json: bool) -> Result<()> {
    let st = zerovdd_installer::status()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&st)?);
    } else {
        println!("Driver installed: {}", st.driver.installed);
        if let Some(ref oem) = st.driver.oem_inf_name {
            println!("  OEM INF: {oem}");
        }
        if let Some(ref store_path) = st.driver.driver_store_path {
            println!("  Driver Store INF: {}", store_path.display());
        }
        println!("  DIRID 13 isolated: {}", st.driver.dirid_13_isolated);
        if let Some(ref ver) = st.driver.driver_version {
            println!("  Driver version: {ver}");
        }
        if let Some(ref signer) = st.driver.signer_name {
            println!("  Signer: {signer}");
        }
        println!("  Matching Root\\ZeroVdd nodes: {}", st.device_nodes.len());
        for node in &st.device_nodes {
            println!(
                "    - {} (present={}, problem_code={})",
                node.instance_id, node.present, node.problem_code
            );
        }
        println!("  Tracked dev certs: {}", st.tracked_dev_certs.len());
    }
    Ok(())
}

#[cfg(not(windows))]
fn run_status(_json: bool) -> Result<()> {
    bail!("zerovdd setup status is only supported on Windows")
}

#[cfg(windows)]
fn print_install_outcome(
    verb: &str,
    outcome: &zerovdd_installer::InstallOutcome,
    json: bool,
) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(outcome)?);
        return Ok(());
    }
    println!("Driver {verb} succeeded.");
    println!("  Created device node: {}", outcome.created_device_node);
    println!("  Device instance IDs: {:?}", outcome.device_instance_ids);
    if let Some(ref oem) = outcome.oem_inf_name {
        println!("  OEM INF: {oem}");
    }
    if let Some(ref store) = outcome.driver_store_path {
        println!("  Driver Store path: {}", store.display());
    }
    if let Some(ref cert) = outcome.installed_dev_cert_sha256 {
        println!("  Installed dev cert SHA-256: {cert}");
    }
    println!("  Service stop status: {:?}", outcome.service_stop);
    if outcome.reboot_required {
        println!("  WARNING: Windows reported a reboot is required to finish driver {verb}.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn resolves_default_inf_relative_to_exe_or_dist_dir() {
        let dir = TempDir::new().unwrap();
        let arch_dir = dir.path().join("dist").join(target_dist_arch_dir());
        fs::create_dir_all(&arch_dir).unwrap();
        let inf = arch_dir.join(DRIVER_INF_FILENAME);
        fs::write(&inf, "[Version]\n").unwrap();

        let resolved = resolve_inf_path_from(None, None, Some(dir.path())).unwrap();
        assert_eq!(resolved, inf);

        let exe_dir = dir.path().join("bin");
        fs::create_dir_all(&exe_dir).unwrap();
        let exe_inf = exe_dir.join(DRIVER_INF_FILENAME);
        fs::write(&exe_inf, "[Version]\n").unwrap();
        let exe_path = exe_dir.join("zerovdd.exe");
        let resolved_exe = resolve_inf_path_from(None, Some(&exe_path), None).unwrap();
        assert_eq!(resolved_exe, exe_inf);

        assert!(resolve_inf_path_from(Some(&dir.path().join("nope.inf")), None, None).is_err());
    }
}
