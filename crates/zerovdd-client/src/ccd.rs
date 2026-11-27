//! Windows CCD topology backend, bounded virtual display resolution, and session orchestration.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::ClientError;
use crate::hdr::HdrTargetSetting;
use crate::topology::{
    load_snapshot, plan_enable_attached_physical, plan_in_place_mode_update, plan_restore_topology,
    plan_streaming_topology, remove_snapshot, save_snapshot_atomic, snapshot_from_state,
    AdapterLuid, CcdTopologyState, DisplayPathRecord, StreamingTopologyRequest,
};

/// Bounded retry configuration used when waiting for a newly arrived virtual display in CCD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Maximum number of CCD query attempts before failing.
    pub max_attempts: u32,
    /// Delay in milliseconds between consecutive retry attempts.
    pub delay_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 20,
            delay_ms: 100,
        }
    }
}

/// Result returned after applying a streaming virtual display topology.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedStreamingSession {
    /// Resolved CCD record for the virtual display prior to applying topology changes.
    pub virtual_display: DisplayPathRecord,
    /// `true` if a new backup snapshot was written; `false` if skipped or an existing backup was kept.
    pub snapshot_saved: bool,
    /// Number of active display paths in the applied topology.
    pub active_path_count: usize,
}

/// Summary returned after restoring a persisted physical topology snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredTopologyReport {
    /// `true` if a backup snapshot existed and was applied; `false` if no backup was present.
    pub snapshot_found: bool,
    /// Number of physical display paths restored as active.
    pub restored_path_count: usize,
    /// Physical `(adapter_luid, target_id)` pairs from the backup skipped because they were unplugged.
    pub skipped_unplugged_targets: Vec<(AdapterLuid, u32)>,
}

/// Abstraction over Windows CCD topology query/apply and per-display HDR/WCG application.
pub trait DisplayTopologyBackend {
    /// Queries the current OS build, all display paths, active modes, and per-display color state.
    fn query_topology(&self) -> Result<CcdTopologyState, ClientError>;

    /// Applies the supplied active display paths via `SetDisplayConfig`.
    fn apply_topology(&self, desired_paths: &[DisplayPathRecord]) -> Result<(), ClientError>;

    /// Applies an HDR/WCG state update to a single display target.
    fn apply_hdr_setting(&self, setting: &HdrTargetSetting) -> Result<(), ClientError>;
}

/// Resolves `(adapter_luid, target_id)` in CCD using a caller-supplied wait hook between attempts.
pub fn resolve_virtual_display_with_wait<B, W>(
    backend: &B,
    adapter_luid: AdapterLuid,
    target_id: u32,
    policy: RetryPolicy,
    mut wait: W,
) -> Result<DisplayPathRecord, ClientError>
where
    B: DisplayTopologyBackend,
    W: FnMut(u32, u64),
{
    let attempts = policy.max_attempts.max(1);
    for attempt in 1..=attempts {
        let state = backend.query_topology()?;
        if let Some(found) = state.paths.into_iter().find(|path| {
            path.adapter_luid == adapter_luid
                && path.target_id == target_id
                && path.target_available
        }) {
            return Ok(found);
        }
        if attempt < attempts {
            wait(attempt, policy.delay_ms);
        }
    }

    Err(ClientError::VirtualDisplayNotFound {
        adapter_luid_low: adapter_luid.low,
        adapter_luid_high: adapter_luid.high,
        target_id,
    })
}

#[cfg(all(windows, not(test)))]
fn production_wait_backoff(_attempt: u32, delay_ms: u64) {
    if delay_ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
    }
}

#[cfg(any(not(windows), test))]
fn production_wait_backoff(_attempt: u32, _delay_ms: u64) {}

/// Resolves `(adapter_luid, target_id)` in CCD using bounded production retry backoff.
pub fn resolve_virtual_display<B: DisplayTopologyBackend>(
    backend: &B,
    adapter_luid: AdapterLuid,
    target_id: u32,
    policy: RetryPolicy,
) -> Result<DisplayPathRecord, ClientError> {
    resolve_virtual_display_with_wait(
        backend,
        adapter_luid,
        target_id,
        policy,
        production_wait_backoff,
    )
}

/// Options controlling snapshot backup and CCD retry timing when applying a streaming session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionApplyOptions<'a> {
    /// Optional backup file path where pre-streaming physical topology should be saved.
    pub snapshot_path: Option<&'a Path>,
    /// Timestamp in milliseconds since Unix epoch recorded in the snapshot.
    pub created_at_unix_ms: u64,
    /// Bounded retry policy used while waiting for the virtual display to appear in CCD.
    pub retry_policy: RetryPolicy,
}

