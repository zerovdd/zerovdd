//! Sunshine streaming session state machine, overlap backup preservation, and crash guardian.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use thiserror::Error;
use uuid::Uuid;
use zerovdd_client::device::ControlClient;
use zerovdd_client::gpu::{parse_gpu_selector, select_gpu, GpuAdapterInfo, GpuSelector};
use zerovdd_client::topology::{default_snapshot_path, AdapterLuid, DisplayRotation};
use zerovdd_client::{monitor_guid_for_name, ClientError};
use zerovdd_config::{ConfigV1, SunshineTopologyMode};
use zerovdd_proto::ipc::{
    ActiveDisplayInfo, DisplayAddRequest, DisplayOrientation, DisplayRemoveRequest,
    DisplaySetModeRequest, GpuAdapterSummary, IpcError, IpcErrorPayload, IpcRequest, IpcResponse,
    ServiceHealth, SessionStatus, StartSessionRequest, StopSessionRequest, SwitchGpuRequest,
    TopologyMode,
};
use zerovdd_proto::types::{
    AddDisplayRequest, BitDepthPreference, ColorSpacePreference, HdrMasteringMetadata, HdrProfile,
    OwnershipMode, RemoveDisplayRequest, SetRenderAdapterRequest, UpdateDisplayModeRequest,
};
use zerovdd_proto::validation::ProtoError;

use crate::console_launcher::{
    ApplyCcdPayload, ConsoleCcdRunner, LauncherError, SetModeCcdPayload,
};
use crate::persistent::{
    apply_configured_gpu_preference, depart_headless_standby, hdr_metadata_from_defaults,
    hdr_metadata_with_overrides, load_and_validate_edid_file, millihz_to_rational,
    reconcile_headless_standby, reconcile_persistent_displays, PersistentError,
};
use crate::pipe::IpcHandler;

