//! Native Windows SetupAPI / `newdev` driver package installer, device-node manager, and pinned dev-cert helper.

#![warn(missing_docs)]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Pure-Rust SHA-256 thumbprint verification, install-state tracking, and Windows cert store helpers.
pub mod cert;
/// Virtual root device-node (`Root\ZeroVdd`) enumeration, creation, removal, and service stop helpers.
pub mod device_node;
/// Windows Driver Store (`oemXX.inf` / `DIRID 13`) staging, inspection, and removal helpers.
pub mod driver_store;

use cert::{
    default_install_state_path, load_and_verify_cert, load_install_state, record_installed_cert,
    save_install_state_atomic, CertStoreLocation, InstalledCertRecord, VerifiedDevCert,
};
use device_node::{DeviceNodeInfo, ServiceStopOutcome, SERVICE_NAME};
use driver_store::{validate_inf_path, DriverPackageStatus, DriverStageResult};

/// Errors returned by `zerovdd-installer` operations.
#[derive(Debug, Error)]
pub enum InstallerError {
    /// Driver INF file was not found at the provided path.
    #[error("driver INF file not found: {0}")]
    InfNotFound(PathBuf),

    /// Expected SHA-256 thumbprint string could not be parsed.
    #[error("invalid expected SHA-256 thumbprint: {0}")]
    InvalidThumbprint(String),

    /// Certificate SHA-256 digest did not match the pinned expected thumbprint.
    #[error(
        "development certificate SHA-256 thumbprint mismatch: expected {expected}, actual {actual}"
    )]
    ThumbprintMismatch {
        /// Expected normalized SHA-256 hex digest.
        expected: String,
        /// Actual SHA-256 hex digest computed from the certificate file.
        actual: String,
    },

    /// Certificate file could not be decoded as valid DER or PEM X.509 data.
    #[error("invalid X.509 certificate: {0}")]
    InvalidCertificate(String),

    /// Persisted install-state JSON is corrupt or uses an unsupported schema version.
    #[error("corrupt install-state metadata: {0}")]
    CorruptInstallState(String),

    /// Caller passed an invalid argument.
    #[error("invalid installer argument: {0}")]
    InvalidInput(String),

    /// `zerovdd-svc` did not transition to `SERVICE_STOPPED` within the timeout.
    #[error("timed out waiting for Windows service `{0}` to stop")]
    ServiceStopTimeout(String),

    /// Filesystem I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Install-state JSON codec error.
    #[error("install-state JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Win32 SetupAPI, `newdev`, CryptoAPI, or SCM call failed.
    #[error("{operation} failed with Win32 error {code:#010x}")]
    Win32 {
        /// Failing Win32 function name.
        operation: &'static str,
        /// Returned Win32/HRESULT error code.
        code: u32,
    },
}

/// Pinned development certificate path and expected SHA-256 thumbprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevCertOption {
    /// Path to a DER (`.cer`) or PEM X.509 certificate file.
    pub cert_path: PathBuf,
    /// Expected 64-character SHA-256 hexadecimal thumbprint.
    pub expected_sha256: String,
}

/// Options controlling `install` and `upgrade`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InstallOptions {
    /// Optional explicit development certificate + pinned SHA-256 thumbprint.
    pub dev_cert: Option<DevCertOption>,
    /// Optional override for `install-state.json` path (defaults to `%ProgramData%\zerovdd\install-state.json`).
    pub state_path: Option<PathBuf>,
}

/// Options controlling `uninstall`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UninstallOptions {
    /// Optional override for `install-state.json` path (defaults to `%ProgramData%\zerovdd\install-state.json`).
    pub state_path: Option<PathBuf>,
}

