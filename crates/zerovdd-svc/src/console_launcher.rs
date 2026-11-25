//! Active console-session trampoline that executes `zerovdd.exe internal-ccd` outside Session 0.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use zerovdd_client::topology::{AdapterLuid, DisplayPathRecord, DisplayRotation};
use zerovdd_config::SunshineTopologyMode;

/// Default maximum wait time (in milliseconds) for a console-session `internal-ccd` invocation.
pub const CONSOLE_LAUNCH_TIMEOUT_MS: u32 = 15_000;

/// Errors returned while locating or launching `zerovdd.exe internal-ccd`.
#[derive(Debug, Error)]
pub enum LauncherError {
    /// No active interactive or console session is currently available.
    #[error("no active console session is available for CCD execution")]
    ConsoleSessionUnavailable,

    /// Sibling `zerovdd.exe` binary could not be located next to the service executable.
    #[error("could not locate sibling CLI executable `{0}`")]
    CliBinaryNotFound(String),

    /// Failed to serialize or deserialize `internal-ccd` payload or output JSON.
    #[error("internal-ccd JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Filesystem or temporary payload I/O error.
    #[error("launcher I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Client topology snapshot read error during display probing.
    #[error("client error while probing topology: {0}")]
    Client(#[from] zerovdd_client::ClientError),

    /// Child `zerovdd.exe internal-ccd` exceeded the execution timeout.
    #[error("console-session internal-ccd timed out after {timeout_ms} ms")]
    Timeout {
        /// Configured wait timeout in milliseconds.
        timeout_ms: u32,
    },

    /// Child `zerovdd.exe internal-ccd` exited with a non-zero exit status.
    #[error("internal-ccd exited with code {exit_code}: {output}")]
    CommandFailed {
        /// Process exit code returned by `zerovdd.exe`.
        exit_code: u32,
        /// Captured stdout/stderr diagnostics.
        output: String,
    },

    /// Underlying Win32 token/session/process launch API failed.
    #[error("{operation} failed with Win32 error {code:#010x}")]
    Win32 {
        /// Win32 function name.
        operation: &'static str,
        /// Win32 or HRESULT error code.
        code: u32,
    },
}

/// JSON payload serialized for `zerovdd.exe internal-ccd apply --payload-json <PATH>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyCcdPayload {
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
    /// Optional backup snapshot path.
    #[serde(default)]
    pub snapshot_path: Option<PathBuf>,
    /// Optional extend origin `(x, y)`.
    #[serde(default)]
    pub extend_origin: Option<(i32, i32)>,
    /// Whether the virtual display should become `(0, 0)` primary when extending.
    #[serde(default)]
    pub make_virtual_primary: bool,
    /// Clockwise rotation applied in CCD (`Identity` = 0 degrees).
    #[serde(default)]
    pub rotation: DisplayRotation,
}

/// Parameters for `zerovdd.exe internal-ccd set-mode`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetModeCcdPayload {
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
    /// Optional HDR10 override (`None` preserves current HDR state).
    pub hdr: Option<bool>,
}

/// Summary parsed from `zerovdd.exe internal-ccd restore` stdout JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreCcdReport {
    /// `true` if a topology backup file existed and was restored.
    pub snapshot_found: bool,
    /// Number of physical paths restored as active.
    pub restored_path_count: usize,
    /// Number of physical targets skipped because they were unplugged.
    pub skipped_unplugged_targets: usize,
}

/// Abstraction over Session-0 to console-session `internal-ccd` execution.
pub trait ConsoleCcdRunner: Send + Sync {
    /// Returns the currently active console session ID (`Session > 0`), or `None` if unavailable.
    fn console_session_id(&self) -> Option<u32>;

    /// Captures physical display topology into `snapshot_path` inside the console session.
    fn snapshot(&self, snapshot_path: &Path, force_overwrite: bool) -> Result<(), LauncherError>;

    /// Applies virtual streaming topology and HDR state inside the console session.
    fn apply(&self, payload: &ApplyCcdPayload) -> Result<Option<DisplayPathRecord>, LauncherError>;

    /// Updates mode and optional HDR state on a virtual display in place inside the console session.
    fn set_mode(
        &self,
        payload: &SetModeCcdPayload,
    ) -> Result<Option<DisplayPathRecord>, LauncherError>;