/// Errors produced by session orchestration, process supervision, or driver control calls.
#[derive(Debug, Error)]
pub enum SessionError {
    /// IPC payload validation failed.
    #[error("invalid IPC request: {0}")]
    InvalidRequest(#[from] IpcError),

    /// Protocol wire payload validation failed.
    #[error("invalid driver payload: {0}")]
    Proto(#[from] ProtoError),

    /// Control Device IOCTL failed.
    #[error("driver error: {0}")]
    Client(#[from] ClientError),

    /// Console-session CCD trampoline failed.
    #[error("console CCD launcher error: {0}")]
    Launcher(#[from] LauncherError),

    /// Persistent or standby display reconciliation failed.
    #[error("persistent display error: {0}")]
    Persistent(#[from] PersistentError),

    /// Caller attempted to stop a session owned by a different `client_id` or `owner_pid` without `--force`.
    #[error("active session belongs to a different owner (pass force=true to override)")]
    SessionOwnerMismatch,

    /// Owning Sunshine process already exited before watch registration.
    #[error("owning process {0} is no longer running")]
    OwnerProcessUnavailable(u32),

    /// Owning PID exists but its creation timestamp does not match (PID reused).
    #[error("owning PID {pid} creation time mismatch (expected {expected}, got {actual})")]
    OwnerProcessCreationMismatch {
        /// OS process identifier.
        pid: u32,
        /// Expected FILETIME creation timestamp.
        expected: u64,
        /// Actual observed FILETIME creation timestamp.
        actual: u64,
    },

    /// Custom EDID binary failed structural or checksum validation.
    #[error("invalid custom EDID payload: {0}")]
    InvalidEdid(String),

    /// Underlying Win32 process supervision error.
    #[error("{operation} failed with Win32 error {code:#010x}")]
    Win32 {
        /// Win32 function name.
        operation: &'static str,
        /// Win32 error code.
        code: u32,
    },
}

impl SessionError {
    /// Returns a stable machine-readable error code for IPC responses.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) => "invalid_request",
            Self::Proto(_) => "invalid_driver_payload",
            Self::Client(ClientError::AccessDenied) => "driver_access_denied",
            Self::Client(ClientError::DeviceNotFound(_)) => "driver_not_found",
            Self::Client(ClientError::MonitorNotFound) => "monitor_not_found",
            Self::Client(_) => "driver_error",
            Self::Launcher(LauncherError::ConsoleSessionUnavailable) => {
                "console_session_unavailable"
            }
            Self::Launcher(_) => "console_launcher_error",
            Self::Persistent(_) => "persistent_display_error",
            Self::SessionOwnerMismatch => "session_owner_mismatch",
            Self::OwnerProcessUnavailable(_) => "owner_process_unavailable",
            Self::OwnerProcessCreationMismatch { .. } => "owner_process_mismatch",
            Self::InvalidEdid(_) => "invalid_edid",
            Self::Win32 { .. } => "win32_error",
        }
    }

    /// Converts this error into an [`IpcErrorPayload`].
    pub fn to_ipc_payload(&self) -> IpcErrorPayload {
        IpcErrorPayload {
            code: self.code().to_string(),
            message: self.to_string(),
        }
    }
}

/// Console session lifecycle events forwarded from Windows `SERVICE_CONTROL_SESSIONCHANGE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleSessionEvent {
    /// Physical console session connected (`WTS_CONSOLE_CONNECT`).
    ConsoleConnect,
    /// Interactive user logged on (`WTS_SESSION_LOGON`).
    SessionLogon,
    /// Interactive session unlocked (`WTS_SESSION_UNLOCK`).
    SessionUnlock,
    /// Any other session notification not requiring recovery action.
    Other,
}

impl ConsoleSessionEvent {
    /// Returns `true` when this event indicates an interactive/console desktop may now be ready.
    pub const fn should_trigger_pending_restore(self) -> bool {
        matches!(
            self,
            Self::ConsoleConnect | Self::SessionLogon | Self::SessionUnlock
        )
    }
}

/// Handle returned by [`ProcessWatcher::watch_process`] to cancel monitoring on clean teardown.
pub trait WatchHandle: Send + Sync {
    /// Cancels process exit supervision without firing the registered callback.
    fn cancel(&self);
}

/// Abstraction for supervising an owning process (`sunshine.exe`) until unexpected exit.
pub trait ProcessWatcher: Send + Sync {
    /// Validates `(pid, expected_creation_time)` and invokes `on_exit` once if the process exits
    /// before the returned [`WatchHandle`] is canceled.
    fn watch_process(
        &self,
        pid: u32,
        expected_creation_time: u64,
        on_exit: Box<dyn FnOnce() + Send>,
    ) -> Result<Box<dyn WatchHandle>, SessionError>;
}

/// Provider that maintains and reconnects the service's privileged Control Device handle.
pub trait DriverProvider: Send + Sync {
    /// Returns an open [`ControlClient`] handle, connecting lazily if needed.
    fn get_or_connect(&self) -> Result<Arc<dyn ControlClient + Send + Sync>, SessionError>;

    /// Returns the currently cached [`ControlClient`] handle if already connected.
    fn connected_client(&self) -> Option<Arc<dyn ControlClient + Send + Sync>>;

    /// Drops the cached Control Device handle so the next operation reconnects cleanly.
    fn reset(&self);
}

/// Abstraction for enumerating DXGI adapters inside `zerovdd-svc`.
pub trait GpuEnumerator: Send + Sync {
    /// Returns all DXGI graphics adapters visible to the service.
    fn enumerate_adapters(&self) -> Result<Vec<GpuAdapterInfo>, ClientError>;
}

/// Production [`GpuEnumerator`] delegating to [`zerovdd_client::gpu::enumerate_dxgi_adapters`].
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultGpuEnumerator;

impl GpuEnumerator for DefaultGpuEnumerator {
    fn enumerate_adapters(&self) -> Result<Vec<GpuAdapterInfo>, ClientError> {
        #[cfg(windows)]
        {
            zerovdd_client::gpu::enumerate_dxgi_adapters()
        }
        #[cfg(not(windows))]
        {
            Err(ClientError::GpuNotFound {
                selector: "DXGI enumeration is only supported on Windows".to_string(),
            })
        }
    }
}

/// Static [`DriverProvider`] wrapper around an existing [`ControlClient`] implementation (for tests).
pub struct StaticDriverProvider {
    client: Arc<dyn ControlClient + Send + Sync>,
}

impl StaticDriverProvider {
    /// Wraps `client` in a [`DriverProvider`].
    pub fn new(client: Arc<dyn ControlClient + Send + Sync>) -> Self {
        Self { client }
    }
}

impl DriverProvider for StaticDriverProvider {
    fn get_or_connect(&self) -> Result<Arc<dyn ControlClient + Send + Sync>, SessionError> {
        Ok(Arc::clone(&self.client))
    }

    fn connected_client(&self) -> Option<Arc<dyn ControlClient + Send + Sync>> {
        Some(Arc::clone(&self.client))
    }

    fn reset(&self) {}
}

#[cfg(windows)]
mod win32 {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        CreateEventW, GetProcessTimes, OpenProcess, SetEvent, WaitForMultipleObjects, INFINITE,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };
    use zerovdd_client::device::{ControlClient, ControlDevice};

    use super::{DriverProvider, ProcessWatcher, SessionError, WatchHandle};

    /// Production Windows [`DriverProvider`] holding an open `ControlDevice` handle.
    #[derive(Default)]
    pub struct WindowsDriverProvider {
        cached: Mutex<Option<Arc<dyn ControlClient + Send + Sync>>>,
    }

    impl WindowsDriverProvider {
        /// Creates an initially unconnected [`WindowsDriverProvider`].
        pub const fn new() -> Self {
            Self {
                cached: Mutex::new(None),
            }
        }
    }

    impl DriverProvider for WindowsDriverProvider {
        fn get_or_connect(&self) -> Result<Arc<dyn ControlClient + Send + Sync>, SessionError> {
            let mut guard = self
                .cached
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(ref client) = *guard {
                return Ok(Arc::clone(client));
            }
            let dev = ControlDevice::open()?;
            let arc: Arc<dyn ControlClient + Send + Sync> = Arc::new(dev);
            *guard = Some(Arc::clone(&arc));
            Ok(arc)
        }

        fn connected_client(&self) -> Option<Arc<dyn ControlClient + Send + Sync>> {
            self.cached
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .map(Arc::clone)
        }

        fn reset(&self) {
            let mut guard = self
                .cached
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *guard = None;
        }
    }

    struct OwnedHandle(HANDLE);

    // SAFETY: Win32 process/event handles owned by `OwnedHandle` may transfer across threads.
    unsafe impl Send for OwnedHandle {}
    // SAFETY: Win32 process/event kernel handles are safe to share across threads for `SetEvent`.
    unsafe impl Sync for OwnedHandle {}

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_invalid() && self.0 != HANDLE::default() {
                // SAFETY: `self.0` was opened via `OpenProcess` or `CreateEventW` and is closed once.
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    struct WindowsWatchHandle {
        cancel_event: Arc<OwnedHandle>,
        canceled: Arc<AtomicBool>,
    }

    impl WatchHandle for WindowsWatchHandle {
        fn cancel(&self) {
            if !self.canceled.swap(true, Ordering::AcqRel) {
                // SAFETY: `self.cancel_event` is kept alive by `Arc<OwnedHandle>` until all clones drop.
                // Never join the watcher thread here because callers hold `op_mutex`.
                unsafe {
                    let _ = SetEvent(self.cancel_event.0);
                }
            }
        }
    }

    impl Drop for WindowsWatchHandle {
        fn drop(&mut self) {
            self.cancel();
        }
    }

    /// Production Windows [`ProcessWatcher`] using `OpenProcess`, `GetProcessTimes`, and `WaitForMultipleObjects`.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct WindowsProcessWatcher;

    impl ProcessWatcher for WindowsProcessWatcher {
        fn watch_process(
            &self,
            pid: u32,
            expected_creation_time: u64,
            on_exit: Box<dyn FnOnce() + Send>,
        ) -> Result<Box<dyn WatchHandle>, SessionError> {
            // SAFETY: Opens `pid` for synchronization and creation-time verification only.
            let proc_raw = unsafe {
                OpenProcess(
                    PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                    false,
                    pid,
                )
            }
            .map_err(|_| SessionError::OwnerProcessUnavailable(pid))?;
            let proc_handle = OwnedHandle(proc_raw);

            let mut creation = FILETIME::default();
            let mut exit = FILETIME::default();
            let mut kernel = FILETIME::default();
            let mut user = FILETIME::default();
            // SAFETY: `proc_handle.0` has `PROCESS_QUERY_LIMITED_INFORMATION` and FILETIME pointers are valid.
            unsafe {
                GetProcessTimes(
                    proc_handle.0,
                    &mut creation,
                    &mut exit,
                    &mut kernel,
                    &mut user,
                )
            }
            .map_err(|err| SessionError::Win32 {
                operation: "GetProcessTimes",
                code: err.code().0 as u32,
            })?;

            let actual_creation =
                (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
            if expected_creation_time != 0 && actual_creation != expected_creation_time {
                return Err(SessionError::OwnerProcessCreationMismatch {
                    pid,
                    expected: expected_creation_time,
                    actual: actual_creation,
                });
            }

            // SAFETY: Creates a manual-reset event closed when the final `Arc<OwnedHandle>` drops.
            let cancel_raw = unsafe { CreateEventW(None, true, false, None) }.map_err(|err| {
                SessionError::Win32 {
                    operation: "CreateEventW",
                    code: err.code().0 as u32,
                }
            })?;
            let cancel_event = Arc::new(OwnedHandle(cancel_raw));
            let thread_cancel = Arc::clone(&cancel_event);
            let canceled = Arc::new(AtomicBool::new(false));
            let thread_canceled = Arc::clone(&canceled);

            thread::Builder::new()
                .name("zerovdd-pid-watch".to_string())
                .spawn(move || {
                    let proc_owned = proc_handle;
                    let handles = [proc_owned.0, thread_cancel.0];
                    // SAFETY: Both `proc_owned.0` and `thread_cancel.0` remain alive for the wait duration.
                    let wait = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
                    if wait == WAIT_OBJECT_0 && !thread_canceled.load(Ordering::Acquire) {
                        on_exit();
                    }
                })
                .map_err(|_| SessionError::Win32 {
                    operation: "thread::spawn",
                    code: 1,
                })?;

            Ok(Box::new(WindowsWatchHandle {
                cancel_event,
                canceled,
            }))
        }
    }
}

#[cfg(windows)]
pub use win32::{WindowsDriverProvider, WindowsProcessWatcher};

struct ActiveSession {
    generation: u64,
    client_id: String,
    monitor_guid: [u8; 16],
    adapter_luid: AdapterLuid,
    target_id: u32,
    display_name: Option<String>,
    width: u32,
    height: u32,
    refresh_mhz: u32,
    hdr_active: bool,
    owner_pid: u32,
    topology_mode: TopologyMode,
    watch_handle: Option<Box<dyn WatchHandle>>,
}

impl ActiveSession {
    fn to_status(&self) -> SessionStatus {
        SessionStatus {
            active: true,
            client_id: Some(self.client_id.clone()),
            display_name: self.display_name.clone(),
            target_id: Some(self.target_id),
            width: self.width,
            height: self.height,
            refresh_mhz: self.refresh_mhz,
            hdr_active: self.hdr_active,
            owner_pid: (self.owner_pid != 0).then_some(self.owner_pid),
            topology_mode: Some(self.topology_mode),
        }
    }
}

struct CoordinatorState {
    active_session: Option<ActiveSession>,
    pending_startup_restore: bool,
    standby_guid: Option<[u8; 16]>,
    persistent_guids: Vec<[u8; 16]>,
    next_generation: u64,
}

/// Thread-safe coordinator owning service display state, Sunshine session lifecycle,
/// topology backup preservation, and crash recovery.
pub struct ServiceCoordinator {
    self_weak: Weak<Self>,
    config: ConfigV1,
    snapshot_path: PathBuf,
    driver: Arc<dyn DriverProvider>,
    launcher: Arc<dyn ConsoleCcdRunner>,
    watcher: Arc<dyn ProcessWatcher>,
    gpu_enumerator: Arc<dyn GpuEnumerator>,
    started_at: Instant,
    op_mutex: Mutex<()>,
    state: Mutex<CoordinatorState>,
}

const fn to_config_topology_mode(mode: TopologyMode) -> SunshineTopologyMode {
    match mode {
        TopologyMode::Isolate => SunshineTopologyMode::Isolate,
        TopologyMode::Extend => SunshineTopologyMode::Extend,
    }
}

const fn to_display_rotation(orient: DisplayOrientation) -> DisplayRotation {
    match orient {
        DisplayOrientation::Identity => DisplayRotation::Identity,
        DisplayOrientation::Rotate90 => DisplayRotation::Rotate90,
        DisplayOrientation::Rotate180 => DisplayRotation::Rotate180,
        DisplayOrientation::Rotate270 => DisplayRotation::Rotate270,
    }
}

fn format_config_gpu_selector(sel: &zerovdd_config::GpuSelector) -> String {
    match sel {
        zerovdd_config::GpuSelector::AutoBest => "auto".to_string(),
        zerovdd_config::GpuSelector::Luid { low, high } => format!("luid:{low:#x}:{high}"),
        zerovdd_config::GpuSelector::PciBus {
            bus,
            device,
            function,
        } => format!("pci:{bus}:{device}.{function}"),
        zerovdd_config::GpuSelector::FriendlyName { name } => name.clone(),
    }
}

fn inactive_session_status() -> SessionStatus {
    SessionStatus {
        active: false,
        client_id: None,
        display_name: None,
        target_id: None,
        width: 0,
        height: 0,
        refresh_mhz: 0,
        hdr_active: false,
        owner_pid: None,
        topology_mode: None,
    }
}

impl ServiceCoordinator {
    /// Creates a new [`ServiceCoordinator`] wrapped in [`Arc`] for watcher callback wiring.
    pub fn new(
        config: ConfigV1,
        snapshot_path: Option<PathBuf>,
        driver: Arc<dyn DriverProvider>,
        launcher: Arc<dyn ConsoleCcdRunner>,
        watcher: Arc<dyn ProcessWatcher>,
    ) -> Arc<Self> {
        Self::with_gpu_enumerator(
            config,
            snapshot_path,
            driver,
            launcher,
            watcher,
            Arc::new(DefaultGpuEnumerator),
        )
    }

    /// Creates a [`ServiceCoordinator`] with a custom [`GpuEnumerator`] (for deterministic tests).
    pub fn with_gpu_enumerator(
        config: ConfigV1,
        snapshot_path: Option<PathBuf>,
        driver: Arc<dyn DriverProvider>,
        launcher: Arc<dyn ConsoleCcdRunner>,
        watcher: Arc<dyn ProcessWatcher>,
        gpu_enumerator: Arc<dyn GpuEnumerator>,
    ) -> Arc<Self> {
        Arc::new_cyclic(|self_weak| Self {
            self_weak: self_weak.clone(),
            config,
            snapshot_path: snapshot_path.unwrap_or_else(default_snapshot_path),
            driver,
            launcher,
            watcher,
            gpu_enumerator,
            started_at: Instant::now(),
            op_mutex: Mutex::new(()),
            state: Mutex::new(CoordinatorState {
                active_session: None,
                pending_startup_restore: false,
                standby_guid: None,
                persistent_guids: Vec::new(),
                next_generation: 1,
            }),
        })
    }

    /// Returns `true` if a startup topology restore is currently deferred waiting for console logon.
    pub fn is_pending_startup_restore(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending_startup_restore
    }

    /// Returns `true` if the headless-standby display is currently attached.
    pub fn is_headless_standby_active(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .standby_guid
            .is_some()
    }

    /// Returns the list of persistent display GUIDs attached during reconciliation.
    pub fn persistent_guids(&self) -> Vec<[u8; 16]> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .persistent_guids
            .clone()
    }

    /// Runs startup recovery for any leftover `topology-backup.json` and reconciles configured
    /// persistent and headless-standby profiles.
    pub fn initialize_on_startup(&self) -> Result<(), SessionError> {
        let _op = self
            .op_mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let _ = self.restore_snapshot_or_mark_pending(None)?;
        self.reconcile_profiles_under_op_lock();
        Ok(())
    }

    /// Handles a Windows `SERVICE_CONTROL_SESSIONCHANGE` notification and fires any deferred
    /// startup topology restore once a console session becomes available.
    pub fn on_session_change(&self, event: ConsoleSessionEvent) -> Result<bool, SessionError> {
        if !event.should_trigger_pending_restore() {
            return Ok(false);
        }

        let _op = self
            .op_mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let should_restore = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !self.snapshot_path.exists() {
                guard.pending_startup_restore = false;
            }
            guard.pending_startup_restore && guard.active_session.is_none()
        };

        if !should_restore {
            self.reconcile_profiles_under_op_lock();
            return Ok(false);
        }

        if self.launcher.console_session_id().is_none() {
            return Ok(false);
        }

        let restored = self.restore_snapshot_or_mark_pending(None)?;
        self.reconcile_profiles_under_op_lock();
        Ok(restored)
    }

    /// Starts or replaces a Sunshine streaming virtual display session.
    pub fn start_session(&self, req: &StartSessionRequest) -> Result<SessionStatus, SessionError> {
        req.validate()?;
        let _op = self
            .op_mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let driver = self.driver.get_or_connect()?;

        let prev_session = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active_session
            .take();

        let backup_existed_before = self.snapshot_path.exists();

        if let Some(mut prev) = prev_session {
            if let Some(watch) = prev.watch_handle.take() {
                watch.cancel();
            }
            // WHY: Preserve the original pre-first-session `topology-backup.json` across overlapping
            // StartSession calls; only replace the active virtual display.
            let _ = self.remove_display_ignore_missing(
                driver.as_ref(),
                &RemoveDisplayRequest::by_guid(prev.monitor_guid),
            );
        } else {
            let mut standby_guid = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .standby_guid;
            if standby_guid.is_some() {
                let _ = depart_headless_standby(driver.as_ref(), &mut standby_guid);
                self.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .standby_guid = standby_guid;
            }
            if !backup_existed_before {
                if let Err(err) = self.launcher.snapshot(&self.snapshot_path, false) {
                    self.reconcile_profiles_under_op_lock();
                    return Err(err.into());
                }
            }
        }

        let monitor_guid = monitor_guid_for_name(&req.client_id);
        let (refresh_num, refresh_den) = millihz_to_rational(req.refresh_mhz);
        let hdr_profile = if req.hdr {
            HdrProfile::Hdr10
        } else {
            HdrProfile::Sdr
        };
        let mut add_req = AddDisplayRequest::new(
            monitor_guid,
            req.width,
            req.height,
            refresh_num,
            refresh_den,
            hdr_profile,
            OwnershipMode::HandleBound,
        );
        if req.hdr {
            add_req.hdr_metadata = hdr_metadata_from_defaults(&self.config.hdr_defaults);
        }
        if let Some(ref edid_path) = self.config.sunshine.default_edid_path {
            match load_and_validate_edid_file(std::path::Path::new(edid_path)) {
                Ok(bytes) => {
                    add_req.set_custom_edid(&bytes)?;
                }
                Err(msg) => {
                    eprintln!("zerovdd-svc ignoring sunshine.default_edid_path warning: {msg}");
                }
            }
        }
        add_req.set_client_name(&req.client_id)?;

        let added = match driver.add_display(&add_req) {
            Ok(resp) => resp,
            Err(err) => {
                self.reset_driver_if_stale(driver.as_ref(), &err);
                // WHY: Always restore any saved physical topology (including during an overlapping
                // StartSession where `prev_session` was already removed) so 0 active displays remain.
                let _ = self.restore_snapshot_or_mark_pending(None);
                self.reconcile_profiles_under_op_lock();
                return Err(err.into());
            }
        };

        let adapter_luid = AdapterLuid::new(added.adapter_luid_low, added.adapter_luid_high);
        let apply_payload = ApplyCcdPayload {
            adapter_luid_low: added.adapter_luid_low,
            adapter_luid_high: added.adapter_luid_high,
            target_id: added.target_id,
            width: req.width,
            height: req.height,
            refresh_num,
            refresh_den,
            hdr: req.hdr,
            topology_mode: to_config_topology_mode(req.topology_mode),
            snapshot_path: None,
            extend_origin: None,
            make_virtual_primary: false,
            rotation: DisplayRotation::Identity,
        };

        let applied_record = match self.launcher.apply(&apply_payload) {
            Ok(record) => record,
            Err(err) => {
                let _ = self.remove_display_ignore_missing(
                    driver.as_ref(),
                    &RemoveDisplayRequest::by_guid(monitor_guid),
                );
                let _ =
                    self.restore_snapshot_or_mark_pending(Some((adapter_luid, added.target_id)));
                self.reconcile_profiles_under_op_lock();
                return Err(err.into());
            }
        };

        let generation = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let gen = guard.next_generation;
            guard.next_generation = guard.next_generation.saturating_add(1);
            gen
        };

        let watch_handle = if req.owner_pid != 0 {
            let weak_self: Weak<Self> = Weak::clone(&self.self_weak);
            match self.watcher.watch_process(
                req.owner_pid,
                req.owner_creation_time,
                Box::new(move || {
                    if let Some(coord) = weak_self.upgrade() {
                        let _ = coord.handle_owner_exit(generation);
                    }
                }),
            ) {
                Ok(handle) => Some(handle),
                Err(err) => {
                    let _ = self.remove_display_ignore_missing(
                        driver.as_ref(),
                        &RemoveDisplayRequest::by_guid(monitor_guid),
                    );
                    let _ = self
                        .restore_snapshot_or_mark_pending(Some((adapter_luid, added.target_id)));
                    self.reconcile_profiles_under_op_lock();
                    return Err(err);
                }
            }
        } else {
            None
        };

        let display_name = applied_record
            .map(|r| r.gdi_name)
            .filter(|name| !name.is_empty());

        let active = ActiveSession {
            generation,
            client_id: req.client_id.clone(),
            monitor_guid,
            adapter_luid,
            target_id: added.target_id,
            display_name,
            width: req.width,
            height: req.height,
            refresh_mhz: req.refresh_mhz,
            hdr_active: req.hdr,
            owner_pid: req.owner_pid,
            topology_mode: req.topology_mode,
            watch_handle,
        };
        let status = active.to_status();

        {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.active_session = Some(active);
            guard.pending_startup_restore = false;
        }

        Ok(status)
    }