/// Resolves the virtual display, snapshots physical topology if needed, and applies streaming layout.
pub fn apply_streaming_session_with_wait<B, W>(
    backend: &B,
    virtual_luid: AdapterLuid,
    virtual_target_id: u32,
    request: &StreamingTopologyRequest,
    options: &SessionApplyOptions<'_>,
    wait: W,
) -> Result<AppliedStreamingSession, ClientError>
where
    B: DisplayTopologyBackend,
    W: FnMut(u32, u64),
{
    let virtual_display = resolve_virtual_display_with_wait(
        backend,
        virtual_luid,
        virtual_target_id,
        options.retry_policy,
        wait,
    )?;

    let current = backend.query_topology()?;
    let mut snapshot_saved = false;
    if let Some(path) = options.snapshot_path {
        // WHY: Preserve an already-persisted physical backup across crash restarts so a second
        // streaming launch never overwrites the pre-streaming layout with an isolated virtual display.
        if load_snapshot(path)?.is_none() {
            let snapshot = snapshot_from_state(
                &current,
                options.created_at_unix_ms,
                Some((virtual_luid, virtual_target_id)),
            );
            // WHY: Headless systems have zero active physical displays to restore, so persisting
            // an empty snapshot would fail teardown or leak a stale backup file on disk.
            if snapshot.paths.iter().any(|p| p.active) {
                save_snapshot_atomic(path, &snapshot)?;
                snapshot_saved = true;
            }
        }
    }

    let plan = plan_streaming_topology(&current, virtual_luid, virtual_target_id, request)?;
    let active_path_count = plan.desired_paths.iter().filter(|p| p.active).count();
    backend.apply_topology(&plan.desired_paths)?;
    for setting in &plan.hdr_updates {
        backend.apply_hdr_setting(setting)?;
    }

    Ok(AppliedStreamingSession {
        virtual_display,
        snapshot_saved,
        active_path_count,
    })
}

/// Convenience wrapper around [`apply_streaming_session_with_wait`] using production backoff.
pub fn apply_streaming_session<B: DisplayTopologyBackend>(
    backend: &B,
    virtual_luid: AdapterLuid,
    virtual_target_id: u32,
    request: &StreamingTopologyRequest,
    options: &SessionApplyOptions<'_>,
) -> Result<AppliedStreamingSession, ClientError> {
    apply_streaming_session_with_wait(
        backend,
        virtual_luid,
        virtual_target_id,
        request,
        options,
        production_wait_backoff,
    )
}

/// Restores the persisted physical display snapshot (optionally enabling attached physical displays if absent).
pub fn restore_streaming_session_with_fallback<B: DisplayTopologyBackend>(
    backend: &B,
    snapshot_path: &Path,
    virtual_target: Option<(AdapterLuid, u32)>,
    enable_physical_fallback: bool,
) -> Result<RestoredTopologyReport, ClientError> {
    let Some(snapshot) = load_snapshot(snapshot_path)? else {
        if !enable_physical_fallback {
            return Ok(RestoredTopologyReport {
                snapshot_found: false,
                restored_path_count: 0,
                skipped_unplugged_targets: Vec::new(),
            });
        }
        let current = backend.query_topology()?;
        let plan = plan_enable_attached_physical(&current, virtual_target)?;
        let restored_path_count = plan.desired_paths.iter().filter(|p| p.active).count();
        backend.apply_topology(&plan.desired_paths)?;
        return Ok(RestoredTopologyReport {
            snapshot_found: false,
            restored_path_count,
            skipped_unplugged_targets: plan.skipped_unplugged_targets,
        });
    };

    if !snapshot.paths.iter().any(|p| p.active) {
        let _ = remove_snapshot(snapshot_path)?;
        return Ok(RestoredTopologyReport {
            snapshot_found: true,
            restored_path_count: 0,
            skipped_unplugged_targets: Vec::new(),
        });
    }

    let current = backend.query_topology()?;
    let plan = match plan_restore_topology(&snapshot, &current, virtual_target) {
        Ok(plan) => plan,
        Err(ClientError::NoActivePhysicalDisplays) if !enable_physical_fallback => {
            // WHY: If all backed-up physical monitors were powered off or unplugged during the
            // session, leaving the snapshot on disk blocks headless-standby reconciliation.
            let skipped: Vec<(AdapterLuid, u32)> = snapshot
                .paths
                .iter()
                .filter(|p| {
                    p.active
                        && !virtual_target
                            .is_some_and(|(vl, vt)| p.adapter_luid == vl && p.target_id == vt)
                })
                .map(|p| (p.adapter_luid, p.target_id))
                .collect();
            let _ = remove_snapshot(snapshot_path)?;
            return Ok(RestoredTopologyReport {
                snapshot_found: true,
                restored_path_count: 0,
                skipped_unplugged_targets: skipped,
            });
        }
        Err(ClientError::NoActivePhysicalDisplays) => {
            plan_enable_attached_physical(&current, virtual_target)?
        }
        Err(err) => return Err(err),
    };
    let restored_path_count = plan.desired_paths.iter().filter(|p| p.active).count();

    backend.apply_topology(&plan.desired_paths)?;
    for setting in &plan.hdr_updates {
        backend.apply_hdr_setting(setting)?;
    }
    let _ = remove_snapshot(snapshot_path)?;

    Ok(RestoredTopologyReport {
        snapshot_found: true,
        restored_path_count,
        skipped_unplugged_targets: plan.skipped_unplugged_targets,
    })
}

/// Restores the persisted physical display snapshot and removes the backup file atomically.
pub fn restore_streaming_session<B: DisplayTopologyBackend>(
    backend: &B,
    snapshot_path: &Path,
    virtual_target: Option<(AdapterLuid, u32)>,
) -> Result<RestoredTopologyReport, ClientError> {
    restore_streaming_session_with_fallback(backend, snapshot_path, virtual_target, false)
}