/// Outcome returned by [`install_with_backend`] or [`upgrade_with_backend`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallOutcome {
    /// `true` if a new `Root\ZeroVdd` device node was created; `false` if an existing node was reused.
    pub created_device_node: bool,
    /// Active `Root\ZeroVdd` PnP device instance IDs present after the operation.
    pub device_instance_ids: Vec<String>,
    /// Published OEM INF filename assigned by Windows, if discovered.
    pub oem_inf_name: Option<String>,
    /// Staged `DriverStore\FileRepository` INF path, if discovered.
    pub driver_store_path: Option<PathBuf>,
    /// SHA-256 thumbprint of the dev cert installed and recorded, if any.
    pub installed_dev_cert_sha256: Option<String>,
    /// Result of stopping `zerovdd-svc` prior to driver staging.
    pub service_stop: ServiceStopOutcome,
    /// `true` if Windows indicated a system reboot is required (`DI_NEEDREBOOT` / `ERROR_SUCCESS_REBOOT_REQUIRED`).
    pub reboot_required: bool,
}

/// Outcome returned by [`uninstall_with_backend`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UninstallOutcome {
    /// Instance IDs of `Root\ZeroVdd` PnP device nodes uninstalled.
    pub removed_device_nodes: Vec<String>,
    /// Published OEM INF filenames removed from Driver Store.
    pub removed_oem_infs: Vec<String>,
    /// Number of certificate entries removed from LocalMachine stores.
    pub removed_dev_cert_count: usize,
    /// Result of stopping `zerovdd-svc` prior to driver removal.
    pub service_stop: ServiceStopOutcome,
    /// `true` if Windows indicated a system reboot is required.
    pub reboot_required: bool,
}

/// Combined Driver Store, PnP device node, and tracked dev-certificate status.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct InstallerStatus {
    /// Driver Store INF isolation status and metadata.
    pub driver: DriverPackageStatus,
    /// Matching `Root\ZeroVdd` PnP device nodes.
    pub device_nodes: Vec<DeviceNodeInfo>,
    /// Development certificates tracked in `install-state.json` for uninstall cleanup.
    pub tracked_dev_certs: Vec<InstalledCertRecord>,
}

/// Testable abstraction over Windows SetupAPI, `newdev`, CryptoAPI, and SCM operations.
pub trait InstallerBackend {
    /// Gracefully stops `service_name` if installed and running.
    fn stop_service_if_running(
        &self,
        service_name: &str,
    ) -> Result<ServiceStopOutcome, InstallerError>;

    /// Imports a pre-verified development certificate into LocalMachine stores.
    fn install_dev_cert(
        &self,
        cert: &VerifiedDevCert,
    ) -> Result<Vec<CertStoreLocation>, InstallerError>;

    /// Removes only certificates matching `sha256_hex` from `stores`.
    fn remove_dev_cert(
        &self,
        sha256_hex: &str,
        stores: &[CertStoreLocation],
    ) -> Result<usize, InstallerError>;

    /// Enumerates all existing `Root\ZeroVdd` PnP device nodes.
    fn enumerate_device_nodes(&self) -> Result<Vec<DeviceNodeInfo>, InstallerError>;

    /// Stages and installs `inf_path` into the Windows Driver Store (`DiInstallDriverW`).
    fn stage_and_install_inf(
        &self,
        inf_path: &Path,
        force_reinstall: bool,
    ) -> Result<DriverStageResult, InstallerError>;

    /// Creates a single `Root\ZeroVdd` device node and binds the staged driver (`DiInstallDevice`).
    fn create_device_node(&self) -> Result<(String, bool), InstallerError>;

    /// Uninstalls all matching `Root\ZeroVdd` PnP device nodes (`DiUninstallDevice`).
    fn uninstall_device_nodes(&self) -> Result<(Vec<String>, bool), InstallerError>;

    /// Removes all matching `zerovdd` OEM INFs from the Windows Driver Store (`SetupUninstallOEMInfW`).
    fn uninstall_oem_infs(&self) -> Result<(Vec<String>, bool), InstallerError>;

    /// Queries current Driver Store INF isolation status and metadata.
    fn query_driver_status(&self) -> Result<DriverPackageStatus, InstallerError>;
}

/// Live Windows SetupAPI / `newdev` / CryptoAPI / SCM implementation of [`InstallerBackend`].
#[cfg(windows)]
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsInstallerBackend;

