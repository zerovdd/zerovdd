//! LizardByte Sunshine streaming session and configuration subcommands (`zerovdd sunshine`).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;
use tempfile::NamedTempFile;
use zerovdd_config::{default_config_path, load_or_default, SunshineTopologyMode};
use zerovdd_proto::ipc::{
    IpcRequest, IpcResponse, ServiceHealth, SessionStatus, StartSessionRequest, StopSessionRequest,
    TopologyMode, IPC_PIPE_NAME, MAX_IPC_CLIENT_ID_LEN,
};
use zerovdd_proto::validation::{validate_dimensions, validate_refresh_rate};

use crate::cmd::display::{parse_hz_to_millihz, try_send_ipc_request};

/// Default Sunshine client identifier used when neither UUID nor name is provided.
pub const DEFAULT_SUNSHINE_CLIENT_ID: &str = "default-sunshine-client";

/// Default Windows installation path for `sunshine.conf`.
pub const DEFAULT_SUNSHINE_CONF_PATH: &str = r"C:\Program Files\Sunshine\config\sunshine.conf";

/// Arguments for `zerovdd sunshine`.
#[derive(Debug, Args)]
pub struct SunshineArgs {
    /// Sunshine integration action to execute.
    #[command(subcommand)]
    pub action: SunshineCommand,
}

/// Supported Sunshine integration subcommands.
#[derive(Debug, Subcommand)]
pub enum SunshineCommand {
    /// Start a managed virtual display session from Sunshine client environment variables.
    Start(SunshineStartArgs),
    /// Stop the active managed virtual display session and restore physical topology.
    Stop(SunshineStopArgs),
    /// Query active Sunshine virtual display session and service health status.
    Status(SunshineStatusArgs),
    /// Inspect or configure `sunshine.conf` prep commands and display switcher settings.
    Configure(SunshineConfigureArgs),
}

/// Arguments for `zerovdd sunshine start`.
#[derive(Debug, Args)]
pub struct SunshineStartArgs {
    /// Explicit owning `sunshine.exe` PID override (skips ancestor process discovery).
    #[arg(long, value_name = "PID")]
    pub owner_pid: Option<u32>,
    /// Override virtual display width in pixels (defaults to `SUNSHINE_CLIENT_WIDTH`).
    #[arg(long, value_name = "PX")]
    pub width: Option<u32>,
    /// Override virtual display height in pixels (defaults to `SUNSHINE_CLIENT_HEIGHT`).
    #[arg(long, value_name = "PX")]
    pub height: Option<u32>,
    /// Override refresh rate in hertz (defaults to `SUNSHINE_CLIENT_FPS`, e.g. `59.94` or `120`).
    #[arg(long, value_name = "HZ")]
    pub fps: Option<String>,
    /// Override HDR10 toggle (defaults to `SUNSHINE_CLIENT_HDR`).
    #[arg(long, value_name = "BOOL", num_args = 0..=1, default_missing_value = "true")]
    pub hdr: Option<bool>,
    /// Override client identifier (defaults to `SUNSHINE_CLIENT_UUID`, then `SUNSHINE_CLIENT_NAME`).
    #[arg(long, value_name = "ID")]
    pub client_id: Option<String>,
    /// Output started session details as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zerovdd sunshine stop`.
#[derive(Debug, Args)]
pub struct SunshineStopArgs {
    /// Optional client identifier filter; when omitted, stops the active session.
    #[arg(long, value_name = "ID")]
    pub client_id: Option<String>,
    /// Force session teardown even if `owner_pid` does not match the recorded owner.
    #[arg(long)]
    pub force: bool,
    /// Output stop result as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zerovdd sunshine status`.
#[derive(Debug, Args)]
pub struct SunshineStatusArgs {
    /// Output session and service status as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zerovdd sunshine configure`.
#[derive(Debug, Args)]
pub struct SunshineConfigureArgs {
    /// Path to `sunshine.conf` (defaults to configured `sunshine_config_path` or Sunshine's standard location).
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Apply proposed edits to `sunshine.conf` after writing a timestamped backup.
    #[arg(long)]
    pub apply: bool,
    /// Clear a pinned `output_name` entry in `sunshine.conf` so Sunshine captures the primary virtual display.
    #[arg(long)]
    pub clear_output_name: bool,
    /// Output configuration plan or result as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Environment variable lookup abstraction for deterministic testing.
pub trait EnvProvider {
    /// Returns the value of `key` if set in the environment.
    fn get_var(&self, key: &str) -> Option<String>;
}

/// Live OS environment variable provider backed by `std::env::var`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemEnv;

impl EnvProvider for SystemEnv {
    fn get_var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

impl EnvProvider for BTreeMap<String, String> {
    fn get_var(&self, key: &str) -> Option<String> {
        self.get(key).cloned()
    }
}

/// Parsed Sunshine client session parameters ready for IPC submission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParsedSunshineSession {
    /// Sanitized bounded client identifier (`<= 128` bytes).
    pub client_id: String,
    /// Virtual display width in pixels.
    pub width: u32,
    /// Virtual display height in pixels.
    pub height: u32,
    /// Virtual display refresh rate in millihertz.
    pub refresh_mhz: u32,
    /// Whether HDR10 is requested for this session.
    pub hdr: bool,
}

/// Portable process snapshot entry used for ancestor `sunshine.exe` discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEntry {
    /// OS process identifier.
    pub pid: u32,
    /// Parent OS process identifier.
    pub parent_pid: u32,
    /// Process executable name or path.
    pub exe_name: String,
    /// Windows process creation time in 100-nanosecond FILETIME intervals.
    pub creation_time: u64,
}

/// Combined status payload returned by `zerovdd sunshine status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SunshineStatusReport {
    /// Whether `zerovdd-svc` is reachable over `IPC_PIPE_NAME`.
    pub service_running: bool,
    /// Active or inactive streaming session details when service is running.
    pub session: Option<SessionStatus>,
    /// Service and driver health telemetry when service is running.
    pub health: Option<ServiceHealth>,
}

/// Planned or applied changes for `zerovdd sunshine configure`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SunshineConfigureReport {
    /// Target `sunshine.conf` path.
    pub config_path: PathBuf,
    /// Whether changes were written (`false` in dry-run mode or when already up to date).
    pub applied: bool,
    /// Whether `sunshine.conf` needed modifications.
    pub changed: bool,
    /// Timestamped backup path created before modifying an existing `sunshine.conf`.
    pub backup_path: Option<PathBuf>,
    /// Human-readable descriptions of planned or applied modifications.
    pub changes: Vec<String>,
    /// Warnings emitted during configuration inspection (e.g. pinned `output_name`).
    pub warnings: Vec<String>,
}

/// Executes a `zerovdd sunshine` subcommand.
pub fn execute(args: SunshineArgs) -> Result<()> {
    match args.action {
        SunshineCommand::Start(start_args) => run_start(start_args),
        SunshineCommand::Stop(stop_args) => run_stop(stop_args),
        SunshineCommand::Status(status_args) => run_status(status_args),
        SunshineCommand::Configure(cfg_args) => run_configure(cfg_args),
    }
}

/// Parses Sunshine client environment variables and optional CLI overrides into validated session settings.
pub fn parse_sunshine_session_env<E: EnvProvider>(
    env: &E,
    args: &SunshineStartArgs,
) -> Result<ParsedSunshineSession> {
    let width = match args.width {
        Some(w) => w,
        None => parse_required_u32_env(env, "SUNSHINE_CLIENT_WIDTH", "--width")?,
    };
    let height = match args.height {
        Some(h) => h,
        None => parse_required_u32_env(env, "SUNSHINE_CLIENT_HEIGHT", "--height")?,
    };
    validate_dimensions(width, height)?;

    let fps_raw = match args.fps.as_deref() {
        Some(raw) => raw.to_string(),
        None => env
            .get_var("SUNSHINE_CLIENT_FPS")
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "SUNSHINE_CLIENT_FPS is not set; run from Sunshine or pass --width, --height, and --fps"
                )
            })?,
    };
    let refresh_mhz = parse_sunshine_fps(&fps_raw)?;

    let hdr = match args.hdr {
        Some(flag) => flag,
        None => parse_hdr_env(env.get_var("SUNSHINE_CLIENT_HDR").as_deref())?,
    };

    let client_id = resolve_client_id(env, args.client_id.as_deref())?;

    Ok(ParsedSunshineSession {
        client_id,
        width,
        height,
        refresh_mhz,
        hdr,
    })
}

