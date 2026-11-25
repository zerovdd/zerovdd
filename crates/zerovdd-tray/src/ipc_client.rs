//! Synchronous named-pipe IPC client over `\\.\pipe\zerovdd-svc` for `zerovdd-tray`.

use anyhow::{bail, Result};
use zerovdd_proto::ipc::{IpcRequest, IpcResponse};

use crate::model::TraySnapshot;

/// Sends a single synchronous IPC request to `zerovdd-svc` with a bounded connect timeout.
///
/// Returns `Ok(None)` when `zerovdd-svc` is offline or unreachable so the tray remains
/// responsive in degraded states without blocking UI message dispatch.
#[cfg(windows)]
pub fn send_ipc_request(req: &IpcRequest) -> Result<Option<IpcResponse>> {
    use std::io::{Read, Write};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_PIPE_BUSY, GENERIC_READ,
        GENERIC_WRITE, HANDLE,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FlushFileBuffers, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_NONE,
        OPEN_EXISTING,
    };
    use windows::Win32::System::Pipes::{
        SetNamedPipeHandleState, WaitNamedPipeW, PIPE_READMODE_BYTE,
    };
    use zerovdd_proto::ipc::{read_frame, write_frame, IpcEnvelope, IPC_PIPE_NAME};

    struct PipeHandle(HANDLE);

    impl Drop for PipeHandle {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: `self.0` was opened via `CreateFileW` and is closed once on drop.
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    impl Read for PipeHandle {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let mut bytes_read: u32 = 0;
            // SAFETY: `self.0` is an open pipe handle and `buf` is valid for `buf.len()` bytes.
            unsafe { ReadFile(self.0, Some(buf), Some(&mut bytes_read), None) }
                .map_err(|err| std::io::Error::from_raw_os_error(err.code().0))?;
            Ok(bytes_read as usize)
        }
    }

    impl Write for PipeHandle {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let mut written: u32 = 0;
            // SAFETY: `self.0` is an open pipe handle and `buf` is valid for `buf.len()` bytes.
            unsafe { WriteFile(self.0, Some(buf), Some(&mut written), None) }
                .map_err(|err| std::io::Error::from_raw_os_error(err.code().0))?;
            Ok(written as usize)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            // SAFETY: `self.0` is an open pipe handle.
            unsafe { FlushFileBuffers(self.0) }
                .map_err(|err| std::io::Error::from_raw_os_error(err.code().0))
        }
    }

    let wide: Vec<u16> = IPC_PIPE_NAME
        .encode_utf16()
        .chain(core::iter::once(0))
        .collect();

    let mut opened = None;
    for _ in 0..3 {
        // SAFETY: `wide` points to a NUL-terminated UTF-16 string naming the service pipe.
        match unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        } {
            Ok(h) => {
                opened = Some(h);
                break;
            }
            Err(err) => {
                let code = err.code().0 as u32 & 0xFFFF;
                if code == ERROR_FILE_NOT_FOUND.0 || code == ERROR_PATH_NOT_FOUND.0 {
                    return Ok(None);
                }
                if code == ERROR_PIPE_BUSY.0 {
                    // SAFETY: `wide` is NUL-terminated UTF-16; waits up to 1000 ms for pipe instance.
                    let _ = unsafe { WaitNamedPipeW(PCWSTR(wide.as_ptr()), 1000) };
                    continue;
                }
                return Err(err.into());
            }
        }
    }

    let Some(handle) = opened else {
        return Ok(None);
    };
    let mode = PIPE_READMODE_BYTE;
    // SAFETY: `handle` is an open pipe handle and `&mode` is a valid stack `u32`.
    let _ = unsafe { SetNamedPipeHandleState(handle, Some(&mode), None, None) };

    let mut stream = PipeHandle(handle);
    let envelope = IpcEnvelope::new(1, req.clone());
    write_frame(&mut stream, &envelope)?;
    let reply: IpcEnvelope<IpcResponse> = read_frame(&mut stream)?;
    reply.validate_version()?;
    Ok(Some(reply.payload))
}

/// Non-Windows stub returning `Ok(None)` cleanly for cross-platform checks.
#[cfg(not(windows))]
pub fn send_ipc_request(_req: &IpcRequest) -> Result<Option<IpcResponse>> {
    Ok(None)
}

/// Sends an IPC request expecting an acknowledged non-error response.
pub fn send_ipc_checked(req: &IpcRequest) -> Result<IpcResponse> {
    let Some(resp) = send_ipc_request(req)? else {
        bail!(
            "zerovdd-svc is not reachable over {}",
            zerovdd_proto::ipc::IPC_PIPE_NAME
        );
    };
    if let IpcResponse::Error(err) = resp {
        bail!("zerovdd-svc error [{}]: {}", err.code, err.message);
    }
    Ok(resp)
}

/// Queries live health, session status, active displays, and GPU list from `zerovdd-svc`.
pub fn fetch_snapshot() -> TraySnapshot {
    let mut snapshot = TraySnapshot::default();
    let Ok(Some(IpcResponse::Health(health))) = send_ipc_request(&IpcRequest::GetHealth) else {
        return snapshot;
    };
    snapshot.service_online = true;
    snapshot.health = Some(health);

    if let Ok(Some(IpcResponse::SessionStatus(sess))) =
        send_ipc_request(&IpcRequest::GetSessionStatus)
    {
        snapshot.session = Some(sess);
    }
    if let Ok(Some(IpcResponse::DisplayList { displays })) =
        send_ipc_request(&IpcRequest::DisplayList)
    {
        snapshot.displays = displays;
    }
    if let Ok(Some(IpcResponse::GpuList {
        gpus,
        configured_selector,
        last_gpu_switch_failed,
    })) = send_ipc_request(&IpcRequest::ListGpus)
    {
        snapshot.gpus = gpus;
        snapshot.configured_gpu_selector = configured_selector;
        snapshot.last_gpu_switch_failed = last_gpu_switch_failed;
    }

    snapshot
}