#[cfg(windows)]
impl WindowsInstallerBackend {
    /// Creates a new [`WindowsInstallerBackend`].
    pub const fn new() -> Self {
        Self
    }
}

#[cfg(windows)]
impl InstallerBackend for WindowsInstallerBackend {
    fn stop_service_if_running(
        &self,
        service_name: &str,
    ) -> Result<ServiceStopOutcome, InstallerError> {
        device_node::stop_service_if_running(service_name)
    }

    fn install_dev_cert(
        &self,
        cert: &VerifiedDevCert,
    ) -> Result<Vec<CertStoreLocation>, InstallerError> {
        cert::install_cert_to_local_machine_stores(cert)
    }

    fn remove_dev_cert(
        &self,
        sha256_hex: &str,
        stores: &[CertStoreLocation],
    ) -> Result<usize, InstallerError> {
        cert::remove_cert_from_local_machine_stores(sha256_hex, stores)
    }

    fn enumerate_device_nodes(&self) -> Result<Vec<DeviceNodeInfo>, InstallerError> {
        device_node::enumerate_zerovdd_nodes()
    }

    fn stage_and_install_inf(
        &self,
        inf_path: &Path,
        force_reinstall: bool,
    ) -> Result<DriverStageResult, InstallerError> {
        driver_store::stage_and_install_driver_inf(inf_path, force_reinstall)
    }

    fn create_device_node(&self) -> Result<(String, bool), InstallerError> {
        device_node::create_root_device_node()
    }

    fn uninstall_device_nodes(&self) -> Result<(Vec<String>, bool), InstallerError> {
        device_node::uninstall_all_zerovdd_nodes()
    }

    fn uninstall_oem_infs(&self) -> Result<(Vec<String>, bool), InstallerError> {
        driver_store::uninstall_matching_oem_infs()
    }

    fn query_driver_status(&self) -> Result<DriverPackageStatus, InstallerError> {
        driver_store::query_driver_package_status()
    }
}

fn run_install_or_upgrade<B: InstallerBackend>(
    backend: &B,
    inf_path: &Path,
    opts: &InstallOptions,
    force_reinstall: bool,
) -> Result<InstallOutcome, InstallerError> {
    let canonical_inf = validate_inf_path(inf_path)?;

    // WHY: Verify the pinned SHA-256 thumbprint in pure Rust BEFORE stopping services,
    // mutating Windows cert stores, or staging any driver into the Driver Store.
    let verified_cert = opts
        .dev_cert
        .as_ref()
        .map(|dc| load_and_verify_cert(&dc.cert_path, &dc.expected_sha256))
        .transpose()?;

    let state_path = opts
        .state_path
        .clone()
        .unwrap_or_else(default_install_state_path);
    let mut installed_dev_cert_sha256 = None;
    if let Some(ref verified) = verified_cert {
        let stores = backend.install_dev_cert(verified)?;
        let mut state = load_install_state(&state_path)?;
        record_installed_cert(&mut state, &verified.sha256_hex, &stores);
        save_install_state_atomic(&state_path, &state)?;
        installed_dev_cert_sha256 = Some(verified.sha256_hex.clone());
    }

    let service_stop = backend.stop_service_if_running(SERVICE_NAME)?;
    let existing_nodes = backend.enumerate_device_nodes()?;
    let stage = backend.stage_and_install_inf(&canonical_inf, force_reinstall)?;

    let mut reboot_required = stage.reboot_required;
    let (created_device_node, device_instance_ids) = if existing_nodes.is_empty() {
        let (instance_id, node_reboot) = backend.create_device_node()?;
        reboot_required |= node_reboot;
        (true, vec![instance_id])
    } else {
        // WHY: Reusing existing Root\ZeroVdd nodes prevents duplicate virtual display adapters
        // from accumulating across driver upgrades or repeated install invocations.
        (
            false,
            existing_nodes
                .into_iter()
                .map(|node| node.instance_id)
                .collect(),
        )
    };

    Ok(InstallOutcome {
        created_device_node,
        device_instance_ids,
        oem_inf_name: stage.oem_inf_name,
        driver_store_path: stage.driver_store_path,
        installed_dev_cert_sha256,
        service_stop,
        reboot_required,
    })
}

