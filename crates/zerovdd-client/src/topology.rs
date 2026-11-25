//! Portable display topology model, atomic JSON snapshot persistence, and topology planner.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use zerovdd_config::{default_config_dir, SunshineTopologyMode};

use crate::error::ClientError;
use crate::hdr::{DisplayColorState, HdrTargetSetting};

/// Current JSON schema version for persisted [`TopologySnapshot`] backups.
pub const TOPOLOGY_SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// Filename used when persisting topology backup snapshots in the config directory.
pub const TOPOLOGY_BACKUP_FILENAME: &str = "topology-backup.json";

/// Portable representation of a Windows `LUID` (`LowPart`, `HighPart`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AdapterLuid {
    /// Low 32 bits of the adapter LUID.
    pub low: u32,
    /// High 32 bits of the adapter LUID.
    pub high: i32,
}

impl AdapterLuid {
    /// Constructs an [`AdapterLuid`] from raw Windows `LUID` parts.
    pub const fn new(low: u32, high: i32) -> Self {
        Self { low, high }
    }

    /// Returns `true` if both `low` and `high` are zero.
    pub const fn is_zero(self) -> bool {
        self.low == 0 && self.high == 0
    }
}

/// CCD scaling mode associated with a display path target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayScaling {
    /// 1:1 pixel mapping (`DISPLAYCONFIG_SCALING_IDENTITY`).
    #[default]
    Identity,
    /// Centered inside native panel (`DISPLAYCONFIG_SCALING_CENTERED`).
    Centered,
    /// Stretched to fill panel (`DISPLAYCONFIG_SCALING_STRETCHED`).
    Stretched,
    /// Aspect-ratio preserved scaling (`DISPLAYCONFIG_SCALING_ASPECTRATIOCENTEREDMAX`).
    AspectRatioCenteredMax,
    /// Driver/custom scaling (`DISPLAYCONFIG_SCALING_CUSTOM`).
    Custom,
    /// OS-preferred scaling (`DISPLAYCONFIG_SCALING_PREFERRED`).
    Preferred,
    /// Unrecognized numeric `DISPLAYCONFIG_SCALING` value preserved verbatim.
    Unknown(i32),
}

/// CCD rotation orientation associated with a display path target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayRotation {
    /// Landscape / unrotated (`DISPLAYCONFIG_ROTATION_IDENTITY`).
    #[default]
    Identity,
    /// Rotated 90 degrees clockwise (`DISPLAYCONFIG_ROTATION_ROTATE90`).
    Rotate90,
    /// Rotated 180 degrees (`DISPLAYCONFIG_ROTATION_ROTATE180`).
    Rotate180,
    /// Rotated 270 degrees clockwise (`DISPLAYCONFIG_ROTATION_ROTATE270`).
    Rotate270,
    /// Unrecognized numeric `DISPLAYCONFIG_ROTATION` value preserved verbatim.
    Unknown(i32),
}

impl DisplayRotation {
    /// Returns `true` when rotation swaps desktop horizontal and vertical axes (`90` or `270` deg).
    pub const fn is_transposed(self) -> bool {
        matches!(self, Self::Rotate90 | Self::Rotate270)
    }

    /// Converts clockwise rotation degrees (`0`, `90`, `180`, `270`) into [`DisplayRotation`].
    pub const fn from_degrees(deg: u16) -> Option<Self> {
        match deg {
            0 => Some(Self::Identity),
            90 => Some(Self::Rotate90),
            180 => Some(Self::Rotate180),
            270 => Some(Self::Rotate270),
            _ => None,
        }
    }
}

