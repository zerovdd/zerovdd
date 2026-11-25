//! Hidden console-session CCD helper (`zerovdd internal-ccd`) invoked by `zerovdd-svc`.

use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::{Args, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use zerovdd_client::topology::{
    default_snapshot_path, AdapterLuid, DisplayRotation, StreamingTopologyRequest,
};
use zerovdd_config::SunshineTopologyMode;

/// Arguments for `zerovdd internal-ccd`.
#[derive(Debug, Args)]
pub struct InternalCcdArgs {
    /// CCD operation to execute inside an interactive console session (`Session > 0`).
    #[command(subcommand)]
    pub action: InternalCcdCommand,
}

/// Supported `internal-ccd` subcommands.
#[derive(Debug, Subcommand)]
pub enum InternalCcdCommand {
    /// Snapshot the current physical display topology to `topology-backup.json`.
    Snapshot(SnapshotArgs),
    /// Resolve the arrived virtual display in CCD and apply streaming topology + HDR state.
    Apply(ApplyArgs),
    /// Update mode and optional HDR state on an active virtual display in place.
    SetMode(SetModeArgs),
    /// Restore physical display topology and HDR/ACM state from `topology-backup.json`.
    Restore(RestoreArgs),
}

/// CLI topology mode selector for `internal-ccd apply`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliTopologyMode {
    /// Disable physical monitors and set the virtual display as sole primary.
    Isolate,
    /// Keep physical monitors enabled and extend with the virtual display.
    Extend,
}

impl From<CliTopologyMode> for SunshineTopologyMode {
    fn from(mode: CliTopologyMode) -> Self {
        match mode {
            CliTopologyMode::Isolate => Self::Isolate,
            CliTopologyMode::Extend => Self::Extend,
        }
    }
}

/// Arguments for `zerovdd internal-ccd snapshot`.
#[derive(Debug, Args)]
pub struct SnapshotArgs {
    /// Backup snapshot path (defaults to `%ProgramData%\zerovdd\topology-backup.json`).
    #[arg(long, value_name = "PATH")]
    pub snapshot_path: Option<PathBuf>,
    /// Overwrite an existing snapshot file instead of preserving the pre-session backup.
    #[arg(long)]
    pub force_overwrite: bool,
}

/// Serializable JSON payload accepted by `zerovdd internal-ccd apply --payload-json <PATH>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InternalCcdApplyPayload {
    /// Virtual adapter LUID low part.
    pub adapter_luid_low: u32,
    /// Virtual adapter LUID high part.
    pub adapter_luid_high: i32,
    /// Virtual display VidPN target ID.
    pub target_id: u32,
    /// Requested width in pixels.
    pub width: u32,
    /// Requested height in pixels.
    pub height: u32,
    /// Requested rational refresh numerator.
    pub refresh_num: u32,
    /// Requested rational refresh denominator.
    pub refresh_den: u32,
    /// Whether HDR should be enabled on the virtual display.
    pub hdr: bool,
    /// Topology mode (`isolate` or `extend`).
    pub topology_mode: SunshineTopologyMode,
    /// Optional backup snapshot file path.
    #[serde(default)]
    pub snapshot_path: Option<PathBuf>,
    /// Optional extend origin `(x, y)`.
    #[serde(default)]
    pub extend_origin: Option<(i32, i32)>,
    /// Whether the virtual display should become `(0, 0)` primary in `extend` mode.
    #[serde(default)]
    pub make_virtual_primary: bool,
    /// Clockwise display rotation applied in CCD.
    #[serde(default)]
    pub rotation: DisplayRotation,
}