    /// Stops the active Sunshine streaming session, departs its virtual display, and restores topology.
    pub fn stop_session(&self, req: &StopSessionRequest) -> Result<bool, SessionError> {
        req.validate()?;
        let _op = self
            .op_mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let active = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(ref current) = guard.active_session {
                if !req.force {
                    if let Some(ref filter_client) = req.client_id {
                        if filter_client != &current.client_id {
                            return Err(SessionError::SessionOwnerMismatch);
                        }
                    }
                    if let Some(filter_pid) = req.owner_pid {
                        if filter_pid != current.owner_pid {
                            return Err(SessionError::SessionOwnerMismatch);
                        }
                    }
                }
            }
            guard.active_session.take()
        };

        let Some(mut session) = active else {
            if self.snapshot_path.exists() {
                let restored = self.restore_snapshot_or_mark_pending(None)?;
                self.reconcile_profiles_under_op_lock();
                return Ok(restored);
            }
            return Ok(false);
        };

        if let Some(watch) = session.watch_handle.take() {
            watch.cancel();
        }

        let restored = self.teardown_session_resources(
            session.monitor_guid,
            session.adapter_luid,
            session.target_id,
        )?;
        Ok(restored)
    }

    /// Invoked by [`ProcessWatcher`] when the owning `sunshine.exe` process exits unexpectedly.
    pub fn handle_owner_exit(&self, generation: u64) -> Result<bool, SessionError> {
        let _op = self
            .op_mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let session = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let matches_gen = guard
                .active_session
                .as_ref()
                .is_some_and(|s| s.generation == generation);
            if !matches_gen {
                return Ok(false);
            }
            guard.active_session.take()
        };

        let Some(mut active) = session else {
            return Ok(false);
        };
        // WHY: Avoid self-joining the watcher thread from inside its own callback.
        let _ = active.watch_handle.take();

        self.teardown_session_resources(active.monitor_guid, active.adapter_luid, active.target_id)
    }

    fn teardown_session_resources(
        &self,
        monitor_guid: [u8; 16],
        adapter_luid: AdapterLuid,
        target_id: u32,
    ) -> Result<bool, SessionError> {
        if let Ok(driver) = self.driver.get_or_connect() {
            let _ = self.remove_display_ignore_missing(
                driver.as_ref(),
                &RemoveDisplayRequest::by_guid(monitor_guid),
            );
        }

        let restored = self.restore_snapshot_or_mark_pending(Some((adapter_luid, target_id)))?;
        self.reconcile_profiles_under_op_lock();
        Ok(restored)
    }

    /// Returns a snapshot of the active streaming session without blocking on console CCD operations.
    pub fn get_session_status(&self) -> SessionStatus {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active_session
            .as_ref()
            .map_or_else(inactive_session_status, ActiveSession::to_status)
    }

    /// Adds a virtual display via the service's held Control Device handle and optionally applies topology.
    pub fn display_add(&self, req: &DisplayAddRequest) -> Result<IpcResponse, SessionError> {
        req.validate()?;
        let _op = self
            .op_mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let effective_topology_mode = req.topology_mode.or_else(|| {
            (req.orientation.is_some()
                || req.position.is_some()
                || req.make_primary.unwrap_or(false))
            .then_some(TopologyMode::Extend)
        });

        if effective_topology_mode.is_some() && self.launcher.console_session_id().is_none() {
            return Err(SessionError::Launcher(
                LauncherError::ConsoleSessionUnavailable,
            ));
        }

        let driver = self.driver.get_or_connect()?;
        if let Some(ref gpu_str) = req.gpu {
            let selector = parse_gpu_selector(gpu_str)?;
            let adapters = self.gpu_enumerator.enumerate_adapters()?;
            let chosen = select_gpu(&adapters, &selector)?;
            let gpu_req =
                SetRenderAdapterRequest::new(chosen.adapter_luid_low, chosen.adapter_luid_high);
            if let Err(err) = driver.set_render_adapter(&gpu_req) {
                self.reset_driver_if_stale(driver.as_ref(), &err);
                return Err(err.into());
            }
        }

        let monitor_guid = match req.guid {
            Some(ref raw_guid) => *Uuid::parse_str(raw_guid.trim())
                .map_err(|_| {
                    SessionError::InvalidRequest(IpcError::InvalidPayload(
                        ProtoError::InvalidSelector,
                    ))
                })?
                .as_bytes(),
            None => monitor_guid_for_name(&req.client_id),
        };
        let (refresh_num, refresh_den) = millihz_to_rational(req.refresh_mhz);
        let hdr_profile = if req.hdr {
            HdrProfile::Hdr10
        } else {
            HdrProfile::Sdr
        };
        let ownership = if req.lease_ttl_secs.is_some() {
            OwnershipMode::Leased
        } else {
            OwnershipMode::HandleBound
        };

        let mut add_req = AddDisplayRequest::new(
            monitor_guid,
            req.width,
            req.height,
            refresh_num,
            refresh_den,
            hdr_profile,
            ownership,
        );
        if let Some(ttl) = req.lease_ttl_secs {
            add_req.lease_ttl_secs = ttl;
        }
        if req.hdr {
            add_req.hdr_metadata = hdr_metadata_with_overrides(
                &self.config.hdr_defaults,
                req.max_luminance_nits,
                req.max_fall_nits,
                req.min_luminance_ten_thousandths,
            );
        }
        add_req.target_ppi = req.target_ppi.unwrap_or(0);
        add_req.color_space =
            ColorSpacePreference::from(req.color_space.unwrap_or_default()).as_u8();
        add_req.bit_depth = BitDepthPreference::from(req.bit_depth.unwrap_or_default()).as_u8();
        if let Some(ref custom_edid) = req.custom_edid {
            zerovdd_edid::validate_custom_edid(custom_edid)
                .map_err(|err| SessionError::InvalidEdid(err.to_string()))?;
            add_req.set_custom_edid(custom_edid)?;
        }
        add_req.set_client_name(&req.client_id)?;

        let resp = match driver.add_display(&add_req) {
            Ok(resp) => resp,
            Err(err) => {
                self.reset_driver_if_stale(driver.as_ref(), &err);
                return Err(err.into());
            }
        };

        if let Some(topology_mode) = effective_topology_mode {
            let apply_payload = ApplyCcdPayload {
                adapter_luid_low: resp.adapter_luid_low,
                adapter_luid_high: resp.adapter_luid_high,
                target_id: resp.target_id,
                width: req.width,
                height: req.height,
                refresh_num,
                refresh_den,
                hdr: req.hdr,
                topology_mode: to_config_topology_mode(topology_mode),
                snapshot_path: None,
                extend_origin: req.position,
                make_virtual_primary: req.make_primary.unwrap_or(false)
                    || topology_mode == TopologyMode::Isolate,
                rotation: to_display_rotation(req.orientation.unwrap_or_default()),
            };
            if let Err(err) = self.launcher.apply(&apply_payload) {
                // WHY: Roll back the newly added display so failed CCD application never leaves an orphan.
                let _ = self.remove_display_ignore_missing(
                    driver.as_ref(),
                    &RemoveDisplayRequest::by_guid(monitor_guid),
                );
                return Err(err.into());
            }
        }

        Ok(IpcResponse::DisplayAdded {
            adapter_luid_low: resp.adapter_luid_low,
            adapter_luid_high: resp.adapter_luid_high,
            target_id: resp.target_id,
            connector_index: resp.connector_index,
        })
    }

    /// Removes a virtual display matching `client_id` (GUID string or name) or `target_id`.
    pub fn display_remove(&self, req: &DisplayRemoveRequest) -> Result<bool, SessionError> {
        req.validate()?;
        let _op = self
            .op_mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let driver = self.driver.get_or_connect()?;
        let list = match driver.list_displays() {
            Ok(list) => list,
            Err(err) => {
                self.reset_driver_if_stale(driver.as_ref(), &err);
                return Err(err.into());
            }
        };
        let entries = list.entries()?;

        let matched_guid: Option<[u8; 16]> = if let Some(target_id) = req.target_id {
            entries
                .iter()
                .find(|info| info.target_id == target_id)
                .map(|info| info.monitor_guid)
        } else if let Some(ref cid) = req.client_id {
            let parsed_uuid = Uuid::parse_str(cid).ok().map(|u| *u.as_bytes());
            let derived_guid = monitor_guid_for_name(cid);
            entries
                .iter()
                .find(|info| {
                    parsed_uuid == Some(info.monitor_guid)
                        || info.monitor_guid == derived_guid
                        || info.client_name_str().ok() == Some(cid.as_str())
                })
                .map(|info| info.monitor_guid)
        } else {
            None
        };

        let Some(matched) = matched_guid else {
            return Ok(false);
        };
        let remove_req = match req.target_id {
            Some(target_id) => RemoveDisplayRequest::by_target_id(target_id),
            None => RemoveDisplayRequest::by_guid(matched),
        };

        match driver.remove_display(&remove_req) {
            Ok(()) => {
                let mut guard = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if guard.standby_guid == Some(matched) {
                    guard.standby_guid = None;
                }
                Ok(true)
            }
            Err(ClientError::MonitorNotFound) => {
                let mut guard = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if guard.standby_guid == Some(matched) {
                    guard.standby_guid = None;
                }
                Ok(false)
            }
            Err(err) => {
                self.reset_driver_if_stale(driver.as_ref(), &err);
                Err(err.into())
            }
        }
    }

    /// Updates resolution, refresh rate, and optional HDR state on an active virtual display in place.
    pub fn display_set_mode(
        &self,
        req: &DisplaySetModeRequest,
    ) -> Result<IpcResponse, SessionError> {
        req.validate()?;
        let _op = self
            .op_mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let driver = self.driver.get_or_connect()?;
        let list = match driver.list_displays() {
            Ok(list) => list,
            Err(err) => {
                self.reset_driver_if_stale(driver.as_ref(), &err);
                return Err(err.into());
            }
        };
        let entries = list.entries()?;

        let matched = if let Some(target_id) = req.target_id {
            entries
                .iter()
                .find(|info| info.target_id == target_id)
                .ok_or(SessionError::Client(ClientError::MonitorNotFound))?
        } else if let Some(ref cid) = req.client_id {
            let parsed_uuid = Uuid::parse_str(cid).ok().map(|u| *u.as_bytes());
            let derived_guid = monitor_guid_for_name(cid);
            let matches: Vec<_> = entries
                .iter()
                .filter(|info| {
                    parsed_uuid == Some(info.monitor_guid)
                        || info.monitor_guid == derived_guid
                        || info.client_name_str().ok() == Some(cid.as_str())
                })
                .collect();
            match matches.as_slice() {
                [] => return Err(SessionError::Client(ClientError::MonitorNotFound)),
                [only] => *only,
                _ => {
                    return Err(SessionError::InvalidRequest(IpcError::InvalidPayload(
                        ProtoError::InvalidSelector,
                    )))
                }
            }
        } else if entries.len() == 1 {
            &entries[0]
        } else if entries.is_empty() {
            return Err(SessionError::Client(ClientError::MonitorNotFound));
        } else {
            return Err(SessionError::InvalidRequest(IpcError::InvalidPayload(
                ProtoError::InvalidSelector,
            )));
        };

        if self.launcher.console_session_id().is_none() {
            return Err(SessionError::Launcher(
                LauncherError::ConsoleSessionUnavailable,
            ));
        }

        let prev_hdr_profile = HdrProfile::try_from(matched.hdr_profile).unwrap_or(HdrProfile::Sdr);
        let current_hdr = prev_hdr_profile == HdrProfile::Hdr10;
        let hdr_active = req.hdr.unwrap_or(current_hdr);
        let hdr_profile = if hdr_active {
            HdrProfile::Hdr10
        } else {
            HdrProfile::Sdr
        };
        let (refresh_num, refresh_den) = millihz_to_rational(req.refresh_mhz);
        let prev_req = UpdateDisplayModeRequest::by_guid(
            matched.monitor_guid,
            matched.width,
            matched.height,
            matched.refresh_num,
            matched.refresh_den,
            prev_hdr_profile,
        );
        let mut update_req = UpdateDisplayModeRequest::by_guid(
            matched.monitor_guid,
            req.width,
            req.height,
            refresh_num,
            refresh_den,
            hdr_profile,
        );
        update_req.color_space =
            ColorSpacePreference::from(req.color_space.unwrap_or_default()).as_u8();
        update_req.bit_depth = BitDepthPreference::from(req.bit_depth.unwrap_or_default()).as_u8();
        if hdr_active
            && (req.max_luminance_nits.is_some()
                || req.max_fall_nits.is_some()
                || req.min_luminance_ten_thousandths.is_some())
        {
            update_req.hdr_metadata = hdr_metadata_with_overrides(
                &self.config.hdr_defaults,
                req.max_luminance_nits,
                req.max_fall_nits,
                req.min_luminance_ten_thousandths,
            );
        } else if hdr_active {
            update_req.hdr_metadata = HdrMasteringMetadata::default();
        }

        let resp = match driver.update_display_mode(&update_req) {
            Ok(resp) => resp,
            Err(err) => {
                self.reset_driver_if_stale(driver.as_ref(), &err);
                return Err(err.into());
            }
        };

        let payload = SetModeCcdPayload {
            adapter_luid_low: resp.adapter_luid_low,
            adapter_luid_high: resp.adapter_luid_high,
            target_id: resp.target_id,
            width: req.width,
            height: req.height,
            refresh_num,
            refresh_den,
            hdr: req.hdr,
        };
        let applied_path = match self.launcher.set_mode(&payload) {
            Ok(path) => path,
            Err(err) => {
                let _ = driver.update_display_mode(&prev_req);
                return Err(err.into());
            }
        };

        {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(ref mut active) = guard.active_session {
                if active.monitor_guid == matched.monitor_guid {
                    active.target_id = resp.target_id;
                    active.adapter_luid.low = resp.adapter_luid_low;
                    active.adapter_luid.high = resp.adapter_luid_high;
                    if let Some(ref path) = applied_path {
                        active.display_name = Some(path.gdi_name.clone());
                    }
                    active.width = req.width;
                    active.height = req.height;
                    active.refresh_mhz = req.refresh_mhz;
                    active.hdr_active = hdr_active;
                }
            }
        }

        Ok(IpcResponse::DisplayModeUpdated {
            adapter_luid_low: resp.adapter_luid_low,
            adapter_luid_high: resp.adapter_luid_high,
            target_id: resp.target_id,
            connector_index: resp.connector_index,
            width: req.width,
            height: req.height,
            refresh_mhz: req.refresh_mhz,
            hdr_active,
            mode_count: resp.mode_count,
        })
    }

    /// Queries service health, driver connection status, active displays, and backup state.
    pub fn get_health(&self) -> ServiceHealth {
        let client = self
            .driver
            .connected_client()
            .or_else(|| self.driver.get_or_connect().ok());

        let (driver_connected, protocol_version, active_displays) = match client {
            Some(dev) => {
                let version = dev.get_version().ok();
                let displays = dev
                    .get_status()
                    .map(|s| s.active_display_count)
                    .or_else(|_| dev.list_displays().map(|l| l.total_active));
                match (version, displays) {
                    (Some(ver), Ok(count)) => {
                        (true, Some((ver.major, ver.minor, ver.patch)), count)
                    }
                    (Some(ver), Err(_)) => (true, Some((ver.major, ver.minor, ver.patch)), 0),
                    (None, _) => {
                        self.driver.reset();
                        (false, None, 0)
                    }
                }
            }
            None => (false, None, 0),
        };

        ServiceHealth {
            service_version: env!("CARGO_PKG_VERSION").to_string(),
            driver_connected,
            protocol_version,
            active_displays,
            uptime_secs: self.started_at.elapsed().as_secs(),
            backup_present: self.snapshot_path.exists(),
        }
    }

    /// Lists active virtual displays reported by the driver over the service Control Device handle.
    pub fn display_list(&self) -> Result<IpcResponse, SessionError> {
        let _op = self
            .op_mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let driver = self.driver.get_or_connect()?;
        let list = match driver.list_displays() {
            Ok(list) => list,
            Err(err) => {
                self.reset_driver_if_stale(driver.as_ref(), &err);
                return Err(err.into());
            }
        };
        let entries = list.entries()?;
        let items = entries
            .iter()
            .map(|d| ActiveDisplayInfo {
                monitor_guid: Uuid::from_bytes(d.monitor_guid).hyphenated().to_string(),
                client_name: d.client_name_str().unwrap_or("<invalid>").to_string(),
                adapter_luid_low: d.adapter_luid_low,
                adapter_luid_high: d.adapter_luid_high,
                target_id: d.target_id,
                connector_index: d.connector_index,
                width: d.width,
                height: d.height,
                refresh_num: d.refresh_num,
                refresh_den: d.refresh_den,
                hdr_active: d.hdr_profile == HdrProfile::Hdr10.as_u8(),
                ownership_mode: d.ownership_mode,
                lease_remaining_secs: d.lease_remaining_secs,
            })
            .collect();
        Ok(IpcResponse::DisplayList { displays: items })
    }

    /// Enumerates DXGI render GPU adapters alongside configured selector and driver switch status.
    pub fn list_gpus(&self) -> Result<IpcResponse, SessionError> {
        let _op = self
            .op_mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let adapters = self.gpu_enumerator.enumerate_adapters()?;
        let best_luid = select_gpu(&adapters, &GpuSelector::AutoBest)
            .ok()
            .map(|a| (a.adapter_luid_low, a.adapter_luid_high));
        let last_gpu_switch_failed = self
            .driver
            .get_or_connect()
            .ok()
            .and_then(|d| d.get_status().ok())
            .is_some_and(|s| s.last_gpu_switch_failed != 0);

        let gpus = adapters
            .into_iter()
            .map(|a| GpuAdapterSummary {
                is_best: best_luid == Some((a.adapter_luid_low, a.adapter_luid_high)),
                friendly_name: a.name,
                adapter_luid_low: a.adapter_luid_low,
                adapter_luid_high: a.adapter_luid_high,
                pci_bus: a.pci_bus,
                pci_device: a.pci_device,
                pci_function: a.pci_function,
                dedicated_vram_bytes: a.dedicated_vram_bytes,
                is_software: a.is_software,
            })
            .collect();

        Ok(IpcResponse::GpuList {
            gpus,
            configured_selector: format_config_gpu_selector(&self.config.gpu),
            last_gpu_switch_failed,
        })
    }

    /// Switches the driver's active render GPU adapter via the service Control Device handle.
    pub fn switch_gpu(&self, req: &SwitchGpuRequest) -> Result<IpcResponse, SessionError> {
        req.validate()?;
        let _op = self
            .op_mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let selector = parse_gpu_selector(&req.selector)?;
        let adapters = self.gpu_enumerator.enumerate_adapters()?;
        let chosen = select_gpu(&adapters, &selector)?;

        let driver = self.driver.get_or_connect()?;
        let wire_req =
            SetRenderAdapterRequest::new(chosen.adapter_luid_low, chosen.adapter_luid_high);
        if let Err(err) = driver.set_render_adapter(&wire_req) {
            self.reset_driver_if_stale(driver.as_ref(), &err);
            return Err(err.into());
        }

        Ok(IpcResponse::GpuSwitched {
            adapter_luid_low: chosen.adapter_luid_low,
            adapter_luid_high: chosen.adapter_luid_high,
            friendly_name: chosen.name.clone(),
        })
    }

    /// Stops any active session, departs active virtual displays, and restores physical topology.
    pub fn restore_topology(&self) -> Result<IpcResponse, SessionError> {
        let _op = self
            .op_mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if self.launcher.console_session_id().is_none() {
            return Err(SessionError::Launcher(
                LauncherError::ConsoleSessionUnavailable,
            ));
        }

        let mut departed_displays = 0u32;
        let mut excluded_target: Option<(AdapterLuid, u32)> = None;

        let active = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.active_session.take()
        };
        if let Some(mut session) = active {
            if let Some(watch) = session.watch_handle.take() {
                watch.cancel();
            }
            excluded_target = Some((session.adapter_luid, session.target_id));
            if let Ok(driver) = self.driver.get_or_connect() {
                if self
                    .remove_display_ignore_missing(
                        driver.as_ref(),
                        &RemoveDisplayRequest::by_guid(session.monitor_guid),
                    )
                    .is_ok()
                {
                    departed_displays = departed_displays.saturating_add(1);
                }
            }
        }

        if let Ok(driver) = self.driver.get_or_connect() {
            if let Ok(list) = driver.list_displays() {
                if let Ok(entries) = list.entries() {
                    for info in entries {
                        if excluded_target.is_none() {
                            excluded_target = Some((
                                AdapterLuid::new(info.adapter_luid_low, info.adapter_luid_high),
                                info.target_id,
                            ));
                        }
                        if driver
                            .remove_display(&RemoveDisplayRequest::by_guid(info.monitor_guid))
                            .is_ok()
                        {
                            departed_displays = departed_displays.saturating_add(1);
                        }
                    }
                }
            }
        }

        {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.standby_guid = None;
            guard.persistent_guids.clear();
            guard.pending_startup_restore = false;
        }

        let report =
            self.launcher
                .restore_with_fallback(&self.snapshot_path, excluded_target, true)?;
        Ok(IpcResponse::TopologyRestored {
            restored_from_backup: report.snapshot_found,
            departed_displays,
        })
    }

    /// Cancels any active session watcher and departs service-managed displays on service shutdown.
    pub fn shutdown(&self) {
        let _op = self
            .op_mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let active = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.active_session.take()
        };
        if let Some(mut session) = active {
            if let Some(watch) = session.watch_handle.take() {
                watch.cancel();
            }
            let _ = self.teardown_session_resources(
                session.monitor_guid,
                session.adapter_luid,
                session.target_id,
            );
        }

        if let Ok(driver) = self.driver.get_or_connect() {
            let mut standby = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .standby_guid
                .take();
            let _ = depart_headless_standby(driver.as_ref(), &mut standby);

            let persistent = {
                let mut guard = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                core::mem::take(&mut guard.persistent_guids)
            };
            for guid in persistent {
                let _ = self.remove_display_ignore_missing(
                    driver.as_ref(),
                    &RemoveDisplayRequest::by_guid(guid),
                );
            }
        }
    }

    fn reconcile_profiles_under_op_lock(&self) {
        let has_active_or_pending = {
            let guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.active_session.is_some() || guard.pending_startup_restore
        };
        if has_active_or_pending {
            return;
        }

        let Ok(driver) = self.driver.get_or_connect() else {
            return;
        };
        let _ = apply_configured_gpu_preference(driver.as_ref(), &self.config);
        if let Ok(guids) = reconcile_persistent_displays(driver.as_ref(), &self.config) {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .persistent_guids = guids;
        }

        let mut standby = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .standby_guid;
        if reconcile_headless_standby(
            driver.as_ref(),
            self.launcher.as_ref(),
            &self.config,
            &mut standby,
        )
        .is_ok()
        {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .standby_guid = standby;
        }
    }

    fn restore_snapshot_or_mark_pending(
        &self,
        virtual_target: Option<(AdapterLuid, u32)>,
    ) -> Result<bool, SessionError> {
        if !self.snapshot_path.exists() {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pending_startup_restore = false;
            return Ok(false);
        }

        if self.launcher.console_session_id().is_none() {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pending_startup_restore = true;
            return Ok(false);
        }

        match self.launcher.restore(&self.snapshot_path, virtual_target) {
            Ok(report) => {
                self.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .pending_startup_restore = false;
                Ok(report.snapshot_found)
            }
            Err(err) => {
                if self.snapshot_path.exists() {
                    self.state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .pending_startup_restore = true;
                }
                Err(err.into())
            }
        }
    }

    fn reset_driver_if_stale(&self, driver: &dyn ControlClient, err: &ClientError) {
        match err {
            ClientError::DeviceNotFound(_) | ClientError::AccessDenied | ClientError::Io(_) => {
                self.driver.reset();
            }
            // WHY: Domain IOCTL rejections arrive as ClientError::Win32; resetting a live
            // Control Device handle fires EvtFileCleanup and departs all HandleBound displays.
            ClientError::Win32 { .. } if driver.get_version().is_err() => {
                self.driver.reset();
            }
            _ => {}
        }
    }

    fn remove_display_ignore_missing(
        &self,
        driver: &dyn ControlClient,
        req: &RemoveDisplayRequest,
    ) -> Result<(), SessionError> {
        match driver.remove_display(req) {
            Ok(()) | Err(ClientError::MonitorNotFound) => Ok(()),
            Err(err) => {
                self.reset_driver_if_stale(driver, &err);
                Err(err.into())
            }
        }
    }
}