/// Idempotently installs the driver INF and ensures a single `Root\ZeroVdd` device node exists.
pub fn install_with_backend<B: InstallerBackend>(
    backend: &B,
    inf_path: &Path,
    opts: &InstallOptions,
) -> Result<InstallOutcome, InstallerError> {
    run_install_or_upgrade(backend, inf_path, opts, false)
}

/// Force-reinstalls/upgrades the driver INF on existing `Root\ZeroVdd` node(s) without duplicating nodes.
pub fn upgrade_with_backend<B: InstallerBackend>(
    backend: &B,
    inf_path: &Path,
    opts: &InstallOptions,
) -> Result<InstallOutcome, InstallerError> {
    run_install_or_upgrade(backend, inf_path, opts, true)
}

/// Stops `zerovdd-svc`, removes all `Root\ZeroVdd` device nodes, deletes OEM INFs, and removes tracked dev certs.
pub fn uninstall_with_backend<B: InstallerBackend>(
    backend: &B,
    opts: &UninstallOptions,
) -> Result<UninstallOutcome, InstallerError> {
    let service_stop = backend.stop_service_if_running(SERVICE_NAME)?;
    let (removed_device_nodes, nodes_reboot) = backend.uninstall_device_nodes()?;
    let (removed_oem_infs, infs_reboot) = backend.uninstall_oem_infs()?;

    let state_path = opts
        .state_path
        .clone()
        .unwrap_or_else(default_install_state_path);
    let mut state = load_install_state(&state_path)?;
    let mut removed_dev_cert_count = 0usize;
    for rec in &state.installed_dev_certs {
        removed_dev_cert_count += backend.remove_dev_cert(&rec.sha256, &rec.stores)?;
    }
    state.installed_dev_certs.clear();
    save_install_state_atomic(&state_path, &state)?;

    Ok(UninstallOutcome {
        removed_device_nodes,
        removed_oem_infs,
        removed_dev_cert_count,
        service_stop,
        reboot_required: nodes_reboot || infs_reboot,
    })
}

/// Queries current Driver Store, PnP device node, and tracked dev-certificate status via `backend`.
pub fn status_with_backend<B: InstallerBackend>(
    backend: &B,
    state_path: Option<&Path>,
) -> Result<InstallerStatus, InstallerError> {
    let driver = backend.query_driver_status()?;
    let device_nodes = backend.enumerate_device_nodes()?;
    let resolved_state = state_path
        .map(PathBuf::from)
        .unwrap_or_else(default_install_state_path);
    let state = load_install_state(&resolved_state)?;

    Ok(InstallerStatus {
        driver,
        device_nodes,
        tracked_dev_certs: state.installed_dev_certs,
    })
}

/// Idempotently installs `inf_path` onto the local Windows machine.
#[cfg(windows)]
pub fn install(inf_path: &Path, opts: &InstallOptions) -> Result<InstallOutcome, InstallerError> {
    install_with_backend(&WindowsInstallerBackend::new(), inf_path, opts)
}

/// Upgrades the driver package from `inf_path` on the local Windows machine without duplicating nodes.
#[cfg(windows)]
pub fn upgrade(inf_path: &Path, opts: &InstallOptions) -> Result<InstallOutcome, InstallerError> {
    upgrade_with_backend(&WindowsInstallerBackend::new(), inf_path, opts)
}

/// Uninstalls the driver, `Root\ZeroVdd` device nodes, and installer-tracked dev certificates.
#[cfg(windows)]
pub fn uninstall(opts: &UninstallOptions) -> Result<UninstallOutcome, InstallerError> {
    uninstall_with_backend(&WindowsInstallerBackend::new(), opts)
}

/// Queries live Windows Driver Store, `Root\ZeroVdd` device node, and tracked dev-certificate status.
#[cfg(windows)]
pub fn status() -> Result<InstallerStatus, InstallerError> {
    status_with_backend(&WindowsInstallerBackend::new(), None)
}