/// Arguments for `zerovdd internal-ccd apply`.
#[derive(Debug, Args)]
pub struct ApplyArgs {
    /// Optional JSON payload file describing the apply parameters.
    #[arg(long, value_name = "PATH")]
    pub payload_json: Option<PathBuf>,
    /// Virtual adapter LUID low part.
    #[arg(long)]
    pub luid_low: Option<u32>,
    /// Virtual adapter LUID high part.
    #[arg(long, allow_hyphen_values = true)]
    pub luid_high: Option<i32>,
    /// Virtual display VidPN target ID.
    #[arg(long)]
    pub target_id: Option<u32>,
    /// Target width in pixels.
    #[arg(long)]
    pub width: Option<u32>,
    /// Target height in pixels.
    #[arg(long)]
    pub height: Option<u32>,
    /// Target rational refresh numerator.
    #[arg(long)]
    pub refresh_num: Option<u32>,
    /// Target rational refresh denominator.
    #[arg(long)]
    pub refresh_den: Option<u32>,
    /// Enable HDR10 on the virtual display.
    #[arg(long)]
    pub hdr: bool,
    /// Streaming topology mode (`isolate` or `extend`).
    #[arg(long, value_enum, default_value_t = CliTopologyMode::Isolate)]
    pub mode: CliTopologyMode,
    /// Backup snapshot path (defaults to `%ProgramData%\zerovdd\topology-backup.json`).
    #[arg(long, value_name = "PATH")]
    pub snapshot_path: Option<PathBuf>,
    /// Skip writing a physical topology backup snapshot.
    #[arg(long)]
    pub no_snapshot: bool,
    /// Make the virtual display `(0, 0)` primary when extending.
    #[arg(long)]
    pub make_virtual_primary: bool,
    /// Clockwise display orientation (`0`, `90`, `180`, `270`).
    #[arg(long, value_name = "DEG")]
    pub orientation: Option<u16>,
}

/// Arguments for `zerovdd internal-ccd restore`.
#[derive(Debug, Args)]
pub struct RestoreArgs {
    /// Backup snapshot path (defaults to `%ProgramData%\zerovdd\topology-backup.json`).
    #[arg(long, value_name = "PATH")]
    pub snapshot_path: Option<PathBuf>,
    /// Optional virtual adapter LUID low part to exclude from restore.
    #[arg(long)]
    pub virtual_luid_low: Option<u32>,
    /// Optional virtual adapter LUID high part to exclude from restore.
    #[arg(long, allow_hyphen_values = true)]
    pub virtual_luid_high: Option<i32>,
    /// Optional virtual target ID to exclude from restore.
    #[arg(long)]
    pub virtual_target_id: Option<u32>,
    /// Enable attached non-virtual displays when no backup snapshot exists.
    #[arg(long)]
    pub fallback_enable_physical: bool,
}

/// Arguments for `zerovdd internal-ccd set-mode`.
#[derive(Debug, Args)]
pub struct SetModeArgs {
    /// Virtual adapter LUID low part.
    #[arg(long)]
    pub luid_low: u32,
    /// Virtual adapter LUID high part.
    #[arg(long, allow_hyphen_values = true)]
    pub luid_high: i32,
    /// Virtual display VidPN target ID.
    #[arg(long)]
    pub target_id: u32,
    /// Target width in pixels.
    #[arg(long)]
    pub width: u32,
    /// Target height in pixels.
    #[arg(long)]
    pub height: u32,
    /// Target rational refresh numerator.
    #[arg(long)]
    pub refresh_num: u32,
    /// Target rational refresh denominator.
    #[arg(long)]
    pub refresh_den: u32,
    /// Optional HDR override (`true` or `false`).
    #[arg(long)]
    pub hdr: Option<bool>,
}

/// Resolves an [`InternalCcdApplyPayload`] from either `--payload-json` or explicit CLI flags.
pub fn resolve_apply_payload(args: &ApplyArgs) -> Result<InternalCcdApplyPayload> {
    if let Some(ref json_path) = args.payload_json {
        let raw = fs::read_to_string(json_path)?;
        let mut payload: InternalCcdApplyPayload = serde_json::from_str(&raw)?;
        if args.no_snapshot {
            payload.snapshot_path = None;
        } else if payload.snapshot_path.is_none() {
            payload.snapshot_path = args
                .snapshot_path
                .clone()
                .or_else(|| Some(default_snapshot_path()));
        }
        return Ok(payload);
    }

    let luid_low = args
        .luid_low
        .ok_or_else(|| anyhow::anyhow!("--luid-low is required when --payload-json is omitted"))?;
    let luid_high = args
        .luid_high
        .ok_or_else(|| anyhow::anyhow!("--luid-high is required when --payload-json is omitted"))?;
    let target_id = args
        .target_id
        .ok_or_else(|| anyhow::anyhow!("--target-id is required when --payload-json is omitted"))?;
    let width = args
        .width
        .ok_or_else(|| anyhow::anyhow!("--width is required when --payload-json is omitted"))?;
    let height = args
        .height
        .ok_or_else(|| anyhow::anyhow!("--height is required when --payload-json is omitted"))?;
    let refresh_num = args.refresh_num.unwrap_or(60);
    let refresh_den = args.refresh_den.unwrap_or(1);

    let snapshot_path = if args.no_snapshot {
        None
    } else {
        Some(
            args.snapshot_path
                .clone()
                .unwrap_or_else(default_snapshot_path),
        )
    };

    let rotation = match args.orientation {
        Some(deg) => DisplayRotation::from_degrees(deg).ok_or_else(|| {
            anyhow::anyhow!("invalid --orientation {deg}: expected 0, 90, 180, or 270")
        })?,
        None => DisplayRotation::Identity,
    };

    Ok(InternalCcdApplyPayload {
        adapter_luid_low: luid_low,
        adapter_luid_high: luid_high,
        target_id,
        width,
        height,
        refresh_num,
        refresh_den,
        hdr: args.hdr,
        topology_mode: args.mode.into(),
        snapshot_path,
        extend_origin: None,
        make_virtual_primary: args.make_virtual_primary,
        rotation,
    })
}