fn parse_required_u32_env<E: EnvProvider>(env: &E, var_name: &str, flag_name: &str) -> Result<u32> {
    let raw = env
        .get_var(var_name)
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{var_name} is not set; run from Sunshine or pass --width, --height, and --fps (override with {flag_name})"
            )
        })?;
    let trimmed = raw.trim();
    trimmed
        .parse::<u32>()
        .map_err(|_| anyhow::anyhow!("invalid {var_name} value `{raw}`: expected positive integer"))
}

/// Parses an integer or fractional FPS string (for example `"59.94"` or `"120"`) into millihertz.
pub fn parse_sunshine_fps(raw: &str) -> Result<u32> {
    let mut fps_str = raw.trim();
    if fps_str.len() >= 2 && fps_str.as_bytes()[fps_str.len() - 2..].eq_ignore_ascii_case(b"hz") {
        fps_str = fps_str[..fps_str.len() - 2].trim();
    }
    if fps_str.is_empty() {
        bail!("invalid SUNSHINE_CLIENT_FPS value `{raw}`: empty refresh rate");
    }
    let refresh_mhz = parse_hz_to_millihz(fps_str, raw)?;
    validate_refresh_rate(refresh_mhz, 1000)?;
    Ok(refresh_mhz)
}

/// Parses `SUNSHINE_CLIENT_HDR` supporting `true`/`1`/`yes`/`on` and `false`/`0`/`no`/`off`/unset.
pub fn parse_hdr_env(raw: Option<&str>) -> Result<bool> {
    let Some(val) = raw else {
        return Ok(false);
    };
    let trimmed = val.trim();
    if trimmed.is_empty() {
        return Ok(false);
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => bail!(
            "invalid SUNSHINE_CLIENT_HDR value `{val}`: expected true/1/yes/on or false/0/no/off"
        ),
    }
}

/// Resolves and sanitizes the session `client_id` from CLI override, `SUNSHINE_CLIENT_UUID`, or `SUNSHINE_CLIENT_NAME`.
pub fn resolve_client_id<E: EnvProvider>(env: &E, override_id: Option<&str>) -> Result<String> {
    if let Some(raw) = override_id {
        let sanitized = sanitize_client_id(raw);
        if sanitized.is_empty() {
            bail!("--client-id must not be empty");
        }
        return Ok(sanitized);
    }

    if let Some(uuid) = env.get_var("SUNSHINE_CLIENT_UUID") {
        let sanitized = sanitize_client_id(&uuid);
        if !sanitized.is_empty() {
            return Ok(sanitized);
        }
    }

    if let Some(name) = env.get_var("SUNSHINE_CLIENT_NAME") {
        let sanitized = sanitize_client_id(&name);
        if !sanitized.is_empty() {
            return Ok(sanitized);
        }
    }

    Ok(DEFAULT_SUNSHINE_CLIENT_ID.to_string())
}

/// Strips control characters and bounds `client_id` to [`MAX_IPC_CLIENT_ID_LEN`] bytes on a UTF-8 boundary.
pub fn sanitize_client_id(raw: &str) -> String {
    let mut cleaned: String = raw
        .trim()
        .chars()
        .map(|ch| if ch.is_control() { '_' } else { ch })
        .collect();
    let trimmed_len = cleaned.trim().len();
    if trimmed_len == 0 {
        return String::new();
    }
    if cleaned.len() > MAX_IPC_CLIENT_ID_LEN {
        let mut end = MAX_IPC_CLIENT_ID_LEN;
        while end > 0 && !cleaned.is_char_boundary(end) {
            end -= 1;
        }
        cleaned.truncate(end);
    }
    cleaned.trim().to_string()
}