    /// Restores physical display topology from `snapshot_path` inside the console session.
    fn restore(
        &self,
        snapshot_path: &Path,
        virtual_target: Option<(AdapterLuid, u32)>,
    ) -> Result<RestoreCcdReport, LauncherError>;

    /// Restores physical display topology from `snapshot_path`, optionally falling back to
    /// re-enabling attached physical displays if no backup snapshot exists.
    fn restore_with_fallback(
        &self,
        snapshot_path: &Path,
        virtual_target: Option<(AdapterLuid, u32)>,
        _enable_physical_fallback: bool,
    ) -> Result<RestoreCcdReport, LauncherError> {
        self.restore(snapshot_path, virtual_target)
    }

    /// Probes whether at least one active display currently exists in the console session.
    fn probe_has_active_displays(&self) -> Result<bool, LauncherError>;
}

/// Resolves the sibling `zerovdd.exe` path in the same directory as `service_exe`.
pub fn resolve_sibling_cli_path(service_exe: &Path) -> Result<PathBuf, LauncherError> {
    let dir = service_exe
        .parent()
        .ok_or_else(|| LauncherError::CliBinaryNotFound(service_exe.display().to_string()))?;
    let candidate = dir.join(if cfg!(windows) {
        "zerovdd.exe"
    } else {
        "zerovdd"
    });
    if candidate.is_file() {
        Ok(candidate)
    } else {
        Err(LauncherError::CliBinaryNotFound(
            candidate.display().to_string(),
        ))
    }
}

#[cfg(windows)]
mod win32 {
    use core::ffi::c_void;
    use core::mem::size_of;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::thread;

    use tempfile::{NamedTempFile, TempDir};
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, DuplicateHandle, GetLastError, SetHandleInformation, DUPLICATE_SAME_ACCESS,
        HANDLE, HANDLE_FLAGS, HANDLE_FLAG_INHERIT, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows::Win32::Security::{
        DuplicateTokenEx, SecurityIdentification, SetTokenInformation, TokenPrimary,
        TokenSessionId, SECURITY_ATTRIBUTES, TOKEN_ACCESS_MASK, TOKEN_ADJUST_DEFAULT,
        TOKEN_ADJUST_SESSIONID, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_QUERY,
    };
    use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
    use windows::Win32::System::Pipes::CreatePipe;
    use windows::Win32::System::RemoteDesktop::{
        ProcessIdToSessionId, WTSActive, WTSConnectState, WTSConnected, WTSFreeMemory,
        WTSGetActiveConsoleSessionId, WTSQuerySessionInformationW, WTS_CONNECTSTATE_CLASS,
        WTS_CURRENT_SERVER_HANDLE,
    };
    use windows::Win32::System::Threading::{
        CreateProcessAsUserW, GetCurrentProcess, GetCurrentProcessId, GetExitCodeProcess,
        OpenProcessToken, TerminateProcess, WaitForSingleObject, CREATE_NO_WINDOW,
        CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOW,
    };
    use zerovdd_client::topology::{load_snapshot, AdapterLuid, DisplayPathRecord};

    use super::{
        resolve_sibling_cli_path, ApplyCcdPayload, ConsoleCcdRunner, LauncherError,
        RestoreCcdReport, SetModeCcdPayload, CONSOLE_LAUNCH_TIMEOUT_MS,
    };

    const INVALID_SESSION_ID: u32 = 0xFFFF_FFFF;

    struct OwnedHandle(HANDLE);