/// Executes `zerovdd internal-ccd`.
pub fn execute(args: InternalCcdArgs) -> Result<()> {
    match args.action {
        InternalCcdCommand::Snapshot(s) => run_snapshot(s),
        InternalCcdCommand::Apply(a) => run_apply(a),
        InternalCcdCommand::SetMode(m) => run_set_mode(m),
        InternalCcdCommand::Restore(r) => run_restore(r),
    }
}

#[cfg(windows)]
fn ensure_console_session() -> Result<()> {
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::GetCurrentProcessId;

    let mut session_id: u32 = 0;
    // SAFETY: `GetCurrentProcessId` returns the current PID and `session_id` is a valid stack `u32`.
    let ok = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session_id) };
    if ok.is_err() {
        bail!("failed to query current Windows session ID for internal-ccd");
    }
    if session_id == 0 {
        bail!(
            "internal-ccd refuses to call Windows CCD/HDR APIs from Session 0; \
             must run inside an active console session (Session > 0)"
        );
    }
    Ok(())
}

#[cfg(windows)]
fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(windows)]
fn run_snapshot(args: SnapshotArgs) -> Result<()> {
    use zerovdd_client::ccd::{DisplayTopologyBackend, WindowsCcdBackend};
    use zerovdd_client::topology::{load_snapshot, save_snapshot_atomic, snapshot_from_state};

    ensure_console_session()?;
    let path = args.snapshot_path.unwrap_or_else(default_snapshot_path);
    if !args.force_overwrite && load_snapshot(&path)?.is_some() {
        return Ok(());
    }

    let backend = WindowsCcdBackend::new();
    let state = backend.query_topology()?;
    let snapshot = snapshot_from_state(&state, unix_time_ms(), None);
    if snapshot.paths.iter().any(|p| p.active) {
        save_snapshot_atomic(&path, &snapshot)?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn run_snapshot(_args: SnapshotArgs) -> Result<()> {
    bail!("zerovdd internal-ccd snapshot is only supported on Windows")
}

#[cfg(windows)]
fn run_apply(args: ApplyArgs) -> Result<()> {
    use zerovdd_client::ccd::{apply_streaming_session, SessionApplyOptions, WindowsCcdBackend};

    ensure_console_session()?;
    let payload = resolve_apply_payload(&args)?;
    let backend = WindowsCcdBackend::new();
    let request = StreamingTopologyRequest {
        mode: payload.topology_mode,
        width: payload.width,
        height: payload.height,
        refresh_num: payload.refresh_num,
        refresh_den: payload.refresh_den,
        hdr_enabled: payload.hdr,
        extend_origin: payload.extend_origin,
        make_virtual_primary: payload.make_virtual_primary,
        rotation: payload.rotation,
    };
    let options = SessionApplyOptions {
        snapshot_path: payload.snapshot_path.as_deref(),
        created_at_unix_ms: unix_time_ms(),
        ..Default::default()
    };
    let applied = apply_streaming_session(
        &backend,
        AdapterLuid::new(payload.adapter_luid_low, payload.adapter_luid_high),
        payload.target_id,
        &request,
        &options,
    )?;
    println!("{}", serde_json::to_string(&applied.virtual_display)?);
    Ok(())
}

#[cfg(not(windows))]
fn run_apply(args: ApplyArgs) -> Result<()> {
    let _ = resolve_apply_payload(&args)?;
    let _ = core::mem::size_of::<StreamingTopologyRequest>();
    let _ = AdapterLuid::new(0, 0);
    bail!("zerovdd internal-ccd apply is only supported on Windows")
}

#[cfg(windows)]
fn run_restore(args: RestoreArgs) -> Result<()> {
    use zerovdd_client::ccd::{restore_streaming_session_with_fallback, WindowsCcdBackend};

    ensure_console_session()?;
    let path = args.snapshot_path.unwrap_or_else(default_snapshot_path);
    let virtual_target = match (
        args.virtual_luid_low,
        args.virtual_luid_high,
        args.virtual_target_id,
    ) {
        (Some(low), Some(high), Some(tid)) => Some((AdapterLuid::new(low, high), tid)),
        (None, None, None) => None,
        _ => bail!("virtual LUID low/high and target ID must all be provided together"),
    };
    let backend = WindowsCcdBackend::new();
    let report = restore_streaming_session_with_fallback(
        &backend,
        &path,
        virtual_target,
        args.fallback_enable_physical,
    )?;
    println!(
        "{}",
        serde_json::json!({
            "snapshot_found": report.snapshot_found,
            "restored_path_count": report.restored_path_count,
            "skipped_unplugged_targets": report.skipped_unplugged_targets.len(),
        })
    );
    Ok(())
}

#[cfg(not(windows))]
fn run_restore(_args: RestoreArgs) -> Result<()> {
    bail!("zerovdd internal-ccd restore is only supported on Windows")
}

#[cfg(windows)]
fn run_set_mode(args: SetModeArgs) -> Result<()> {
    use zerovdd_client::ccd::{apply_display_mode_in_place, WindowsCcdBackend};

    ensure_console_session()?;
    let backend = WindowsCcdBackend::new();
    let updated = apply_display_mode_in_place(
        &backend,
        AdapterLuid::new(args.luid_low, args.luid_high),
        args.target_id,
        args.width,
        args.height,
        args.refresh_num,
        args.refresh_den,
        args.hdr,
    )?;
    println!("{}", serde_json::to_string(&updated)?);
    Ok(())
}

#[cfg(not(windows))]
fn run_set_mode(_args: SetModeArgs) -> Result<()> {
    bail!("zerovdd internal-ccd set-mode is only supported on Windows")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn resolves_apply_payload_from_json_file_or_cli_flags() {
        let dir = TempDir::new().unwrap();
        let json_path = dir.path().join("apply.json");
        let payload = InternalCcdApplyPayload {
            adapter_luid_low: 0x100,
            adapter_luid_high: 0,
            target_id: 42,
            width: 2560,
            height: 1600,
            refresh_num: 90,
            refresh_den: 1,
            hdr: true,
            topology_mode: SunshineTopologyMode::Isolate,
            snapshot_path: Some(dir.path().join("backup.json")),
            extend_origin: None,
            make_virtual_primary: true,
            rotation: DisplayRotation::Identity,
        };
        fs::write(&json_path, serde_json::to_string(&payload).unwrap()).unwrap();

        let parsed = resolve_apply_payload(&ApplyArgs {
            payload_json: Some(json_path),
            luid_low: None,
            luid_high: None,
            target_id: None,
            width: None,
            height: None,
            refresh_num: None,
            refresh_den: None,
            hdr: false,
            mode: CliTopologyMode::Isolate,
            snapshot_path: None,
            no_snapshot: false,
            make_virtual_primary: false,
            orientation: None,
        })
        .unwrap();
        assert_eq!(parsed, payload);

        let from_flags = resolve_apply_payload(&ApplyArgs {
            payload_json: None,
            luid_low: Some(7),
            luid_high: Some(0),
            target_id: Some(9),
            width: Some(3840),
            height: Some(2160),
            refresh_num: Some(60_000),
            refresh_den: Some(1001),
            hdr: true,
            mode: CliTopologyMode::Extend,
            snapshot_path: None,
            no_snapshot: true,
            make_virtual_primary: false,
            orientation: Some(90),
        })
        .unwrap();
        assert_eq!(from_flags.rotation, DisplayRotation::Rotate90);
        assert_eq!(from_flags.topology_mode, SunshineTopologyMode::Extend);
        assert!(from_flags.snapshot_path.is_none());
        assert_eq!(from_flags.refresh_num, 60_000);
        assert_eq!(from_flags.refresh_den, 1001);
    }
}