/// Applies a resolution, refresh-rate, and optional HDR update using a caller-supplied wait hook.
#[allow(
    clippy::too_many_arguments,
    reason = "explicit mode fields and wait hooks avoid builder boilerplate"
)]
pub fn apply_display_mode_in_place_with_wait<B, W>(
    backend: &B,
    virtual_luid: AdapterLuid,
    virtual_target_id: u32,
    width: u32,
    height: u32,
    refresh_num: u32,
    refresh_den: u32,
    hdr_enabled: Option<bool>,
    retry_policy: RetryPolicy,
    wait: W,
) -> Result<DisplayPathRecord, ClientError>
where
    B: DisplayTopologyBackend,
    W: FnMut(u32, u64),
{
    let _ = resolve_virtual_display_with_wait(
        backend,
        virtual_luid,
        virtual_target_id,
        retry_policy,
        wait,
    )?;
    let current = backend.query_topology()?;
    let plan = plan_in_place_mode_update(
        &current,
        virtual_luid,
        virtual_target_id,
        width,
        height,
        refresh_num,
        refresh_den,
        hdr_enabled,
    )?;
    backend.apply_topology(&plan.desired_paths)?;
    for setting in &plan.hdr_updates {
        backend.apply_hdr_setting(setting)?;
    }

    let updated = backend.query_topology()?;
    updated
        .paths
        .into_iter()
        .find(|p| p.adapter_luid == virtual_luid && p.target_id == virtual_target_id)
        .or_else(|| {
            plan.desired_paths
                .into_iter()
                .find(|p| p.adapter_luid == virtual_luid && p.target_id == virtual_target_id)
        })
        .ok_or(ClientError::VirtualDisplayNotFound {
            adapter_luid_low: virtual_luid.low,
            adapter_luid_high: virtual_luid.high,
            target_id: virtual_target_id,
        })
}

/// Applies an in-place resolution, refresh-rate, and optional HDR update on a single virtual target.
#[allow(
    clippy::too_many_arguments,
    reason = "explicit mode fields avoid intermediate builder boilerplate"
)]
pub fn apply_display_mode_in_place<B: DisplayTopologyBackend>(
    backend: &B,
    virtual_luid: AdapterLuid,
    virtual_target_id: u32,
    width: u32,
    height: u32,
    refresh_num: u32,
    refresh_den: u32,
    hdr_enabled: Option<bool>,
) -> Result<DisplayPathRecord, ClientError> {
    apply_display_mode_in_place_with_wait(
        backend,
        virtual_luid,
        virtual_target_id,
        width,
        height,
        refresh_num,
        refresh_den,
        hdr_enabled,
        RetryPolicy::default(),
        production_wait_backoff,
    )
}

/// Bitmask combining `DISPLAYCONFIG_TARGET_FORCED_AVAILABILITY_BOOT | PATH | SYSTEM`.
pub const DISPLAYCONFIG_TARGET_FORCED_AVAILABILITY_MASK: u32 =
    0x0000_0004 | 0x0000_0008 | 0x0000_0010;

/// Returns `true` when `target_status_flags` marks an OS-forced/simulated display target.
pub const fn is_forced_availability_target(target_status_flags: u32) -> bool {
    (target_status_flags & DISPLAYCONFIG_TARGET_FORCED_AVAILABILITY_MASK) != 0
}

/// Returns `true` when a CCD path represents a live, non-simulated active display.
pub fn is_live_ccd_display(
    active: bool,
    target_available: bool,
    target_status_flags: u32,
    monitor_device_path: &str,
    in_qdc_only_active: bool,
) -> bool {
    active
        && target_available
        && in_qdc_only_active
        && !is_forced_availability_target(target_status_flags)
        && !monitor_device_path.is_empty()
}

/// Diagnostic snapshot of a CCD path reported as `DISPLAYCONFIG_PATH_ACTIVE`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CcdActivePathProbe {
    /// Adapter LUID owning the display target.
    pub adapter_luid: AdapterLuid,
    /// VidPN target identifier.
    pub target_id: u32,
    /// Raw `DISPLAYCONFIG_PATH_INFO.flags`.
    pub path_flags: u32,
    /// Raw `DISPLAYCONFIG_PATH_TARGET_INFO.targetAvailable`.
    pub target_available: bool,
    /// Raw `DISPLAYCONFIG_PATH_TARGET_INFO.statusFlags`.
    pub target_status_flags: u32,
    /// Raw `DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY` discriminant.
    pub output_technology: i32,
    /// Raw `DISPLAYCONFIG_TARGET_DEVICE_NAME.flags` bitfield value.
    pub name_flags: u32,
    /// Friendly monitor name reported by `DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME`.
    pub monitor_friendly_name: String,
    /// PnP monitor device interface path (`\\?\DISPLAY#...`), empty for simulated/ghost paths.
    pub monitor_device_path: String,
    /// Whether this target was also returned by `QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS)`.
    pub in_qdc_only_active: bool,
    /// Whether `target_status_flags` has any `DISPLAYCONFIG_TARGET_FORCED_AVAILABILITY_*` bit set.
    pub forced_availability: bool,
    /// Whether this path qualifies as a real, live active display for standby/topology decisions.
    pub is_live_display: bool,
}

/// Queries diagnostic metadata for every active CCD path in the current Windows session.
#[cfg(windows)]
pub fn query_active_path_probes() -> Result<Vec<CcdActivePathProbe>, ClientError> {
    win32::query_active_path_probes_win32()
}

/// Queries diagnostic metadata for every active CCD path in the current Windows session.
#[cfg(not(windows))]
pub fn query_active_path_probes() -> Result<Vec<CcdActivePathProbe>, ClientError> {
    Err(ClientError::Ccd {
        operation: "QueryDisplayConfig",
        status: 1,
    })
}

#[cfg(any(windows, test))]
const WIN32_ERROR_ACCESS_DENIED: i32 = 5;