/// CCD scanline ordering associated with a display path target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayScanlineOrdering {
    /// Unspecified scanline ordering (`DISPLAYCONFIG_SCANLINE_ORDERING_UNSPECIFIED`).
    Unspecified,
    /// Progressive scan (`DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE`).
    #[default]
    Progressive,
    /// Interlaced upper-field first (`DISPLAYCONFIG_SCANLINE_ORDERING_INTERLACED`).
    Interlaced,
    /// Interlaced lower-field first (`DISPLAYCONFIG_SCANLINE_ORDERING_INTERLACED_LOWERFIELDFIRST`).
    InterlacedLowerFieldFirst,
    /// Unrecognized numeric `DISPLAYCONFIG_SCANLINE_ORDERING` value preserved verbatim.
    Unknown(i32),
}

const fn default_true() -> bool {
    true
}

/// Portable snapshot of a single Windows CCD source-to-target display path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayPathRecord {
    /// Adapter LUID owning the target.
    pub adapter_luid: AdapterLuid,
    /// OS VidPN target identifier.
    pub target_id: u32,
    /// OS VidPN source identifier.
    pub source_id: u32,
    /// GDI view device name (e.g. `\\.\DISPLAY1`).
    #[serde(default)]
    pub gdi_name: String,
    /// Human-readable monitor friendly device name reported by CCD.
    #[serde(default)]
    pub monitor_friendly_name: String,
    /// Whether this display path is currently active in the desktop topology.
    pub active: bool,
    /// Whether the target monitor is physically/virtually attached (`targetAvailable`).
    #[serde(default = "default_true")]
    pub target_available: bool,
    /// Active desktop source width in pixels.
    pub width: u32,
    /// Active desktop source height in pixels.
    pub height: u32,
    /// Desktop horizontal origin in pixels (`(0, 0)` designates the primary monitor).
    pub origin_x: i32,
    /// Desktop vertical origin in pixels (`(0, 0)` designates the primary monitor).
    pub origin_y: i32,
    /// Rational refresh rate numerator (e.g. `60000` for `59.94 Hz`).
    pub refresh_num: u32,
    /// Rational refresh rate denominator (e.g. `1001` for `59.94 Hz`).
    pub refresh_den: u32,
    /// CCD scaling mode.
    #[serde(default)]
    pub scaling: DisplayScaling,
    /// CCD rotation orientation.
    #[serde(default)]
    pub rotation: DisplayRotation,
    /// CCD scanline ordering.
    #[serde(default)]
    pub scanline_ordering: DisplayScanlineOrdering,
    /// Per-display HDR and Auto Color Management state, when available.
    #[serde(default)]
    pub hdr_state: Option<DisplayColorState>,
}

/// Captured Windows CCD topology state across all known display paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CcdTopologyState {
    /// OS build number (e.g. `26100` for Windows 11 24H2, `22631` for 23H2).
    pub os_build: u32,
    /// Display path records returned by CCD enumeration.
    pub paths: Vec<DisplayPathRecord>,
}

/// Per-display HDR/WCG entry stored inside [`TopologySnapshot`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayHdrSnapshotRecord {
    /// Adapter LUID owning the target.
    pub adapter_luid: AdapterLuid,
    /// OS VidPN target identifier.
    pub target_id: u32,
    /// Captured HDR and WCG/ACM color state.
    pub state: DisplayColorState,
}

/// Versioned backup of the user's physical display topology prior to a streaming session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologySnapshot {
    /// Backup format schema version (must equal [`TOPOLOGY_SNAPSHOT_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// Snapshot creation timestamp in milliseconds since the Unix epoch.
    pub created_at_unix_ms: u64,
    /// Physical display path records captured before streaming topology changes.
    pub paths: Vec<DisplayPathRecord>,
    /// Per-display HDR and WCG/ACM states indexed by `(adapter_luid, target_id)`.
    #[serde(default)]
    pub hdr_states: Vec<DisplayHdrSnapshotRecord>,
}