impl IpcHandler for ServiceCoordinator {
    fn handle(&self, request: IpcRequest) -> IpcResponse {
        match request {
            IpcRequest::StartSession(req) => match self.start_session(&req) {
                Ok(status) => IpcResponse::SessionStarted(status),
                Err(err) => IpcResponse::Error(err.to_ipc_payload()),
            },
            IpcRequest::StopSession(req) => match self.stop_session(&req) {
                Ok(restored_topology) => IpcResponse::SessionStopped { restored_topology },
                Err(err) => IpcResponse::Error(err.to_ipc_payload()),
            },
            IpcRequest::GetSessionStatus => IpcResponse::SessionStatus(self.get_session_status()),
            IpcRequest::DisplayAdd(req) => match self.display_add(&req) {
                Ok(resp) => resp,
                Err(err) => IpcResponse::Error(err.to_ipc_payload()),
            },
            IpcRequest::DisplayRemove(req) => match self.display_remove(&req) {
                Ok(removed) => IpcResponse::DisplayRemoved { removed },
                Err(err) => IpcResponse::Error(err.to_ipc_payload()),
            },
            IpcRequest::DisplaySetMode(req) => match self.display_set_mode(&req) {
                Ok(resp) => resp,
                Err(err) => IpcResponse::Error(err.to_ipc_payload()),
            },
            IpcRequest::DisplayList => match self.display_list() {
                Ok(resp) => resp,
                Err(err) => IpcResponse::Error(err.to_ipc_payload()),
            },
            IpcRequest::ListGpus => match self.list_gpus() {
                Ok(resp) => resp,
                Err(err) => IpcResponse::Error(err.to_ipc_payload()),
            },
            IpcRequest::SwitchGpu(req) => match self.switch_gpu(&req) {
                Ok(resp) => resp,
                Err(err) => IpcResponse::Error(err.to_ipc_payload()),
            },
            IpcRequest::RestoreTopology => match self.restore_topology() {
                Ok(resp) => resp,
                Err(err) => IpcResponse::Error(err.to_ipc_payload()),
            },
            IpcRequest::GetHealth => IpcResponse::Health(self.get_health()),
        }
    }
}