    // SAFETY: Win32 handles owned by `OwnedHandle` can be moved between threads and closed once.
    unsafe impl Send for OwnedHandle {}

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_invalid() && self.0 != HANDLE::default() {
                // SAFETY: `self.0` is an owned Win32 handle closed exactly once on drop.
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    struct EnvBlockGuard(*mut c_void);

    impl Drop for EnvBlockGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `self.0` was allocated by `CreateEnvironmentBlock` and is freed once.
                unsafe {
                    let _ = DestroyEnvironmentBlock(self.0);
                }
            }
        }
    }

    struct WtsMemGuard(*mut c_void);

    impl Drop for WtsMemGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `self.0` was allocated by `WTSQuerySessionInformationW`.
                unsafe {
                    WTSFreeMemory(self.0);
                }
            }
        }
    }

    fn last_win32_error(operation: &'static str) -> LauncherError {
        // SAFETY: `GetLastError` reads the calling thread's last-error slot.
        let err = unsafe { GetLastError() };
        LauncherError::Win32 {
            operation,
            code: err.0,
        }
    }

    fn map_win32_err(operation: &'static str, err: &windows::core::Error) -> LauncherError {
        LauncherError::Win32 {
            operation,
            code: err.code().0 as u32,
        }
    }

    fn to_wide_nul(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(core::iter::once(0)).collect()
    }

    fn quote_windows_arg(arg: &str) -> String {
        if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
            return arg.to_string();
        }
        let mut out = String::from("\"");
        let mut backslashes = 0usize;
        for ch in arg.chars() {
            if ch == '\\' {
                backslashes += 1;
            } else if ch == '"' {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            } else {
                out.extend(std::iter::repeat_n('\\', backslashes));
                backslashes = 0;
                out.push(ch);
            }
        }
        out.extend(std::iter::repeat_n('\\', backslashes * 2));
        out.push('"');
        out
    }

    fn build_command_line(exe: &Path, args: &[String]) -> Vec<u16> {
        let mut cmd = quote_windows_arg(&exe.to_string_lossy());
        for arg in args {
            cmd.push(' ');
            cmd.push_str(&quote_windows_arg(arg));
        }
        to_wide_nul(&cmd)
    }

    fn current_process_session_id() -> Option<u32> {
        let mut session_id: u32 = 0;
        // SAFETY: `GetCurrentProcessId` returns the current PID and `session_id` is stack-local.
        let res = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session_id) };
        if res.is_ok() {
            Some(session_id)
        } else {
            None
        }
    }

    /// Production Windows console-session CCD launcher using `CreateProcessAsUserW`.
    #[derive(Debug, Clone)]
    pub struct WindowsConsoleLauncher {
        cli_exe_override: Option<PathBuf>,
    }

    impl Default for WindowsConsoleLauncher {
        fn default() -> Self {
            Self::new()
        }
    }

    impl WindowsConsoleLauncher {
        /// Creates a launcher that resolves `zerovdd.exe` adjacent to `current_exe()`.
        pub const fn new() -> Self {
            Self {
                cli_exe_override: None,
            }
        }

        /// Creates a launcher targeting an explicit `zerovdd.exe` path.
        pub const fn with_cli_path(cli_exe: PathBuf) -> Self {
            Self {
                cli_exe_override: Some(cli_exe),
            }
        }

        fn cli_exe_path(&self) -> Result<PathBuf, LauncherError> {
            if let Some(ref explicit) = self.cli_exe_override {
                if explicit.is_file() {
                    return Ok(explicit.clone());
                }
                return Err(LauncherError::CliBinaryNotFound(
                    explicit.display().to_string(),
                ));
            }
            let current = std::env::current_exe()?;
            resolve_sibling_cli_path(&current)
        }

        fn run_direct_in_current_session(
            cli_exe: &Path,
            args: &[String],
        ) -> Result<String, LauncherError> {
            let mut child = Command::new(cli_exe)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;

            let stdout_pipe = child.stdout.take();
            let stderr_pipe = child.stderr.take();

            let stdout_thread = thread::spawn(move || -> Vec<u8> {
                let mut buf = Vec::new();
                if let Some(mut r) = stdout_pipe {
                    let _ = r.read_to_end(&mut buf);
                }
                buf
            });
            let stderr_thread = thread::spawn(move || -> Vec<u8> {
                let mut buf = Vec::new();
                if let Some(mut r) = stderr_pipe {
                    let _ = r.read_to_end(&mut buf);
                }
                buf
            });

            let proc_handle = HANDLE(child.as_raw_handle());
            // SAFETY: `proc_handle` is borrowed from live `child` for `WaitForSingleObject`.
            let wait_res = unsafe { WaitForSingleObject(proc_handle, CONSOLE_LAUNCH_TIMEOUT_MS) };
            if wait_res == WAIT_TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return Err(LauncherError::Timeout {
                    timeout_ms: CONSOLE_LAUNCH_TIMEOUT_MS,
                });
            }
            if wait_res != WAIT_OBJECT_0 {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return Err(last_win32_error("WaitForSingleObject"));
            }

            let status = child.wait()?;
            let stdout_bytes = stdout_thread.join().unwrap_or_default();
            let stderr_bytes = stderr_thread.join().unwrap_or_default();
            let stdout = String::from_utf8_lossy(&stdout_bytes).trim().to_string();
            let stderr = String::from_utf8_lossy(&stderr_bytes).trim().to_string();

            if !status.success() {
                let code = status.code().map_or(1, |c| c as u32);
                let output = if stderr.is_empty() { stdout } else { stderr };
                return Err(LauncherError::CommandFailed {
                    exit_code: code,
                    output,
                });
            }
            Ok(stdout)
        }

        fn launch_retargeted_on_desktop(
            cli_exe: &Path,
            args: &[String],
            session_id: u32,
            desktop: &str,
        ) -> Result<(u32, String), LauncherError> {
            let mut proc_token_raw = HANDLE::default();
            let desired_access: TOKEN_ACCESS_MASK = TOKEN_DUPLICATE
                | TOKEN_QUERY
                | TOKEN_ASSIGN_PRIMARY
                | TOKEN_ADJUST_SESSIONID
                | TOKEN_ADJUST_DEFAULT;
            // SAFETY: `GetCurrentProcess()` is a pseudo-handle; `proc_token_raw` receives the token handle.
            unsafe { OpenProcessToken(GetCurrentProcess(), desired_access, &mut proc_token_raw) }
                .map_err(|err| map_win32_err("OpenProcessToken", &err))?;
            let proc_token = OwnedHandle(proc_token_raw);

            let mut dup_token_raw = HANDLE::default();
            // SAFETY: `proc_token.0` is an open process token duplicated into a primary token.
            unsafe {
                DuplicateTokenEx(
                    proc_token.0,
                    TOKEN_ACCESS_MASK(0x000F_01FF),
                    None,
                    SecurityIdentification,
                    TokenPrimary,
                    &mut dup_token_raw,
                )
            }
            .map_err(|err| map_win32_err("DuplicateTokenEx", &err))?;
            let dup_token = OwnedHandle(dup_token_raw);

            // SAFETY: `dup_token.0` is a valid primary token and `session_id` is a valid `u32`.
            unsafe {
                SetTokenInformation(
                    dup_token.0,
                    TokenSessionId,
                    (&raw const session_id).cast::<c_void>(),
                    size_of::<u32>() as u32,
                )
            }
            .map_err(|err| map_win32_err("SetTokenInformation(TokenSessionId)", &err))?;

            let mut env_ptr: *mut c_void = core::ptr::null_mut();
            // SAFETY: `dup_token.0` is valid; if successful `EnvBlockGuard` frees `env_ptr`.
            let env_guard = if unsafe {
                CreateEnvironmentBlock(&mut env_ptr, Some(dup_token.0), false)
            }
            .is_ok()
            {
                Some(EnvBlockGuard(env_ptr))
            } else {
                None
            };

            let sa = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: core::ptr::null_mut(),
                bInheritHandle: true.into(),
            };
            let mut read_pipe_raw = HANDLE::default();
            let mut write_pipe_raw = HANDLE::default();
            // SAFETY: `sa` and pipe out-pointers are stack-valid for `CreatePipe`.
            unsafe { CreatePipe(&mut read_pipe_raw, &mut write_pipe_raw, Some(&sa), 0) }
                .map_err(|err| map_win32_err("CreatePipe", &err))?;
            let read_pipe = OwnedHandle(read_pipe_raw);
            let write_stdout = OwnedHandle(write_pipe_raw);

            // SAFETY: Prevent child process from inheriting the read end so EOF occurs on child exit.
            unsafe { SetHandleInformation(read_pipe.0, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) }
                .map_err(|err| map_win32_err("SetHandleInformation", &err))?;

            let mut write_stderr_raw = HANDLE::default();
            // SAFETY: Duplicate `write_stdout.0` so stdout and stderr can both be safely passed.
            unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    write_stdout.0,
                    GetCurrentProcess(),
                    &mut write_stderr_raw,
                    0,
                    true,
                    DUPLICATE_SAME_ACCESS,
                )
            }
            .map_err(|err| map_win32_err("DuplicateHandle", &err))?;
            let write_stderr = OwnedHandle(write_stderr_raw);

            let mut desktop_wide = to_wide_nul(desktop);
            let mut cmd_wide = build_command_line(cli_exe, args);
            let exe_wide = to_wide_nul(&cli_exe.to_string_lossy());

            let startup_info = STARTUPINFOW {
                cb: size_of::<STARTUPINFOW>() as u32,
                lpDesktop: PWSTR(desktop_wide.as_mut_ptr()),
                dwFlags: STARTF_USESTDHANDLES,
                hStdOutput: write_stdout.0,
                hStdError: write_stderr.0,
                ..Default::default()
            };
            let mut proc_info = PROCESS_INFORMATION::default();
            let mut flags = CREATE_NO_WINDOW;
            let env_block = if let Some(ref guard) = env_guard {
                flags |= CREATE_UNICODE_ENVIRONMENT;
                Some(guard.0.cast_const())
            } else {
                None
            };

            // SAFETY: All pointers (`dup_token`, `exe_wide`, `cmd_wide`, `env_block`, `startup_info`,
            // and `proc_info`) remain valid for the duration of `CreateProcessAsUserW`.
            let create_res = unsafe {
                CreateProcessAsUserW(
                    Some(dup_token.0),
                    PCWSTR(exe_wide.as_ptr()),
                    Some(PWSTR(cmd_wide.as_mut_ptr())),
                    None,
                    None,
                    true,
                    flags,
                    env_block,
                    PCWSTR::null(),
                    &startup_info,
                    &mut proc_info,
                )
            };

            drop(write_stdout);
            drop(write_stderr);

            if let Err(err) = create_res {
                return Err(map_win32_err("CreateProcessAsUserW", &err));
            }

            let child_process = OwnedHandle(proc_info.hProcess);
            let _child_thread = OwnedHandle(proc_info.hThread);

            // SAFETY: `read_pipe` ownership transfers to `File` and is closed when `reader_thread` finishes.
            let mut pipe_file = unsafe { File::from_raw_handle(read_pipe.0 .0) };
            core::mem::forget(read_pipe);
            let reader_thread = thread::spawn(move || -> Vec<u8> {
                let mut out = Vec::new();
                let _ = pipe_file.read_to_end(&mut out);
                out
            });

            // SAFETY: `child_process.0` is an open process handle for `WaitForSingleObject`.
            let wait_res =
                unsafe { WaitForSingleObject(child_process.0, CONSOLE_LAUNCH_TIMEOUT_MS) };
            if wait_res == WAIT_TIMEOUT {
                // SAFETY: Child timed out and must be terminated before returning error.
                unsafe {
                    let _ = TerminateProcess(child_process.0, 1);
                }
                let _ = reader_thread.join();
                return Err(LauncherError::Timeout {
                    timeout_ms: CONSOLE_LAUNCH_TIMEOUT_MS,
                });
            }
            if wait_res != WAIT_OBJECT_0 {
                // SAFETY: Terminate orphaned child if wait failed unexpectedly.
                unsafe {
                    let _ = TerminateProcess(child_process.0, 1);
                }
                let _ = reader_thread.join();
                return Err(last_win32_error("WaitForSingleObject"));
            }

            let mut exit_code: u32 = 1;
            // SAFETY: `child_process.0` has exited and `exit_code` is a valid stack `u32`.
            unsafe { GetExitCodeProcess(child_process.0, &mut exit_code) }
                .map_err(|err| map_win32_err("GetExitCodeProcess", &err))?;

            let output_bytes = reader_thread.join().unwrap_or_default();
            let output = String::from_utf8_lossy(&output_bytes).trim().to_string();
            Ok((exit_code, output))
        }

        fn execute_internal_ccd(&self, args: &[String]) -> Result<String, LauncherError> {
            let cli_exe = self.cli_exe_path()?;
            let target_session = self
                .console_session_id()
                .ok_or(LauncherError::ConsoleSessionUnavailable)?;

            if let Some(current_session) = current_process_session_id() {
                if current_session != 0 && current_session == target_session {
                    return Self::run_direct_in_current_session(&cli_exe, args);
                }
            }

            let mut last_err = None;
            for desktop in [r"winsta0\default", r"winsta0\winlogon"] {
                match Self::launch_retargeted_on_desktop(&cli_exe, args, target_session, desktop) {
                    Ok((0, output)) => return Ok(output),
                    Ok((exit_code, output)) => {
                        let cmd_err = LauncherError::CommandFailed { exit_code, output };
                        if desktop == r"winsta0\default" {
                            last_err = Some(cmd_err);
                            continue;
                        }
                        return Err(cmd_err);
                    }
                    Err(err) => {
                        if last_err.is_none() || desktop == r"winsta0\default" {
                            last_err = Some(err);
                        }
                    }
                }
            }

            Err(last_err.unwrap_or(LauncherError::ConsoleSessionUnavailable))
        }
    }

    impl ConsoleCcdRunner for WindowsConsoleLauncher {
        fn console_session_id(&self) -> Option<u32> {
            // SAFETY: `WTSGetActiveConsoleSessionId` takes no pointers and returns session ID or `0xFFFFFFFF`.
            let session_id = unsafe { WTSGetActiveConsoleSessionId() };
            if session_id == 0 || session_id == INVALID_SESSION_ID {
                return None;
            }

            let mut buf_ptr: PWSTR = PWSTR::null();
            let mut bytes_returned: u32 = 0;
            // SAFETY: `WTS_CURRENT_SERVER_HANDLE` queries the local session; `buf_ptr` is freed by `WtsMemGuard`.
            let query_res = unsafe {
                WTSQuerySessionInformationW(
                    Some(WTS_CURRENT_SERVER_HANDLE),
                    session_id,
                    WTSConnectState,
                    &mut buf_ptr,
                    &mut bytes_returned,
                )
            };
            if query_res.is_ok() && !buf_ptr.is_null() {
                let _guard = WtsMemGuard(buf_ptr.0.cast::<c_void>());
                if (bytes_returned as usize) >= size_of::<WTS_CONNECTSTATE_CLASS>() {
                    // SAFETY: `WTSQuerySessionInformationW` guarantees at least `bytes_returned` bytes.
                    let state = unsafe { *buf_ptr.0.cast::<WTS_CONNECTSTATE_CLASS>() };
                    if state != WTSActive && state != WTSConnected {
                        return None;
                    }
                }
            }

            Some(session_id)
        }

        fn snapshot(
            &self,
            snapshot_path: &Path,
            force_overwrite: bool,
        ) -> Result<(), LauncherError> {
            let mut args = vec![
                "internal-ccd".to_string(),
                "snapshot".to_string(),
                "--snapshot-path".to_string(),
                snapshot_path.to_string_lossy().into_owned(),
            ];
            if force_overwrite {
                args.push("--force-overwrite".to_string());
            }
            let _ = self.execute_internal_ccd(&args)?;
            Ok(())
        }

        fn apply(
            &self,
            payload: &ApplyCcdPayload,
        ) -> Result<Option<DisplayPathRecord>, LauncherError> {
            let mut temp = NamedTempFile::new()?;
            let encoded = serde_json::to_vec(payload)?;
            temp.write_all(&encoded)?;
            temp.flush()?;
            let temp_path = temp.path().to_string_lossy().into_owned();

            let args = vec![
                "internal-ccd".to_string(),
                "apply".to_string(),
                "--payload-json".to_string(),
                temp_path,
                "--no-snapshot".to_string(),
            ];
            let stdout = self.execute_internal_ccd(&args)?;
            let last_line = stdout
                .lines()
                .rev()
                .map(str::trim)
                .find(|line| !line.is_empty());
            match last_line {
                Some(json_line) => Ok(serde_json::from_str::<DisplayPathRecord>(json_line).ok()),
                None => Ok(None),
            }
        }

        fn restore_with_fallback(
            &self,
            snapshot_path: &Path,
            virtual_target: Option<(AdapterLuid, u32)>,
            enable_physical_fallback: bool,
        ) -> Result<RestoreCcdReport, LauncherError> {
            let mut args = vec![
                "internal-ccd".to_string(),
                "restore".to_string(),
                "--snapshot-path".to_string(),
                snapshot_path.to_string_lossy().into_owned(),
            ];
            if let Some((luid, target_id)) = virtual_target {
                args.push("--virtual-luid-low".to_string());
                args.push(luid.low.to_string());
                args.push("--virtual-luid-high".to_string());
                args.push(luid.high.to_string());
                args.push("--virtual-target-id".to_string());
                args.push(target_id.to_string());
            }
            if enable_physical_fallback {
                args.push("--fallback-enable-physical".to_string());
            }
            let stdout = self.execute_internal_ccd(&args)?;
            let last_line = stdout
                .lines()
                .rev()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("{}");
            Ok(serde_json::from_str(last_line)?)
        }

        fn restore(
            &self,
            snapshot_path: &Path,
            virtual_target: Option<(AdapterLuid, u32)>,
        ) -> Result<RestoreCcdReport, LauncherError> {
            self.restore_with_fallback(snapshot_path, virtual_target, false)
        }

        fn probe_has_active_displays(&self) -> Result<bool, LauncherError> {
            let dir = TempDir::new()?;
            let probe_path = dir.path().join("probe.json");
            self.snapshot(&probe_path, true)?;
            let Some(snap) = load_snapshot(&probe_path)? else {
                return Ok(false);
            };
            Ok(snap.paths.iter().any(|p| p.active))
        }

        fn set_mode(
            &self,
            payload: &SetModeCcdPayload,
        ) -> Result<Option<DisplayPathRecord>, LauncherError> {
            let mut args = vec![
                "internal-ccd".to_string(),
                "set-mode".to_string(),
                "--luid-low".to_string(),
                payload.adapter_luid_low.to_string(),
                "--luid-high".to_string(),
                payload.adapter_luid_high.to_string(),
                "--target-id".to_string(),
                payload.target_id.to_string(),
                "--width".to_string(),
                payload.width.to_string(),
                "--height".to_string(),
                payload.height.to_string(),
                "--refresh-num".to_string(),
                payload.refresh_num.to_string(),
                "--refresh-den".to_string(),
                payload.refresh_den.to_string(),
            ];
            if let Some(hdr) = payload.hdr {
                args.push("--hdr".to_string());
                args.push(hdr.to_string());
            }
            let stdout = self.execute_internal_ccd(&args)?;
            let last_line = stdout
                .lines()
                .rev()
                .map(str::trim)
                .find(|line| !line.is_empty());
            match last_line {
                Some(json_line) => Ok(serde_json::from_str::<DisplayPathRecord>(json_line).ok()),
                None => Ok(None),
            }
        }
    }
}