/// Walks `parent_pid` links upward from `start_pid` to find the nearest ancestor `sunshine.exe`.
///
/// Guards against process-tree cycles (including self-parenting PIDs), verifies monotonic
/// ancestor creation timestamps (`parent.creation_time <= descendant.creation_time`) to reject
/// recycled PIDs, and skips wrapper shells such as `cmd.exe`, `powershell.exe`, or `conhost.exe`.
pub fn find_ancestor_sunshine(entries: &[ProcessEntry], start_pid: u32) -> Option<(u32, u64)> {
    let by_pid: BTreeMap<u32, &ProcessEntry> = entries.iter().map(|e| (e.pid, e)).collect();
    let start_entry = by_pid.get(&start_pid)?;
    let mut descendant_ct = start_entry.creation_time;
    let mut visited = BTreeSet::new();
    let mut current_pid = start_entry.parent_pid;
    visited.insert(start_pid);

    while current_pid != 0 {
        if !visited.insert(current_pid) {
            break;
        }
        let entry = by_pid.get(&current_pid)?;
        // WHY: An ancestor process cannot have been created after any of its descendants; a newer
        // non-zero creation timestamp proves the OS recycled the parent PID after the original exited.
        if descendant_ct != 0 && entry.creation_time != 0 && entry.creation_time > descendant_ct {
            return None;
        }
        if entry.creation_time != 0 {
            descendant_ct = if descendant_ct == 0 {
                entry.creation_time
            } else {
                descendant_ct.min(entry.creation_time)
            };
        }
        if is_sunshine_exe(&entry.exe_name) {
            return Some((entry.pid, entry.creation_time));
        }
        current_pid = entry.parent_pid;
    }
    None
}

fn is_sunshine_exe(exe_name: &str) -> bool {
    let basename = exe_name
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(exe_name)
        .trim();
    basename.eq_ignore_ascii_case("sunshine.exe")
}

#[cfg(windows)]
fn utf16_nul_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

#[cfg(any(windows, test))]
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProcessCreationQuery {
    Timestamp(u64),
    AccessDenied,
    Failed(String),
}

#[cfg(any(windows, test))]
fn resolve_owner_creation_time<F>(cached_ct: u64, query: F) -> Result<u64>
where
    F: FnOnce() -> ProcessCreationQuery,
{
    if cached_ct != 0 {
        return Ok(cached_ct);
    }
    match query() {
        ProcessCreationQuery::Timestamp(ct) => Ok(ct),
        // WHY: When `sunshine.exe` runs under LocalSystem (`SunshineService`) and launches
        // `zerovdd sunshine start` unelevated (`elevated: "false"`), `OpenProcess` returns
        // `ERROR_ACCESS_DENIED` for the live ancestor PID; returning `0` delegates process
        // liveness validation and crash supervision to the LocalSystem `zerovdd-svc` service.
        ProcessCreationQuery::AccessDenied => Ok(0),
        ProcessCreationQuery::Failed(msg) => bail!("{msg}"),
    }
}

#[cfg(windows)]
fn query_process_creation_time(pid: u32) -> ProcessCreationQuery {
    use windows::core::HRESULT;
    use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, FILETIME, HANDLE};
    use windows::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    struct HandleGuard(HANDLE);
    impl Drop for HandleGuard {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: `self.0` was opened via `OpenProcess` and is closed once on drop.
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    // SAFETY: Opens target PID with limited query access strictly to read process timestamps.
    let raw = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(h) => h,
        Err(err) if err.code() == HRESULT::from_win32(ERROR_ACCESS_DENIED.0) => {
            return ProcessCreationQuery::AccessDenied;
        }
        Err(err) => {
            return ProcessCreationQuery::Failed(format!(
                "failed to open process PID {pid} to query creation time: {err}"
            ));
        }
    };
    let guard = HandleGuard(raw);

    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();

    // SAFETY: `guard.0` is valid and all `FILETIME` out-pointers point to initialized stack locals.
    if let Err(err) =
        unsafe { GetProcessTimes(guard.0, &mut creation, &mut exit, &mut kernel, &mut user) }
    {
        if err.code() == HRESULT::from_win32(ERROR_ACCESS_DENIED.0) {
            return ProcessCreationQuery::AccessDenied;
        }
        return ProcessCreationQuery::Failed(format!(
            "GetProcessTimes failed for PID {pid}: {err}"
        ));
    }

    ProcessCreationQuery::Timestamp(
        (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime),
    )
}

#[cfg(windows)]
fn build_process_snapshot() -> Result<Vec<ProcessEntry>> {
    use core::mem::size_of;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    struct SnapGuard(HANDLE);
    impl Drop for SnapGuard {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: `self.0` is a ToolHelp snapshot handle closed once on drop.
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    // SAFETY: Captures a point-in-time snapshot of local processes.
    let snap_raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .context("CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS) failed")?;
    let snap = SnapGuard(snap_raw);

    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: `snap.0` is valid and `entry.dwSize` is initialized to `size_of::<PROCESSENTRY32W>()`.
    if unsafe { Process32FirstW(snap.0, &mut entry) }.is_err() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    loop {
        let exe_name = utf16_nul_to_string(&entry.szExeFile);
        entries.push(ProcessEntry {
            pid: entry.th32ProcessID,
            parent_pid: entry.th32ParentProcessID,
            exe_name,
            creation_time: 0,
        });
        // SAFETY: `snap.0` is valid and `entry.dwSize` remains initialized.
        if unsafe { Process32NextW(snap.0, &mut entry) }.is_err() {
            break;
        }
    }

    Ok(entries)
}

#[cfg(windows)]
fn populate_ancestor_creation_times(entries: &mut [ProcessEntry], start_pid: u32) {
    let index_by_pid: BTreeMap<u32, usize> = entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| (entry.pid, idx))
        .collect();

    let mut visited = BTreeSet::new();
    let mut current_pid = start_pid;
    while current_pid != 0 && visited.insert(current_pid) {
        let Some(&idx) = index_by_pid.get(&current_pid) else {
            break;
        };
        if entries[idx].creation_time == 0 {
            if let ProcessCreationQuery::Timestamp(ct) = query_process_creation_time(current_pid) {
                entries[idx].creation_time = ct;
            }
        }
        if current_pid != start_pid && is_sunshine_exe(&entries[idx].exe_name) {
            break;
        }
        current_pid = entries[idx].parent_pid;
    }
}