/// Parameters describing a desired virtual streaming display topology.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StreamingTopologyRequest {
    /// Isolate (sole virtual display) or Extend (alongside physical displays).
    pub mode: SunshineTopologyMode,
    /// Desired active virtual display width in pixels (unrotated target coordinates).
    pub width: u32,
    /// Desired active virtual display height in pixels (unrotated target coordinates).
    pub height: u32,
    /// Desired rational refresh rate numerator.
    pub refresh_num: u32,
    /// Desired rational refresh rate denominator.
    pub refresh_den: u32,
    /// Whether HDR should be enabled on the virtual display.
    pub hdr_enabled: bool,
    /// Optional explicit origin when extending; `None` places it beside active physical displays.
    pub extend_origin: Option<(i32, i32)>,
    /// When extending, whether the virtual display should be positioned as `(0, 0)` primary.
    pub make_virtual_primary: bool,
    /// Clockwise display rotation orientation applied by CCD.
    pub rotation: DisplayRotation,
}

/// Output of the pure topology planner ready to apply via a [`crate::ccd::DisplayTopologyBackend`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedTopology {
    /// Active display paths (plus any explicit inactive records) to apply.
    pub desired_paths: Vec<DisplayPathRecord>,
    /// Ordered HDR/WCG updates to apply after `SetDisplayConfig` succeeds.
    pub hdr_updates: Vec<HdrTargetSetting>,
    /// Physical monitors present in a snapshot that were skipped because they are unplugged.
    pub skipped_unplugged_targets: Vec<(AdapterLuid, u32)>,
}

/// Returns the default backup path `%ProgramData%\zerovdd\topology-backup.json` (or XDG equivalent).
pub fn default_snapshot_path() -> PathBuf {
    default_config_dir().join(TOPOLOGY_BACKUP_FILENAME)
}

/// Captures a [`TopologySnapshot`] from `state`, optionally excluding the virtual display target.
pub fn snapshot_from_state(
    state: &CcdTopologyState,
    created_at_unix_ms: u64,
    exclude_virtual: Option<(AdapterLuid, u32)>,
) -> TopologySnapshot {
    let mut paths = Vec::new();
    let mut hdr_states = Vec::new();
    let mut seen_hdr = BTreeSet::new();

    for path in &state.paths {
        if let Some((vl, vt)) = exclude_virtual {
            if path.adapter_luid == vl && path.target_id == vt {
                continue;
            }
        }
        paths.push(path.clone());
        if path.active {
            if let Some(color) = path.hdr_state {
                if seen_hdr.insert((path.adapter_luid, path.target_id)) {
                    hdr_states.push(DisplayHdrSnapshotRecord {
                        adapter_luid: path.adapter_luid,
                        target_id: path.target_id,
                        state: color,
                    });
                }
            }
        }
    }

    TopologySnapshot {
        schema_version: TOPOLOGY_SNAPSHOT_SCHEMA_VERSION,
        created_at_unix_ms,
        paths,
        hdr_states,
    }
}

/// Validates and atomically persists `snapshot` to `path` using a sibling temp file + rename.
pub fn save_snapshot_atomic(path: &Path, snapshot: &TopologySnapshot) -> Result<(), ClientError> {
    if snapshot.schema_version != TOPOLOGY_SNAPSHOT_SCHEMA_VERSION {
        return Err(ClientError::SnapshotCorrupt(format!(
            "unsupported schema_version {} (expected {TOPOLOGY_SNAPSHOT_SCHEMA_VERSION})",
            snapshot.schema_version
        )));
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            ClientError::InvalidInput(format!("invalid snapshot path {}", path.display()))
        })?;

    fs::create_dir_all(parent)?;
    let encoded = serde_json::to_string_pretty(snapshot)?;

    // WHY: Placing the temporary file inside `parent` ensures `persist()` is an atomic rename on
    // the same volume so an unexpected crash never leaves a half-written backup JSON on disk.
    let mut temp = NamedTempFile::new_in(parent)?;
    temp.write_all(encoded.as_bytes())?;
    temp.flush()?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|err| ClientError::Io(err.error))?;
    Ok(())
}