#[cfg(any(windows, test))]
pub(crate) fn should_retry_on_input_desktop(err: &ClientError) -> bool {
    matches!(
        err,
        ClientError::Ccd { status, .. } if *status == WIN32_ERROR_ACCESS_DENIED
    )
}

#[cfg(windows)]
mod win32 {
    use core::mem::size_of;
    use std::collections::{BTreeMap, BTreeSet};

    use windows::core::BOOL;
    use windows::Win32::Devices::Display::{
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
        SetDisplayConfig, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
        DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE,
        DISPLAYCONFIG_MODE_INFO_TYPE_TARGET, DISPLAYCONFIG_PATH_INFO,
        DISPLAYCONFIG_PIXELFORMAT_32BPP, DISPLAYCONFIG_RATIONAL, DISPLAYCONFIG_ROTATION,
        DISPLAYCONFIG_ROTATION_IDENTITY, DISPLAYCONFIG_ROTATION_ROTATE180,
        DISPLAYCONFIG_ROTATION_ROTATE270, DISPLAYCONFIG_ROTATION_ROTATE90, DISPLAYCONFIG_SCALING,
        DISPLAYCONFIG_SCALING_ASPECTRATIOCENTEREDMAX, DISPLAYCONFIG_SCALING_CENTERED,
        DISPLAYCONFIG_SCALING_CUSTOM, DISPLAYCONFIG_SCALING_IDENTITY,
        DISPLAYCONFIG_SCALING_PREFERRED, DISPLAYCONFIG_SCALING_STRETCHED,
        DISPLAYCONFIG_SCANLINE_ORDERING, DISPLAYCONFIG_SCANLINE_ORDERING_INTERLACED,
        DISPLAYCONFIG_SCANLINE_ORDERING_INTERLACED_LOWERFIELDFIRST,
        DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE, DISPLAYCONFIG_SCANLINE_ORDERING_UNSPECIFIED,
        DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_SOURCE_MODE,
        DISPLAYCONFIG_TARGET_DEVICE_NAME, QDC_ALL_PATHS, QDC_ONLY_ACTIVE_PATHS,
        QUERY_DISPLAY_CONFIG_FLAGS, SDC_ALLOW_CHANGES, SDC_APPLY, SDC_SAVE_TO_DATABASE,
        SDC_USE_SUPPLIED_DISPLAY_CONFIG,
    };
    use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, LUID, POINTL};
    use windows::Win32::Graphics::Gdi::{
        DISPLAYCONFIG_PATH_ACTIVE, DISPLAYCONFIG_PATH_MODE_IDX_INVALID,
    };
    use windows::Win32::System::StationsAndDesktops::{
        CloseDesktop, OpenInputDesktop, SetThreadDesktop, DESKTOP_ACCESS_FLAGS,
        DESKTOP_CONTROL_FLAGS, HDESK,
    };

    const DESKTOP_MAXIMUM_ALLOWED: DESKTOP_ACCESS_FLAGS = DESKTOP_ACCESS_FLAGS(0x0200_0000);

    struct OwnedDesktop(HDESK);

    impl Drop for OwnedDesktop {
        fn drop(&mut self) {
            if !self.0.is_invalid() && !self.0 .0.is_null() {
                // SAFETY: `self.0` is an owned desktop handle returned by `OpenInputDesktop`.
                unsafe {
                    let _ = CloseDesktop(self.0);
                }
            }
        }
    }

    /// Runs `op` directly first, retrying on a fresh worker attached to the active input desktop on `ERROR_ACCESS_DENIED`.
    pub(crate) fn run_on_input_desktop_if_access_denied<T, F>(op: F) -> Result<T, ClientError>
    where
        F: Fn() -> Result<T, ClientError> + Send,
        T: Send,
    {
        let first_err = match op() {
            Ok(val) => return Ok(val),
            Err(err) => err,
        };
        if !super::should_retry_on_input_desktop(&first_err) {
            return Err(first_err);
        }

        // WHY: Processes launched from automation shells or non-interactive desktops inside Session > 0
        // receive ERROR_ACCESS_DENIED (5) from SetDisplayConfig unless attached to the active input desktop
        // on a clean thread with no windows or hooks.
        let retried = std::thread::scope(|s| {
            s.spawn(move || -> Option<Result<T, ClientError>> {
                // SAFETY: `OpenInputDesktop` takes primitive flags and returns a new desktop handle or error.
                let desk_raw = unsafe {
                    OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_MAXIMUM_ALLOWED)
                }
                .ok()?;
                let desk = OwnedDesktop(desk_raw);
                // SAFETY: This scoped thread was just spawned and owns no windows or hooks.
                if unsafe { SetThreadDesktop(desk.0) }.is_err() {
                    return None;
                }
                Some(op())
            })
            .join()
            .ok()
            .flatten()
        });

        retried.unwrap_or(Err(first_err))
    }

    use super::{
        is_forced_availability_target, is_live_ccd_display, CcdActivePathProbe,
        DisplayTopologyBackend,
    };
    use crate::error::ClientError;
    use crate::hdr::{
        apply_hdr_setting, query_display_color_state, query_os_version, HdrTargetSetting,
    };
    use crate::topology::{
        AdapterLuid, CcdTopologyState, DisplayPathRecord, DisplayRotation, DisplayScaling,
        DisplayScanlineOrdering,
    };

    /// Live Windows CCD implementation of [`DisplayTopologyBackend`].
    #[derive(Debug, Default, Clone, Copy)]
    pub struct WindowsCcdBackend;

    impl WindowsCcdBackend {
        /// Creates a new [`WindowsCcdBackend`].
        pub const fn new() -> Self {
            Self
        }
    }

    fn utf16_nul_to_string(buf: &[u16]) -> String {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len])
    }

    fn to_adapter_luid(luid: LUID) -> AdapterLuid {
        AdapterLuid::new(luid.LowPart, luid.HighPart)
    }

    fn to_win_luid(luid: AdapterLuid) -> LUID {
        LUID {
            LowPart: luid.low,
            HighPart: luid.high,
        }
    }

    fn from_win_scaling(scaling: DISPLAYCONFIG_SCALING) -> DisplayScaling {
        match scaling {
            DISPLAYCONFIG_SCALING_IDENTITY => DisplayScaling::Identity,
            DISPLAYCONFIG_SCALING_CENTERED => DisplayScaling::Centered,
            DISPLAYCONFIG_SCALING_STRETCHED => DisplayScaling::Stretched,
            DISPLAYCONFIG_SCALING_ASPECTRATIOCENTEREDMAX => DisplayScaling::AspectRatioCenteredMax,
            DISPLAYCONFIG_SCALING_CUSTOM => DisplayScaling::Custom,
            DISPLAYCONFIG_SCALING_PREFERRED => DisplayScaling::Preferred,
            other => DisplayScaling::Unknown(other.0),
        }
    }

    fn to_win_scaling(scaling: DisplayScaling) -> DISPLAYCONFIG_SCALING {
        match scaling {
            DisplayScaling::Identity => DISPLAYCONFIG_SCALING_IDENTITY,
            DisplayScaling::Centered => DISPLAYCONFIG_SCALING_CENTERED,
            DisplayScaling::Stretched => DISPLAYCONFIG_SCALING_STRETCHED,
            DisplayScaling::AspectRatioCenteredMax => DISPLAYCONFIG_SCALING_ASPECTRATIOCENTEREDMAX,
            DisplayScaling::Custom => DISPLAYCONFIG_SCALING_CUSTOM,
            DisplayScaling::Preferred => DISPLAYCONFIG_SCALING_PREFERRED,
            DisplayScaling::Unknown(raw) => DISPLAYCONFIG_SCALING(raw),
        }
    }

    fn from_win_rotation(rotation: DISPLAYCONFIG_ROTATION) -> DisplayRotation {
        match rotation {
            DISPLAYCONFIG_ROTATION_IDENTITY => DisplayRotation::Identity,
            DISPLAYCONFIG_ROTATION_ROTATE90 => DisplayRotation::Rotate90,
            DISPLAYCONFIG_ROTATION_ROTATE180 => DisplayRotation::Rotate180,
            DISPLAYCONFIG_ROTATION_ROTATE270 => DisplayRotation::Rotate270,
            other => DisplayRotation::Unknown(other.0),
        }
    }

    fn to_win_rotation(rotation: DisplayRotation) -> DISPLAYCONFIG_ROTATION {
        match rotation {
            DisplayRotation::Identity => DISPLAYCONFIG_ROTATION_IDENTITY,
            DisplayRotation::Rotate90 => DISPLAYCONFIG_ROTATION_ROTATE90,
            DisplayRotation::Rotate180 => DISPLAYCONFIG_ROTATION_ROTATE180,
            DisplayRotation::Rotate270 => DISPLAYCONFIG_ROTATION_ROTATE270,
            DisplayRotation::Unknown(raw) => DISPLAYCONFIG_ROTATION(raw),
        }
    }

    fn from_win_scanline(ordering: DISPLAYCONFIG_SCANLINE_ORDERING) -> DisplayScanlineOrdering {
        match ordering {
            DISPLAYCONFIG_SCANLINE_ORDERING_UNSPECIFIED => DisplayScanlineOrdering::Unspecified,
            DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE => DisplayScanlineOrdering::Progressive,
            DISPLAYCONFIG_SCANLINE_ORDERING_INTERLACED => DisplayScanlineOrdering::Interlaced,
            DISPLAYCONFIG_SCANLINE_ORDERING_INTERLACED_LOWERFIELDFIRST => {
                DisplayScanlineOrdering::InterlacedLowerFieldFirst
            }
            other => DisplayScanlineOrdering::Unknown(other.0),
        }
    }

    fn to_win_scanline(ordering: DisplayScanlineOrdering) -> DISPLAYCONFIG_SCANLINE_ORDERING {
        match ordering {
            DisplayScanlineOrdering::Unspecified => DISPLAYCONFIG_SCANLINE_ORDERING_UNSPECIFIED,
            DisplayScanlineOrdering::Progressive => DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE,
            DisplayScanlineOrdering::Interlaced => DISPLAYCONFIG_SCANLINE_ORDERING_INTERLACED,
            DisplayScanlineOrdering::InterlacedLowerFieldFirst => {
                DISPLAYCONFIG_SCANLINE_ORDERING_INTERLACED_LOWERFIELDFIRST
            }
            DisplayScanlineOrdering::Unknown(raw) => DISPLAYCONFIG_SCANLINE_ORDERING(raw),
        }
    }

    fn query_source_gdi_name(adapter_id: LUID, source_id: u32) -> String {
        let mut pkt = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                adapterId: adapter_id,
                id: source_id,
            },
            ..Default::default()
        };
        // SAFETY: `pkt` is stack-allocated and initialized with exact `header.size`.
        let status = unsafe { DisplayConfigGetDeviceInfo(&mut pkt.header) };
        if status as u32 == ERROR_SUCCESS.0 {
            utf16_nul_to_string(&pkt.viewGdiDeviceName)
        } else {
            String::new()
        }
    }

    struct TargetDeviceInfo {
        ok: bool,
        friendly_name: String,
        device_path: String,
        name_flags: u32,
    }

    fn query_target_device_info(adapter_id: LUID, target_id: u32) -> TargetDeviceInfo {
        let mut pkt = DISPLAYCONFIG_TARGET_DEVICE_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                size: size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
                adapterId: adapter_id,
                id: target_id,
            },
            ..Default::default()
        };
        // SAFETY: `pkt` is stack-allocated and initialized with exact `header.size`.
        let status = unsafe { DisplayConfigGetDeviceInfo(&mut pkt.header) };
        if status as u32 == ERROR_SUCCESS.0 {
            // SAFETY: `value` is a `u32` view of the `DISPLAYCONFIG_TARGET_DEVICE_NAME_FLAGS_0` union.
            let name_flags = unsafe { pkt.flags.Anonymous.value };
            TargetDeviceInfo {
                ok: true,
                friendly_name: utf16_nul_to_string(&pkt.monitorFriendlyDeviceName),
                device_path: utf16_nul_to_string(&pkt.monitorDevicePath),
                name_flags,
            }
        } else {
            TargetDeviceInfo {
                ok: false,
                friendly_name: String::new(),
                device_path: String::new(),
                name_flags: 0,
            }
        }
    }

    fn query_paths_and_modes_with_flags(
        flags: QUERY_DISPLAY_CONFIG_FLAGS,
    ) -> Result<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>), ClientError> {
        // WHY: Topology changes can resize buffer counts between GetDisplayConfigBufferSizes
        // and QueryDisplayConfig, so Windows explicitly recommends retrying on ERROR_INSUFFICIENT_BUFFER.
        for _ in 0..4 {
            let mut path_count: u32 = 0;
            let mut mode_count: u32 = 0;
            // SAFETY: Output pointers refer to initialized stack locals.
            let size_err =
                unsafe { GetDisplayConfigBufferSizes(flags, &mut path_count, &mut mode_count) };
            if size_err != ERROR_SUCCESS {
                return Err(ClientError::Ccd {
                    operation: "GetDisplayConfigBufferSizes",
                    status: size_err.0 as i32,
                });
            }

            let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
            // SAFETY: `paths` and `modes` match `path_count` and `mode_count` capacity.
            let query_err = unsafe {
                QueryDisplayConfig(
                    flags,
                    &mut path_count,
                    paths.as_mut_ptr(),
                    &mut mode_count,
                    modes.as_mut_ptr(),
                    None,
                )
            };
            if query_err == ERROR_SUCCESS {
                paths.truncate(path_count as usize);
                modes.truncate(mode_count as usize);
                return Ok((paths, modes));
            }
            if query_err != ERROR_INSUFFICIENT_BUFFER {
                return Err(ClientError::Ccd {
                    operation: "QueryDisplayConfig",
                    status: query_err.0 as i32,
                });
            }
        }

        Err(ClientError::Ccd {
            operation: "QueryDisplayConfig",
            status: ERROR_INSUFFICIENT_BUFFER.0 as i32,
        })
    }

    struct RawCcdSnapshot {
        all_paths: Vec<DISPLAYCONFIG_PATH_INFO>,
        all_modes: Vec<DISPLAYCONFIG_MODE_INFO>,
        only_active_targets: BTreeSet<(AdapterLuid, u32)>,
    }

    fn query_all_and_only_active_paths() -> Result<RawCcdSnapshot, ClientError> {
        let (all_paths, all_modes) = query_paths_and_modes_with_flags(QDC_ALL_PATHS)?;
        let (active_paths, _) = query_paths_and_modes_with_flags(QDC_ONLY_ACTIVE_PATHS)?;
        let only_active_targets = active_paths
            .iter()
            .filter(|p| {
                (p.flags & DISPLAYCONFIG_PATH_ACTIVE) != 0
                    && !is_forced_availability_target(p.targetInfo.statusFlags)
            })
            .map(|p| (to_adapter_luid(p.targetInfo.adapterId), p.targetInfo.id))
            .collect();
        Ok(RawCcdSnapshot {
            all_paths,
            all_modes,
            only_active_targets,
        })
    }

    pub(super) fn query_active_path_probes_win32() -> Result<Vec<CcdActivePathProbe>, ClientError> {
        let RawCcdSnapshot {
            all_paths: raw_paths,
            only_active_targets,
            ..
        } = run_on_input_desktop_if_access_denied(query_all_and_only_active_paths)?;
        let mut seen = BTreeSet::new();
        let mut probes = Vec::new();
        for path in &raw_paths {
            let raw_active = (path.flags & DISPLAYCONFIG_PATH_ACTIVE) != 0;
            if !raw_active {
                continue;
            }
            let luid = to_adapter_luid(path.targetInfo.adapterId);
            let target_id = path.targetInfo.id;
            if !seen.insert((luid, target_id)) {
                continue;
            }
            let target_available = path.targetInfo.targetAvailable.as_bool();
            let target_status_flags = path.targetInfo.statusFlags;
            let info = query_target_device_info(path.targetInfo.adapterId, target_id);
            let in_qdc_only_active = only_active_targets.contains(&(luid, target_id));
            let forced_availability = is_forced_availability_target(target_status_flags);
            let is_live_display = is_live_ccd_display(
                raw_active,
                target_available,
                target_status_flags,
                &info.device_path,
                in_qdc_only_active,
            );
            probes.push(CcdActivePathProbe {
                adapter_luid: luid,
                target_id,
                path_flags: path.flags,
                target_available,
                target_status_flags,
                output_technology: path.targetInfo.outputTechnology.0,
                name_flags: info.name_flags,
                monitor_friendly_name: info.friendly_name,
                monitor_device_path: info.device_path,
                in_qdc_only_active,
                forced_availability,
                is_live_display,
            });
        }
        Ok(probes)
    }

    impl DisplayTopologyBackend for WindowsCcdBackend {
        fn query_topology(&self) -> Result<CcdTopologyState, ClientError> {
            let os_build = query_os_version()?.build;
            let RawCcdSnapshot {
                all_paths: raw_paths,
                all_modes: raw_modes,
                only_active_targets,
            } = run_on_input_desktop_if_access_denied(query_all_and_only_active_paths)?;

            let mut active_sources_by_luid: BTreeMap<AdapterLuid, BTreeSet<u32>> = BTreeMap::new();
            for path in &raw_paths {
                let target_luid = to_adapter_luid(path.targetInfo.adapterId);
                let is_live_active = (path.flags & DISPLAYCONFIG_PATH_ACTIVE) != 0
                    && !is_forced_availability_target(path.targetInfo.statusFlags)
                    && only_active_targets.contains(&(target_luid, path.targetInfo.id));
                if is_live_active {
                    active_sources_by_luid
                        .entry(to_adapter_luid(path.sourceInfo.adapterId))
                        .or_default()
                        .insert(path.sourceInfo.id);
                }
            }

            type PathRank = (bool, bool, bool);
            let mut best_paths: BTreeMap<(AdapterLuid, u32), (PathRank, &DISPLAYCONFIG_PATH_INFO)> =
                BTreeMap::new();
            for path in &raw_paths {
                let luid = to_adapter_luid(path.targetInfo.adapterId);
                let target_id = path.targetInfo.id;
                let active = (path.flags & DISPLAYCONFIG_PATH_ACTIVE) != 0;
                let target_available = path.targetInfo.targetAvailable.as_bool();
                let source_luid = to_adapter_luid(path.sourceInfo.adapterId);
                // WHY: QDC_ALL_PATHS emits one candidate per VidPN source for inactive targets;
                // preferring a source_id not occupied by an active path avoids collisions on Extend.
                let source_free = active
                    || !active_sources_by_luid
                        .get(&source_luid)
                        .is_some_and(|used| used.contains(&path.sourceInfo.id));
                let rank = (active, target_available, source_free);

                let key = (luid, target_id);
                let replace = best_paths
                    .get(&key)
                    .is_none_or(|(existing_rank, _)| rank > *existing_rank);
                if replace {
                    best_paths.insert(key, (rank, path));
                }
            }

            let mut records = Vec::with_capacity(best_paths.len());
            for ((luid, target_id), (_, path)) in best_paths {
                let raw_active = (path.flags & DISPLAYCONFIG_PATH_ACTIVE) != 0;
                let raw_target_available = path.targetInfo.targetAvailable.as_bool();
                let target_info = query_target_device_info(path.targetInfo.adapterId, target_id);
                let forced_availability =
                    is_forced_availability_target(path.targetInfo.statusFlags);
                let in_qdc_only_active = only_active_targets.contains(&(luid, target_id));
                // WHY: Windows CCD can retain OS-forced/simulated paths with `DISPLAYCONFIG_PATH_ACTIVE`
                // and `targetAvailable=TRUE` after the last real monitor departs; filter those out so
                // headless standby and topology snapshots never mistake simulated paths for live monitors.
                let active = is_live_ccd_display(
                    raw_active,
                    raw_target_available,
                    path.targetInfo.statusFlags,
                    &target_info.device_path,
                    in_qdc_only_active,
                );
                let target_available = raw_target_available
                    && !forced_availability
                    && (!target_info.ok || !target_info.device_path.is_empty());

                // SAFETY: `modeInfoIdx` is a `u32` alternative in the `DISPLAYCONFIG_PATH_SOURCE_INFO_0` union.
                let source_mode_idx = unsafe { path.sourceInfo.Anonymous.modeInfoIdx };
                let (width, height, origin_x, origin_y) =
                    if active && source_mode_idx != DISPLAYCONFIG_PATH_MODE_IDX_INVALID {
                        if let Some(mode) = raw_modes.get(source_mode_idx as usize) {
                            if mode.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE {
                                // SAFETY: `infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE` guarantees `sourceMode` is active.
                                let src = unsafe { mode.Anonymous.sourceMode };
                                (src.width, src.height, src.position.x, src.position.y)
                            } else {
                                (0, 0, 0, 0)
                            }
                        } else {
                            (0, 0, 0, 0)
                        }
                    } else {
                        (0, 0, 0, 0)
                    };

                // SAFETY: `modeInfoIdx` is a `u32` alternative in `DISPLAYCONFIG_PATH_TARGET_INFO_0`.
                let target_mode_idx = unsafe { path.targetInfo.Anonymous.modeInfoIdx };
                let (mut refresh_num, mut refresh_den) = (
                    path.targetInfo.refreshRate.Numerator,
                    path.targetInfo.refreshRate.Denominator,
                );
                if (refresh_num == 0 || refresh_den == 0)
                    && active
                    && target_mode_idx != DISPLAYCONFIG_PATH_MODE_IDX_INVALID
                {
                    if let Some(mode) = raw_modes.get(target_mode_idx as usize) {
                        if mode.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_TARGET {
                            // SAFETY: `infoType == DISPLAYCONFIG_MODE_INFO_TYPE_TARGET` guarantees `targetMode` is active.
                            let tgt = unsafe { mode.Anonymous.targetMode };
                            refresh_num = tgt.targetVideoSignalInfo.vSyncFreq.Numerator;
                            refresh_den = tgt.targetVideoSignalInfo.vSyncFreq.Denominator;
                        }
                    }
                }

                let gdi_name = query_source_gdi_name(path.sourceInfo.adapterId, path.sourceInfo.id);
                let monitor_friendly_name = target_info.friendly_name;
                let hdr_state = if active && target_available {
                    query_display_color_state(luid, target_id, os_build).ok()
                } else {
                    None
                };

                records.push(DisplayPathRecord {
                    adapter_luid: luid,
                    target_id,
                    source_id: path.sourceInfo.id,
                    gdi_name,
                    monitor_friendly_name,
                    active,
                    target_available,
                    width,
                    height,
                    origin_x,
                    origin_y,
                    refresh_num,
                    refresh_den,
                    scaling: from_win_scaling(path.targetInfo.scaling),
                    rotation: from_win_rotation(path.targetInfo.rotation),
                    scanline_ordering: from_win_scanline(path.targetInfo.scanLineOrdering),
                    hdr_state,
                });
            }

            Ok(CcdTopologyState {
                os_build,
                paths: records,
            })
        }

        fn apply_topology(&self, desired_paths: &[DisplayPathRecord]) -> Result<(), ClientError> {
            let mut win_paths = Vec::new();
            let mut win_modes = Vec::new();

            for rec in desired_paths.iter().filter(|p| p.active) {
                let source_mode_idx = u32::try_from(win_modes.len()).map_err(|_| {
                    ClientError::InvalidInput("too many CCD display modes".to_string())
                })?;
                let win_luid = to_win_luid(rec.adapter_luid);

                let mut source_mode_info = DISPLAYCONFIG_MODE_INFO {
                    infoType: DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE,
                    id: rec.source_id,
                    adapterId: win_luid,
                    ..Default::default()
                };
                source_mode_info.Anonymous.sourceMode = DISPLAYCONFIG_SOURCE_MODE {
                    width: rec.width,
                    height: rec.height,
                    pixelFormat: DISPLAYCONFIG_PIXELFORMAT_32BPP,
                    position: POINTL {
                        x: rec.origin_x,
                        y: rec.origin_y,
                    },
                };
                win_modes.push(source_mode_info);

                let mut path = DISPLAYCONFIG_PATH_INFO::default();
                path.sourceInfo.adapterId = win_luid;
                path.sourceInfo.id = rec.source_id;
                path.sourceInfo.Anonymous.modeInfoIdx = source_mode_idx;

                path.targetInfo.adapterId = win_luid;
                path.targetInfo.id = rec.target_id;
                path.targetInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
                path.targetInfo.rotation = to_win_rotation(rec.rotation);
                path.targetInfo.scaling = to_win_scaling(rec.scaling);
                path.targetInfo.refreshRate = DISPLAYCONFIG_RATIONAL {
                    Numerator: rec.refresh_num,
                    Denominator: rec.refresh_den,
                };
                path.targetInfo.scanLineOrdering = to_win_scanline(rec.scanline_ordering);
                path.targetInfo.targetAvailable = BOOL(1);
                path.flags = DISPLAYCONFIG_PATH_ACTIVE;

                win_paths.push(path);
            }

            if win_paths.is_empty() {
                return Err(ClientError::InvalidInput(
                    "cannot apply an empty active display topology".to_string(),
                ));
            }

            run_on_input_desktop_if_access_denied(|| {
                // SAFETY: `win_paths` and `win_modes` hold valid `#[repr(C)]` CCD descriptors with
                // consistent `sourceInfo.Anonymous.modeInfoIdx` indices into `win_modes`.
                let status = unsafe {
                    SetDisplayConfig(
                        Some(&win_paths),
                        Some(&win_modes),
                        SDC_APPLY
                            | SDC_USE_SUPPLIED_DISPLAY_CONFIG
                            | SDC_ALLOW_CHANGES
                            | SDC_SAVE_TO_DATABASE,
                    )
                };
                if status as u32 != ERROR_SUCCESS.0 {
                    return Err(ClientError::Ccd {
                        operation: "SetDisplayConfig",
                        status,
                    });
                }
                Ok(())
            })
        }

        fn apply_hdr_setting(&self, setting: &HdrTargetSetting) -> Result<(), ClientError> {
            let os_build = query_os_version()?.build;
            apply_hdr_setting(setting, os_build)
        }
    }
}

#[cfg(windows)]
pub(crate) use win32::run_on_input_desktop_if_access_denied;
#[cfg(windows)]
pub use win32::WindowsCcdBackend;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_retry_on_input_desktop_matches_only_access_denied_ccd() {
        assert!(should_retry_on_input_desktop(&ClientError::Ccd {
            operation: "SetDisplayConfig",
            status: 5,
        }));
        assert!(!should_retry_on_input_desktop(&ClientError::Ccd {
            operation: "SetDisplayConfig",
            status: 87,
        }));
        assert!(!should_retry_on_input_desktop(&ClientError::AccessDenied));
    }
}