#[cfg(windows)]
fn find_sunshine_owner(owner_pid_override: Option<u32>) -> Result<(u32, u64)> {
    use windows::Win32::System::Threading::GetCurrentProcessId;

    if let Some(pid) = owner_pid_override {
        if pid == 0 {
            bail!("--owner-pid must be greater than 0");
        }
        let creation_time = resolve_owner_creation_time(0, || query_process_creation_time(pid))?;
        return Ok((pid, creation_time));
    }

    // SAFETY: `GetCurrentProcessId` takes no pointers and returns the calling process ID.
    let self_pid = unsafe { GetCurrentProcessId() };
    let mut entries = build_process_snapshot()?;
    populate_ancestor_creation_times(&mut entries, self_pid);
    let Some((pid, cached_ct)) = find_ancestor_sunshine(&entries, self_pid) else {
        bail!(
            "could not find an ancestor sunshine.exe process; run from Sunshine or pass --owner-pid <PID>"
        );
    };
    let creation_time =
        resolve_owner_creation_time(cached_ct, || query_process_creation_time(pid))?;
    Ok((pid, creation_time))
}

#[cfg(not(windows))]
fn find_sunshine_owner(owner_pid_override: Option<u32>) -> Result<(u32, u64)> {
    if let Some(pid) = owner_pid_override {
        if pid == 0 {
            bail!("--owner-pid must be greater than 0");
        }
        return Ok((pid, 0));
    }
    bail!(
        "could not find an ancestor sunshine.exe process; pass --owner-pid <PID> or run on Windows"
    )
}

#[cfg(windows)]
fn try_find_ancestor_sunshine_pid() -> Option<u32> {
    use windows::Win32::System::Threading::GetCurrentProcessId;

    // SAFETY: `GetCurrentProcessId` takes no pointers and returns the calling process ID.
    let self_pid = unsafe { GetCurrentProcessId() };
    let mut entries = build_process_snapshot().ok()?;
    populate_ancestor_creation_times(&mut entries, self_pid);
    find_ancestor_sunshine(&entries, self_pid).map(|(pid, _)| pid)
}

#[cfg(not(windows))]
fn try_find_ancestor_sunshine_pid() -> Option<u32> {
    None
}

/// Resolves the IPC [`TopologyMode`] from the `zerovdd` TOML config file at `config_path`.
pub fn resolve_topology_mode_from_path(config_path: &Path) -> Result<TopologyMode> {
    let cfg = load_or_default(config_path)
        .with_context(|| format!("failed to load zerovdd config at {}", config_path.display()))?;
    Ok(match cfg.sunshine.topology_mode {
        SunshineTopologyMode::Isolate => TopologyMode::Isolate,
        SunshineTopologyMode::Extend => TopologyMode::Extend,
    })
}

fn require_ipc_response(req: &IpcRequest) -> Result<IpcResponse> {
    let Some(resp) = try_send_ipc_request(req)? else {
        bail!(
            "zerovdd-svc is not running (named pipe {IPC_PIPE_NAME} not found); start the zerovdd-svc service and retry"
        );
    };
    if let IpcResponse::Error(err) = resp {
        bail!("zerovdd-svc error ({}): {}", err.code, err.message);
    }
    Ok(resp)
}

fn run_start(args: SunshineStartArgs) -> Result<()> {
    let parsed = parse_sunshine_session_env(&SystemEnv, &args)?;
    let (owner_pid, owner_creation_time) = find_sunshine_owner(args.owner_pid)?;
    let topology_mode = resolve_topology_mode_from_path(&default_config_path())?;

    let req = StartSessionRequest {
        client_id: parsed.client_id,
        width: parsed.width,
        height: parsed.height,
        refresh_mhz: parsed.refresh_mhz,
        hdr: parsed.hdr,
        owner_pid,
        owner_creation_time,
        topology_mode,
    };
    req.validate()?;

    let fallback_client_id = req.client_id.clone();
    match require_ipc_response(&IpcRequest::StartSession(req))? {
        IpcResponse::SessionStarted(status) => {
            if args.json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                let display_label = status.display_name.as_deref().unwrap_or("<pending>");
                println!(
                    "Started Sunshine virtual display session for `{}`: {}x{}@{:.3}Hz (HDR={}, display={}, owner_pid={}).",
                    status.client_id.as_deref().unwrap_or(&fallback_client_id),
                    status.width,
                    status.height,
                    f64::from(status.refresh_mhz) / 1000.0,
                    status.hdr_active,
                    display_label,
                    owner_pid
                );
            }
            Ok(())
        }
        other => bail!("unexpected IPC response from zerovdd-svc: {other:?}"),
    }
}

fn run_stop(args: SunshineStopArgs) -> Result<()> {
    let client_id = match args.client_id {
        Some(raw) => {
            let cleaned = sanitize_client_id(&raw);
            if cleaned.is_empty() {
                bail!("--client-id must not be empty");
            }
            Some(cleaned)
        }
        None => None,
    };
    let owner_pid = try_find_ancestor_sunshine_pid();

    let req = StopSessionRequest {
        client_id,
        owner_pid,
        force: args.force,
    };
    req.validate()?;

    match require_ipc_response(&IpcRequest::StopSession(req))? {
        IpcResponse::SessionStopped { restored_topology } => {
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "stopped": true,
                        "restored_topology": restored_topology,
                    }))?
                );
            } else {
                println!(
                    "Stopped Sunshine virtual display session (restored_topology={restored_topology})."
                );
            }
            Ok(())
        }
        other => bail!("unexpected IPC response from zerovdd-svc: {other:?}"),
    }
}