#[cfg(windows)]
pub use win32::WindowsConsoleLauncher;

/// Non-Windows stub [`ConsoleCcdRunner`] for cross-platform compilation and diagnostics.
#[cfg(not(windows))]
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedConsoleLauncher;

#[cfg(not(windows))]
impl ConsoleCcdRunner for UnsupportedConsoleLauncher {
    fn console_session_id(&self) -> Option<u32> {
        None
    }

    fn snapshot(&self, _snapshot_path: &Path, _force_overwrite: bool) -> Result<(), LauncherError> {
        Err(LauncherError::ConsoleSessionUnavailable)
    }

    fn apply(
        &self,
        _payload: &ApplyCcdPayload,
    ) -> Result<Option<DisplayPathRecord>, LauncherError> {
        Err(LauncherError::ConsoleSessionUnavailable)
    }

    fn set_mode(
        &self,
        _payload: &SetModeCcdPayload,
    ) -> Result<Option<DisplayPathRecord>, LauncherError> {
        Err(LauncherError::ConsoleSessionUnavailable)
    }

    fn restore(
        &self,
        _snapshot_path: &Path,
        _virtual_target: Option<(AdapterLuid, u32)>,
    ) -> Result<RestoreCcdReport, LauncherError> {
        Err(LauncherError::ConsoleSessionUnavailable)
    }

    fn probe_has_active_displays(&self) -> Result<bool, LauncherError> {
        Err(LauncherError::ConsoleSessionUnavailable)
    }
}