/// Loads and validates a [`TopologySnapshot`] from `path`, returning `Ok(None)` if absent.
pub fn load_snapshot(path: &Path) -> Result<Option<TopologySnapshot>, ClientError> {
    let raw = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(ClientError::Io(err)),
    };
    let snapshot: TopologySnapshot = serde_json::from_str(&raw)?;
    if snapshot.schema_version != TOPOLOGY_SNAPSHOT_SCHEMA_VERSION {
        return Err(ClientError::SnapshotCorrupt(format!(
            "unsupported schema_version {} (expected {TOPOLOGY_SNAPSHOT_SCHEMA_VERSION})",
            snapshot.schema_version
        )));
    }
    Ok(Some(snapshot))
}

/// Atomically removes a persisted snapshot at `path`, returning `Ok(false)` if already absent.
pub fn remove_snapshot(path: &Path) -> Result<bool, ClientError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => Err(ClientError::Io(err)),
    }
}

/// Plans a streaming desktop topology (`Isolate` or `Extend`) for an attached virtual display.
pub fn plan_streaming_topology(
    current: &CcdTopologyState,
    virtual_luid: AdapterLuid,
    virtual_target_id: u32,
    request: &StreamingTopologyRequest,
) -> Result<PlannedTopology, ClientError> {
    if request.width == 0
        || request.height == 0
        || request.refresh_num == 0
        || request.refresh_den == 0
    {
        return Err(ClientError::InvalidInput(
            "virtual display resolution and refresh rate must be non-zero".to_string(),
        ));
    }

    let virtual_base = current
        .paths
        .iter()
        .find(|p| p.adapter_luid == virtual_luid && p.target_id == virtual_target_id)
        .ok_or(ClientError::VirtualDisplayNotFound {
            adapter_luid_low: virtual_luid.low,
            adapter_luid_high: virtual_luid.high,
            target_id: virtual_target_id,
        })?;

    let (desktop_w, desktop_h) = if request.rotation.is_transposed() {
        (request.height, request.width)
    } else {
        (request.width, request.height)
    };

    let mut desired_paths = Vec::new();
    let mut hdr_updates = Vec::new();

    match request.mode {
        SunshineTopologyMode::Isolate => {
            let mut virt = virtual_base.clone();
            virt.active = true;
            virt.width = desktop_w;
            virt.height = desktop_h;
            virt.origin_x = 0;
            virt.origin_y = 0;
            virt.refresh_num = request.refresh_num;
            virt.refresh_den = request.refresh_den;
            virt.scaling = DisplayScaling::Identity;
            virt.rotation = request.rotation;
            virt.scanline_ordering = DisplayScanlineOrdering::Progressive;
            desired_paths.push(virt);

            for path in &current.paths {
                if path.adapter_luid == virtual_luid && path.target_id == virtual_target_id {
                    continue;
                }
                let mut disabled = path.clone();
                disabled.active = false;
                desired_paths.push(disabled);
            }
        }
        SunshineTopologyMode::Extend => {
            let mut active_physical: Vec<DisplayPathRecord> = current
                .paths
                .iter()
                .filter(|p| {
                    p.active
                        && !(p.adapter_luid == virtual_luid && p.target_id == virtual_target_id)
                })
                .cloned()
                .collect();

            let (vx, vy) = if active_physical.is_empty() {
                (0, 0)
            } else if request.make_virtual_primary {
                if let Some((ox, oy)) = request.extend_origin {
                    // WHY: When both an explicit origin and `--primary` are given, translate
                    // the whole desktop uniformly so the virtual display anchors at `(0, 0)`.
                    for phys in &mut active_physical {
                        phys.origin_x = phys.origin_x.saturating_sub(ox);
                        phys.origin_y = phys.origin_y.saturating_sub(oy);
                    }
                    (0, 0)
                } else {
                    // WHY: Monitors wholly left of `x = 0` cannot overlap `[0, desktop_w)` and
                    // stay anchored, while monitors intersecting `x > 0` shift rightwards together.
                    let min_right_side_x = active_physical
                        .iter()
                        .filter(|p| i64::from(p.origin_x) + i64::from(p.width) > 0)
                        .map(|p| i64::from(p.origin_x))
                        .min();
                    if let Some(min_x) = min_right_side_x {
                        let shift = (i64::from(desktop_w) - min_x).max(0);
                        let shift_i32 = i32::try_from(shift).unwrap_or(i32::MAX);
                        if shift_i32 > 0 {
                            for phys in &mut active_physical {
                                if i64::from(phys.origin_x) + i64::from(phys.width) > 0 {
                                    phys.origin_x = phys.origin_x.saturating_add(shift_i32);
                                }
                            }
                        }
                    }
                    (0, 0)
                }
            } else if let Some((ox, oy)) = request.extend_origin {
                (ox, oy)
            } else {
                let max_right = active_physical
                    .iter()
                    .map(|p| {
                        p.origin_x
                            .saturating_add(i32::try_from(p.width).unwrap_or(0))
                    })
                    .max()
                    .unwrap_or(0);
                (max_right, 0)
            };

            let used_sources_on_luid: BTreeSet<u32> = active_physical
                .iter()
                .filter(|p| p.adapter_luid == virtual_luid)
                .map(|p| p.source_id)
                .collect();

            let mut virt = virtual_base.clone();
            if used_sources_on_luid.contains(&virt.source_id) {
                if let Some(free_source) = (0u32..).find(|id| !used_sources_on_luid.contains(id)) {
                    virt.source_id = free_source;
                }
            }
            virt.active = true;
            virt.width = desktop_w;
            virt.height = desktop_h;
            virt.origin_x = vx;
            virt.origin_y = vy;
            virt.refresh_num = request.refresh_num;
            virt.refresh_den = request.refresh_den;
            virt.scaling = DisplayScaling::Identity;
            virt.rotation = request.rotation;
            virt.scanline_ordering = DisplayScanlineOrdering::Progressive;

            desired_paths.extend(active_physical);
            desired_paths.push(virt);
        }
    }

    if request.hdr_enabled
        || virtual_base.hdr_state.is_none()
        || virtual_base
            .hdr_state
            .is_some_and(|s| s.hdr_enabled != request.hdr_enabled)
    {
        hdr_updates.push(HdrTargetSetting {
            adapter_luid: virtual_luid,
            target_id: virtual_target_id,
            hdr_enabled: request.hdr_enabled,
            wcg_acm_enabled: None,
        });
    }

    Ok(PlannedTopology {
        desired_paths,
        hdr_updates,
        skipped_unplugged_targets: Vec::new(),
    })
}