fn run_status(args: SunshineStatusArgs) -> Result<()> {
    let session_resp = try_send_ipc_request(&IpcRequest::GetSessionStatus)?;
    let report = match session_resp {
        None => SunshineStatusReport {
            service_running: false,
            session: None,
            health: None,
        },
        Some(IpcResponse::Error(err)) => {
            bail!("zerovdd-svc error ({}): {}", err.code, err.message);
        }
        Some(IpcResponse::SessionStatus(session)) => {
            let health = match try_send_ipc_request(&IpcRequest::GetHealth)? {
                Some(IpcResponse::Health(h)) => Some(h),
                Some(IpcResponse::Error(err)) => {
                    bail!("zerovdd-svc error ({}): {}", err.code, err.message);
                }
                _ => None,
            };
            SunshineStatusReport {
                service_running: true,
                session: Some(session),
                health,
            }
        }
        Some(other) => bail!("unexpected IPC response from zerovdd-svc: {other:?}"),
    };

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("Service running: {}", report.service_running);
    if let Some(ref health) = report.health {
        println!(
            "  Service version: {} (driver_connected={}, active_displays={}, uptime={}s, backup_present={})",
            health.service_version,
            health.driver_connected,
            health.active_displays,
            health.uptime_secs,
            health.backup_present
        );
    }
    if let Some(ref session) = report.session {
        println!("Session active: {}", session.active);
        if session.active {
            println!(
                "  Client: {} | Mode: {}x{}@{:.3}Hz | HDR: {}",
                session.client_id.as_deref().unwrap_or("<none>"),
                session.width,
                session.height,
                f64::from(session.refresh_mhz) / 1000.0,
                session.hdr_active
            );
            println!(
                "  Display: {} (target_id={:?}, owner_pid={:?}, topology={:?})",
                session.display_name.as_deref().unwrap_or("<none>"),
                session.target_id,
                session.owner_pid,
                session.topology_mode
            );
        }
    }
    Ok(())
}

fn resolve_sunshine_conf_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    let cfg = load_or_default(&default_config_path())?;
    if let Some(custom) = cfg.sunshine.sunshine_config_path {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    Ok(PathBuf::from(DEFAULT_SUNSHINE_CONF_PATH))
}

fn current_exe_command_pair(exe_path: &Path) -> Result<(String, String)> {
    let exe_str = exe_path.to_str().ok_or_else(|| {
        anyhow::anyhow!(
            "executable path `{}` is not valid UTF-8",
            exe_path.display()
        )
    })?;
    Ok((
        format!("\"{exe_str}\" sunshine start"),
        format!("\"{exe_str}\" sunshine stop"),
    ))
}

fn is_zerovdd_prep_entry(entry: &Value) -> bool {
    let Some(obj) = entry.as_object() else {
        return false;
    };
    let do_cmd = obj
        .get("do")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let undo_cmd = obj
        .get("undo")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    (do_cmd.contains("zerovdd") && do_cmd.contains("sunshine start"))
        || (undo_cmd.contains("zerovdd") && undo_cmd.contains("sunshine stop"))
}

fn update_global_prep_cmd(raw_value: &str, exe_path: &Path) -> Result<(String, bool, String)> {
    let (do_cmd, undo_cmd) = current_exe_command_pair(exe_path)?;
    let desired = serde_json::json!({
        "do": do_cmd,
        "undo": undo_cmd,
        "elevated": "false"
    });

    let trimmed = raw_value.trim();
    let mut entries: Vec<Value> = if trimmed.is_empty() {
        Vec::new()
    } else {
        let parsed: Value = serde_json::from_str(trimmed)
            .with_context(|| format!("invalid JSON in Sunshine `global_prep_cmd`: `{trimmed}`"))?;
        match parsed {
            Value::Array(items) => items,
            _ => bail!("Sunshine `global_prep_cmd` must be a JSON array"),
        }
    };

    let mut seen_zerovdd = false;
    let mut updated = Vec::with_capacity(entries.len() + 1);
    for item in entries.drain(..) {
        if is_zerovdd_prep_entry(&item) {
            if !seen_zerovdd {
                seen_zerovdd = true;
                updated.push(desired.clone());
            }
        } else {
            updated.push(item);
        }
    }

    let action = if !seen_zerovdd {
        updated.push(desired);
        "add zerovdd start/stop entry to global_prep_cmd".to_string()
    } else {
        "update zerovdd entry in global_prep_cmd".to_string()
    };

    let serialized = serde_json::to_string(&updated)?;
    let changed = serialized != trimmed;
    Ok((serialized, changed, action))
}

/// Computes the updated `sunshine.conf` text, planned changes, and warnings without touching disk.
pub fn transform_sunshine_conf(
    original: &str,
    exe_path: &Path,
    clear_output_name: bool,
) -> Result<(String, Vec<String>, Vec<String>, bool)> {
    let line_ending = if original.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let had_trailing_newline = original.ends_with('\n');

    let mut out_lines: Vec<String> = Vec::new();
    let mut changes: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    let mut saw_prep = false;
    let mut saw_dd_option = false;

    for raw_line in original.lines() {
        let trimmed_start = raw_line.trim_start();
        if trimmed_start.is_empty()
            || trimmed_start.starts_with('#')
            || trimmed_start.starts_with(';')
        {
            out_lines.push(raw_line.to_string());
            continue;
        }

        let Some((raw_key, raw_val)) = raw_line.split_once('=') else {
            out_lines.push(raw_line.to_string());
            continue;
        };
        let key = raw_key.trim();
        let val = raw_val.trim();

        match key {
            "global_prep_cmd" => {
                if saw_prep {
                    changes.push("remove duplicate `global_prep_cmd` line".to_string());
                    continue;
                }
                saw_prep = true;
                let (new_json, prep_changed, action) = update_global_prep_cmd(val, exe_path)?;
                if prep_changed {
                    changes.push(action);
                    out_lines.push(format!("global_prep_cmd = {new_json}"));
                } else {
                    out_lines.push(raw_line.to_string());
                }
            }
            "dd_configuration_option" => {
                if saw_dd_option {
                    changes.push("remove duplicate `dd_configuration_option` line".to_string());
                    continue;
                }
                saw_dd_option = true;
                if val == "disabled" {
                    out_lines.push(raw_line.to_string());
                } else {
                    changes.push(format!(
                        "set `dd_configuration_option = disabled` (was `{val}`)"
                    ));
                    out_lines.push("dd_configuration_option = disabled".to_string());
                }
            }
            "output_name" => {
                if !val.is_empty() {
                    if clear_output_name {
                        warnings.push(format!(
                            "cleared pinned `output_name = {val}` so Sunshine captures the primary virtual display"
                        ));
                        changes.push(format!("clear pinned `output_name = {val}`"));
                        continue;
                    }
                    warnings.push(format!(
                        "sunshine.conf pins `output_name = {val}`, which can prevent Sunshine from capturing the virtual display; pass --clear-output-name to remove it"
                    ));
                }
                out_lines.push(raw_line.to_string());
            }
            _ => out_lines.push(raw_line.to_string()),
        }
    }

    if !saw_prep {
        let (new_json, _, action) = update_global_prep_cmd("", exe_path)?;
        changes.push(action);
        out_lines.push(format!("global_prep_cmd = {new_json}"));
    }
    if !saw_dd_option {
        changes.push("add `dd_configuration_option = disabled`".to_string());
        out_lines.push("dd_configuration_option = disabled".to_string());
    }

    let mut rendered = out_lines.join(line_ending);
    if !rendered.is_empty() && (had_trailing_newline || !changes.is_empty() || original.is_empty())
    {
        rendered.push_str(line_ending);
    }

    let changed = !changes.is_empty();
    Ok((rendered, changes, warnings, changed))
}

fn normalize_parent_dir(path: &Path) -> Result<&Path> {
    let _file_name = path
        .file_name()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("invalid config path `{}`", path.display()))?;
    match path.parent() {
        Some(p) if p.as_os_str().is_empty() => Ok(Path::new(".")),
        Some(p) => Ok(p),
        None => Ok(Path::new(".")),
    }
}

fn create_timestamped_backup(config_path: &Path, original_bytes: &[u8]) -> Result<PathBuf> {
    let parent = normalize_parent_dir(config_path)?;
    let file_name = config_path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid config file name `{}`", config_path.display()))?;
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut suffix = 0u32;
    loop {
        let candidate = if suffix == 0 {
            parent.join(format!("{file_name}.bak.{secs}"))
        } else {
            parent.join(format!("{file_name}.bak.{secs}.{suffix}"))
        };
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut f) => {
                f.write_all(original_bytes)?;
                f.flush()?;
                f.sync_all()?;
                return Ok(candidate);
            }
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                suffix = suffix.saturating_add(1);
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("failed to create backup file at {}", candidate.display())
                });
            }
        }
    }
}

fn write_text_atomic(path: &Path, content: &str) -> Result<()> {
    let parent = normalize_parent_dir(path)?;
    let mut temp = NamedTempFile::new_in(parent)?;
    temp.write_all(content.as_bytes())?;
    temp.flush()?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|err| anyhow::anyhow!("failed to persist `{}`: {}", path.display(), err.error))?;
    Ok(())
}

/// Inspects or applies `zerovdd` integration edits to `config_path`.
pub fn configure_sunshine_file(
    config_path: &Path,
    exe_path: &Path,
    apply: bool,
    clear_output_name: bool,
) -> Result<SunshineConfigureReport> {
    let parent = normalize_parent_dir(config_path)?;
    if !parent.exists() {
        bail!(
            "Sunshine config directory `{}` does not exist; pass --config <PATH> if Sunshine is installed elsewhere",
            parent.display()
        );
    }

    let (original, existed) = match fs::read_to_string(config_path) {
        Ok(text) => (text, true),
        Err(err) if err.kind() == ErrorKind::NotFound => (String::new(), false),
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "failed to read Sunshine config at {}",
                    config_path.display()
                )
            });
        }
    };

    let (updated, changes, warnings, changed) =
        transform_sunshine_conf(&original, exe_path, clear_output_name)?;

    if !apply || !changed {
        return Ok(SunshineConfigureReport {
            config_path: config_path.to_path_buf(),
            applied: false,
            changed,
            backup_path: None,
            changes,
            warnings,
        });
    }

    let backup_path = if existed {
        Some(create_timestamped_backup(config_path, original.as_bytes())?)
    } else {
        None
    };
    write_text_atomic(config_path, &updated)?;

    Ok(SunshineConfigureReport {
        config_path: config_path.to_path_buf(),
        applied: true,
        changed,
        backup_path,
        changes,
        warnings,
    })
}