/// Plans an in-place resolution, refresh-rate, and optional HDR update on `(virtual_luid, virtual_target_id)`.
///
/// All other currently active display paths keep their exact geometry, origins, and active status.
#[allow(
    clippy::too_many_arguments,
    reason = "explicit mode fields avoid intermediate builder boilerplate"
)]
pub fn plan_in_place_mode_update(
    current: &CcdTopologyState,
    virtual_luid: AdapterLuid,
    virtual_target_id: u32,
    width: u32,
    height: u32,
    refresh_num: u32,
    refresh_den: u32,
    hdr_enabled: Option<bool>,
) -> Result<PlannedTopology, ClientError> {
    if width == 0 || height == 0 || refresh_num == 0 || refresh_den == 0 {
        return Err(ClientError::InvalidInput(
            "virtual display resolution and refresh rate must be non-zero".to_string(),
        ));
    }

    let mut found_virtual = None;
    let mut active_others = Vec::new();
    for path in &current.paths {
        if path.adapter_luid == virtual_luid && path.target_id == virtual_target_id {
            if path.target_available {
                found_virtual = Some(path.clone());
            }
        } else if path.active {
            active_others.push(path.clone());
        }
    }

    let Some(mut virt) = found_virtual else {
        return Err(ClientError::VirtualDisplayNotFound {
            adapter_luid_low: virtual_luid.low,
            adapter_luid_high: virtual_luid.high,
            target_id: virtual_target_id,
        });
    };

    if !virt.active {
        let used_sources: BTreeSet<u32> = active_others
            .iter()
            .filter(|p| p.adapter_luid == virtual_luid)
            .map(|p| p.source_id)
            .collect();
        if used_sources.contains(&virt.source_id) {
            if let Some(free_source) = (0u32..).find(|id| !used_sources.contains(id)) {
                virt.source_id = free_source;
            }
        }
        if !active_others.is_empty() {
            let max_right = active_others
                .iter()
                .map(|p| {
                    p.origin_x
                        .saturating_add(i32::try_from(p.width).unwrap_or(0))
                })
                .max()
                .unwrap_or(0);
            virt.origin_x = max_right;
            virt.origin_y = 0;
        } else {
            virt.origin_x = 0;
            virt.origin_y = 0;
        }
    }

    virt.active = true;
    let (desktop_w, desktop_h) = if virt.rotation.is_transposed() {
        (height, width)
    } else {
        (width, height)
    };
    virt.width = desktop_w;
    virt.height = desktop_h;
    virt.refresh_num = refresh_num;
    virt.refresh_den = refresh_den;
    virt.scaling = DisplayScaling::Identity;
    virt.scanline_ordering = DisplayScanlineOrdering::Progressive;

    let mut desired_paths = active_others;
    desired_paths.push(virt.clone());

    let mut hdr_updates = Vec::new();
    if let Some(enable) = hdr_enabled {
        if enable
            || virt.hdr_state.is_none()
            || virt.hdr_state.is_some_and(|s| s.hdr_enabled != enable)
        {
            hdr_updates.push(HdrTargetSetting {
                adapter_luid: virtual_luid,
                target_id: virtual_target_id,
                hdr_enabled: enable,
                wcg_acm_enabled: None,
            });
        }
    }

    Ok(PlannedTopology {
        desired_paths,
        hdr_updates,
        skipped_unplugged_targets: Vec::new(),
    })
}

/// Plans restoring physical displays from `snapshot`, skipping any unplugged monitors cleanly.
///
/// If the original `(0, 0)` primary monitor was unplugged during the streaming session, this
/// shifts all remaining physical displays so a surviving monitor takes `(0, 0)` primary origin.
pub fn plan_restore_topology(
    snapshot: &TopologySnapshot,
    current: &CcdTopologyState,
    virtual_target: Option<(AdapterLuid, u32)>,
) -> Result<PlannedTopology, ClientError> {
    let available_targets: BTreeSet<(AdapterLuid, u32)> = current
        .paths
        .iter()
        .filter(|p| p.target_available)
        .map(|p| (p.adapter_luid, p.target_id))
        .collect();

    let mut hdr_map: BTreeMap<(AdapterLuid, u32), DisplayColorState> = BTreeMap::new();
    for entry in &snapshot.hdr_states {
        hdr_map.insert((entry.adapter_luid, entry.target_id), entry.state);
    }

    let mut restored_active = Vec::new();
    let mut skipped_unplugged_targets = Vec::new();

    for path in &snapshot.paths {
        if let Some((vl, vt)) = virtual_target {
            if path.adapter_luid == vl && path.target_id == vt {
                continue;
            }
        }
        if !path.active {
            continue;
        }
        let key = (path.adapter_luid, path.target_id);
        if !available_targets.contains(&key) {
            skipped_unplugged_targets.push(key);
            continue;
        }
        restored_active.push(path.clone());
    }

    if restored_active.is_empty() {
        return Err(ClientError::NoActivePhysicalDisplays);
    }

    let has_primary = restored_active
        .iter()
        .any(|p| p.origin_x == 0 && p.origin_y == 0);
    if !has_primary {
        // WHY: Windows SetDisplayConfig requires an active path at `(0, 0)` as the desktop primary;
        // shifting all surviving monitors by the leftmost/topmost surviving origin preserves layout.
        if let Some((shift_x, shift_y)) = restored_active
            .iter()
            .min_by_key(|p| (p.origin_x, p.origin_y))
            .map(|p| (p.origin_x, p.origin_y))
        {
            for path in &mut restored_active {
                path.origin_x = path.origin_x.saturating_sub(shift_x);
                path.origin_y = path.origin_y.saturating_sub(shift_y);
            }
        }
    }

    let mut hdr_updates = Vec::new();
    for path in &restored_active {
        let key = (path.adapter_luid, path.target_id);
        if let Some(color) = hdr_map.get(&key).copied().or(path.hdr_state) {
            hdr_updates.push(HdrTargetSetting {
                adapter_luid: path.adapter_luid,
                target_id: path.target_id,
                hdr_enabled: color.hdr_enabled,
                wcg_acm_enabled: Some(color.wcg_acm_enabled),
            });
        }
    }

    Ok(PlannedTopology {
        desired_paths: restored_active,
        hdr_updates,
        skipped_unplugged_targets,
    })
}