fn run_configure(args: SunshineConfigureArgs) -> Result<()> {
    let config_path = resolve_sunshine_conf_path(args.config)?;
    let exe_path = std::env::current_exe().context("failed to resolve current executable path")?;
    let report =
        configure_sunshine_file(&config_path, &exe_path, args.apply, args.clear_output_name)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    for warning in &report.warnings {
        eprintln!("WARNING: {warning}");
    }

    if !report.changed {
        println!(
            "Sunshine config `{}` is already configured; no changes needed.",
            report.config_path.display()
        );
        return Ok(());
    }

    if report.applied {
        println!(
            "Updated Sunshine config at `{}`.",
            report.config_path.display()
        );
        if let Some(ref backup) = report.backup_path {
            println!("  Backup saved to: {}", backup.display());
        }
        for change in &report.changes {
            println!("  - {change}");
        }
    } else {
        println!(
            "Dry-run for `{}` (no files modified):",
            report.config_path.display()
        );
        for change in &report.changes {
            println!("  - would {change}");
        }
        println!("Pass --apply to write these changes (a timestamped backup will be created).");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn base_start_args() -> SunshineStartArgs {
        SunshineStartArgs {
            owner_pid: None,
            width: None,
            height: None,
            fps: None,
            hdr: None,
            client_id: None,
            json: false,
        }
    }

    #[test]
    fn parses_upstream_sunshine_env_with_name_fallback_and_fractional_fps() {
        let mut env = BTreeMap::new();
        env.insert("SUNSHINE_CLIENT_WIDTH".to_string(), "3840".to_string());
        env.insert("SUNSHINE_CLIENT_HEIGHT".to_string(), "2160".to_string());
        env.insert("SUNSHINE_CLIENT_FPS".to_string(), "59.94".to_string());
        env.insert("SUNSHINE_CLIENT_HDR".to_string(), "TRUE".to_string());
        env.insert(
            "SUNSHINE_CLIENT_NAME".to_string(),
            "Living Room TV".to_string(),
        );

        let parsed = parse_sunshine_session_env(&env, &base_start_args()).unwrap();
        assert_eq!(parsed.width, 3840);
        assert_eq!(parsed.height, 2160);
        assert_eq!(parsed.refresh_mhz, 59_940);
        assert!(parsed.hdr);
        assert_eq!(parsed.client_id, "Living Room TV");
    }

    #[test]
    fn prioritizes_client_uuid_and_falls_back_to_default_client_id() {
        let mut env = BTreeMap::new();
        env.insert("SUNSHINE_CLIENT_WIDTH".to_string(), "2560".to_string());
        env.insert("SUNSHINE_CLIENT_HEIGHT".to_string(), "1600".to_string());
        env.insert("SUNSHINE_CLIENT_FPS".to_string(), "119.88".to_string());
        env.insert("SUNSHINE_CLIENT_HDR".to_string(), "1".to_string());
        env.insert("SUNSHINE_CLIENT_NAME".to_string(), "Laptop".to_string());
        env.insert(
            "SUNSHINE_CLIENT_UUID".to_string(),
            "11111111-2222-3333-4444-555555555555".to_string(),
        );

        let with_uuid = parse_sunshine_session_env(&env, &base_start_args()).unwrap();
        assert_eq!(with_uuid.refresh_mhz, 119_880);
        assert!(with_uuid.hdr);
        assert_eq!(with_uuid.client_id, "11111111-2222-3333-4444-555555555555");

        env.remove("SUNSHINE_CLIENT_UUID");
        env.remove("SUNSHINE_CLIENT_NAME");
        env.insert("SUNSHINE_CLIENT_FPS".to_string(), "239.76".to_string());
        env.insert("SUNSHINE_CLIENT_HDR".to_string(), "0".to_string());
        let fallback = parse_sunshine_session_env(&env, &base_start_args()).unwrap();
        assert_eq!(fallback.refresh_mhz, 239_760);
        assert!(!fallback.hdr);
        assert_eq!(fallback.client_id, DEFAULT_SUNSHINE_CLIENT_ID);
    }

    #[test]
    fn rejects_malformed_sunshine_env_inputs() {
        let mut env = BTreeMap::new();
        assert!(parse_sunshine_session_env(&env, &base_start_args()).is_err());

        env.insert("SUNSHINE_CLIENT_WIDTH".to_string(), "1920".to_string());
        env.insert("SUNSHINE_CLIENT_HEIGHT".to_string(), "1080".to_string());
        for bad_fps in ["0", "-60", "NaN", "999999999", "60.1234", "5"] {
            env.insert("SUNSHINE_CLIENT_FPS".to_string(), bad_fps.to_string());
            assert!(
                parse_sunshine_session_env(&env, &base_start_args()).is_err(),
                "expected FPS `{bad_fps}` to fail"
            );
        }

        env.insert("SUNSHINE_CLIENT_FPS".to_string(), "60".to_string());
        for bad_hdr in ["maybe", "2", "enabled"] {
            env.insert("SUNSHINE_CLIENT_HDR".to_string(), bad_hdr.to_string());
            assert!(
                parse_sunshine_session_env(&env, &base_start_args()).is_err(),
                "expected HDR `{bad_hdr}` to fail"
            );
        }

        env.insert("SUNSHINE_CLIENT_HDR".to_string(), "false".to_string());
        env.insert("SUNSHINE_CLIENT_WIDTH".to_string(), "0".to_string());
        assert!(parse_sunshine_session_env(&env, &base_start_args()).is_err());
    }

    #[test]
    fn finds_ancestor_sunshine_skipping_wrappers_and_guarding_cycles() {
        let tree = vec![
            ProcessEntry {
                pid: 100,
                parent_pid: 4,
                exe_name: r"C:\Program Files\Sunshine\Sunshine.EXE".to_string(),
                creation_time: 100,
            },
            ProcessEntry {
                pid: 200,
                parent_pid: 100,
                exe_name: "cmd.exe".to_string(),
                creation_time: 200,
            },
            ProcessEntry {
                pid: 300,
                parent_pid: 200,
                exe_name: "conhost.exe".to_string(),
                creation_time: 300,
            },
            ProcessEntry {
                pid: 400,
                parent_pid: 300,
                exe_name: "zerovdd.exe".to_string(),
                creation_time: 400,
            },
        ];
        assert_eq!(find_ancestor_sunshine(&tree, 400), Some((100, 100)));

        let recycled_sunshine = vec![
            ProcessEntry {
                pid: 100,
                parent_pid: 4,
                exe_name: "sunshine.exe".to_string(),
                creation_time: 500,
            },
            ProcessEntry {
                pid: 200,
                parent_pid: 100,
                exe_name: "cmd.exe".to_string(),
                creation_time: 200,
            },
            ProcessEntry {
                pid: 400,
                parent_pid: 200,
                exe_name: "zerovdd.exe".to_string(),
                creation_time: 400,
            },
        ];
        assert_eq!(find_ancestor_sunshine(&recycled_sunshine, 400), None);

        let recycled_wrapper = vec![
            ProcessEntry {
                pid: 100,
                parent_pid: 4,
                exe_name: "sunshine.exe".to_string(),
                creation_time: 100,
            },
            ProcessEntry {
                pid: 200,
                parent_pid: 100,
                exe_name: "cmd.exe".to_string(),
                creation_time: 450,
            },
            ProcessEntry {
                pid: 400,
                parent_pid: 200,
                exe_name: "zerovdd.exe".to_string(),
                creation_time: 400,
            },
        ];
        assert_eq!(find_ancestor_sunshine(&recycled_wrapper, 400), None);

        let cycle = vec![
            ProcessEntry {
                pid: 10,
                parent_pid: 20,
                exe_name: "cmd.exe".to_string(),
                creation_time: 1,
            },
            ProcessEntry {
                pid: 20,
                parent_pid: 10,
                exe_name: "powershell.exe".to_string(),
                creation_time: 2,
            },
            ProcessEntry {
                pid: 30,
                parent_pid: 20,
                exe_name: "zerovdd.exe".to_string(),
                creation_time: 3,
            },
        ];
        assert_eq!(find_ancestor_sunshine(&cycle, 30), None);
    }

    #[test]
    fn sanitize_client_id_replaces_control_chars_and_truncates_utf8_boundaries() {
        assert_eq!(
            sanitize_client_id("  My\tClient\nName\x07  "),
            "My_Client_Name_"
        );
        assert_eq!(sanitize_client_id("  \n\t  "), "");

        let mut with_emoji = "a".repeat(MAX_IPC_CLIENT_ID_LEN - 2);
        with_emoji.push('🚀');
        let truncated_emoji = sanitize_client_id(&with_emoji);
        assert_eq!(truncated_emoji.len(), MAX_IPC_CLIENT_ID_LEN - 2);
        assert!(truncated_emoji.chars().all(|c| c == 'a'));

        let multi_utf8 = "é".repeat(100);
        let truncated_utf8 = sanitize_client_id(&multi_utf8);
        assert_eq!(truncated_utf8.len(), MAX_IPC_CLIENT_ID_LEN);
        assert!(truncated_utf8.is_char_boundary(truncated_utf8.len()));
    }

    #[test]
    fn resolve_owner_creation_time_handles_cached_timestamp_access_denied_and_failures() {
        let cached = resolve_owner_creation_time(987_654, || {
            panic!("cached timestamp must not invoke query")
        })
        .unwrap();
        assert_eq!(cached, 987_654);

        let queried =
            resolve_owner_creation_time(0, || ProcessCreationQuery::Timestamp(42)).unwrap();
        assert_eq!(queried, 42);

        let denied = resolve_owner_creation_time(0, || ProcessCreationQuery::AccessDenied).unwrap();
        assert_eq!(denied, 0);

        let err = resolve_owner_creation_time(0, || {
            ProcessCreationQuery::Failed("invalid parameter".to_string())
        })
        .unwrap_err();
        assert!(err.to_string().contains("invalid parameter"));
    }

    #[test]
    fn finds_ancestor_sunshine_when_system_ancestor_creation_time_is_zero() {
        let tree = vec![
            ProcessEntry {
                pid: 100,
                parent_pid: 4,
                exe_name: "sunshine.exe".to_string(),
                creation_time: 0,
            },
            ProcessEntry {
                pid: 400,
                parent_pid: 100,
                exe_name: "zerovdd.exe".to_string(),
                creation_time: 400,
            },
        ];
        assert_eq!(find_ancestor_sunshine(&tree, 400), Some((100, 0)));
    }

    #[test]
    fn owner_pid_override_validates_and_queries_live_process() {
        assert!(find_sunshine_owner(Some(0)).is_err());

        #[cfg(windows)]
        {
            let self_pid = std::process::id();
            let (pid, creation_time) = find_sunshine_owner(Some(self_pid)).unwrap();
            assert_eq!(pid, self_pid);
            assert!(creation_time > 0);
            assert!(find_sunshine_owner(Some(u32::MAX)).is_err());
        }
    }

    #[test]
    fn configure_dry_run_is_readonly_and_apply_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let conf_path = dir.path().join("sunshine.conf");
        let exe_path = Path::new(r"C:\Program Files\zerovdd\zerovdd.exe");
        let initial = concat!(
            "# User Sunshine settings\n",
            "fec_percentage = 20\n",
            "global_prep_cmd = [{\"do\":\"echo pre\",\"undo\":\"echo post\",\"elevated\":\"false\"}]\n",
            "output_name = {9acddf6d-1234-5678-9abc-def012345678}\n",
        );
        fs::write(&conf_path, initial).unwrap();

        let dry = configure_sunshine_file(&conf_path, exe_path, false, false).unwrap();
        assert!(!dry.applied);
        assert!(dry.changed);
        assert!(dry.backup_path.is_none());
        assert!(!dry.warnings.is_empty());
        assert_eq!(fs::read_to_string(&conf_path).unwrap(), initial);

        let first = configure_sunshine_file(&conf_path, exe_path, true, true).unwrap();
        assert!(first.applied);
        assert!(first.changed);
        let backup = first.backup_path.expect("backup created");
        assert_eq!(fs::read_to_string(&backup).unwrap(), initial);

        let after_first = fs::read_to_string(&conf_path).unwrap();
        assert!(after_first.contains("fec_percentage = 20"));
        assert!(after_first.contains("dd_configuration_option = disabled"));
        assert!(!after_first.contains("{9acddf6d-1234-5678-9abc-def012345678}"));

        let prep_line = after_first
            .lines()
            .find(|l| l.starts_with("global_prep_cmd = "))
            .unwrap();
        let prep_json: Value =
            serde_json::from_str(prep_line.strip_prefix("global_prep_cmd = ").unwrap()).unwrap();
        let arr = prep_json.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["do"], "echo pre");
        assert_eq!(arr[1]["elevated"], "false");

        let second = configure_sunshine_file(&conf_path, exe_path, true, true).unwrap();
        assert!(!second.applied);
        assert!(!second.changed);
        assert!(second.backup_path.is_none());
        assert_eq!(fs::read_to_string(&conf_path).unwrap(), after_first);
    }
}