/// Plans re-enabling attached non-virtual displays when no saved snapshot backup exists.
pub fn plan_enable_attached_physical(
    current: &CcdTopologyState,
    exclude_virtual: Option<(AdapterLuid, u32)>,
) -> Result<PlannedTopology, ClientError> {
    let is_excluded = |p: &DisplayPathRecord| {
        exclude_virtual.is_some_and(|(vl, vt)| p.adapter_luid == vl && p.target_id == vt)
    };

    let already_active: Vec<DisplayPathRecord> = current
        .paths
        .iter()
        .filter(|p| p.active && p.target_available && !is_excluded(p))
        .cloned()
        .collect();
    if !already_active.is_empty() {
        let has_primary = already_active
            .iter()
            .any(|p| p.origin_x == 0 && p.origin_y == 0);
        let mut desired_paths = already_active;
        if !has_primary {
            if let Some((sx, sy)) = desired_paths
                .iter()
                .min_by_key(|p| (p.origin_x, p.origin_y))
                .map(|p| (p.origin_x, p.origin_y))
            {
                for path in &mut desired_paths {
                    path.origin_x = path.origin_x.saturating_sub(sx);
                    path.origin_y = path.origin_y.saturating_sub(sy);
                }
            }
        }
        return Ok(PlannedTopology {
            desired_paths,
            hdr_updates: Vec::new(),
            skipped_unplugged_targets: Vec::new(),
        });
    }

    let mut cursor_x = 0i32;
    let mut used_sources: BTreeSet<(AdapterLuid, u32)> = BTreeSet::new();
    let mut desired_paths = Vec::new();
    for path in &current.paths {
        if !path.target_available || is_excluded(path) {
            continue;
        }
        let mut enabled = path.clone();
        enabled.active = true;
        if enabled.width == 0 || enabled.height == 0 {
            enabled.width = 1920;
            enabled.height = 1080;
        }
        if enabled.refresh_num == 0 || enabled.refresh_den == 0 {
            enabled.refresh_num = 60;
            enabled.refresh_den = 1;
        }
        if used_sources.contains(&(enabled.adapter_luid, enabled.source_id)) {
            if let Some(free) =
                (0u32..).find(|s| !used_sources.contains(&(enabled.adapter_luid, *s)))
            {
                enabled.source_id = free;
            }
        }
        used_sources.insert((enabled.adapter_luid, enabled.source_id));
        enabled.origin_x = cursor_x;
        enabled.origin_y = 0;
        cursor_x = cursor_x.saturating_add(i32::try_from(enabled.width).unwrap_or(1920));
        desired_paths.push(enabled);
    }

    if desired_paths.is_empty() {
        return Err(ClientError::NoActivePhysicalDisplays);
    }

    Ok(PlannedTopology {
        desired_paths,
        hdr_updates: Vec::new(),
        skipped_unplugged_targets: Vec::new(),
    })
}
