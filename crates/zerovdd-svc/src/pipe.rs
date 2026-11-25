//! ACL-protected Windows named pipe server and portable length-prefixed JSON IPC dispatcher.

use std::io::{Read, Write};

use zerovdd_proto::ipc::{
    read_frame, write_frame, IpcEnvelope, IpcError, IpcErrorPayload, IpcRequest, IpcResponse,
};

/// Strict SDDL DACL applied to `\\.\pipe\zerovdd-svc`:
/// `SYSTEM` (full control), `Builtin Administrators` (full control), and
/// `Interactive Logged-On Users` (read/write only, never `Everyone`).
pub const IPC_PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)";

/// Portable handler trait invoked by the named-pipe IPC dispatcher.
pub trait IpcHandler: Send + Sync {
    /// Processes a validated [`IpcRequest`] and produces an [`IpcResponse`].
    fn handle(&self, request: IpcRequest) -> IpcResponse;
}

fn validate_request(request: &IpcRequest) -> Result<(), IpcError> {
    match request {
        IpcRequest::StartSession(req) => req.validate(),
        IpcRequest::StopSession(req) => req.validate(),
        IpcRequest::DisplayAdd(req) => req.validate(),
        IpcRequest::DisplayRemove(req) => req.validate(),
        IpcRequest::DisplaySetMode(req) => req.validate(),
        IpcRequest::SwitchGpu(req) => req.validate(),
        IpcRequest::GetSessionStatus
        | IpcRequest::GetHealth
        | IpcRequest::DisplayList
        | IpcRequest::ListGpus
        | IpcRequest::RestoreTopology => Ok(()),
    }
}

fn map_ipc_validation_error(err: &IpcError) -> IpcErrorPayload {
    let code = match err {
        IpcError::UnsupportedVersion { .. } => "unsupported_protocol_version",
        IpcError::InvalidClientId => "invalid_client_id",
        IpcError::InvalidPayload(_) => "invalid_payload",
        _ => "invalid_request",
    };
    IpcErrorPayload {
        code: code.to_string(),
        message: err.to_string(),
    }
}

/// Reads one length-prefixed JSON IPC request frame from `stream`, dispatches it through
/// `handler`, and writes the matching versioned response frame.
pub fn handle_pipe_stream<S: Read + Write>(
    stream: &mut S,
    handler: &dyn IpcHandler,
) -> Result<(), IpcError> {
    let envelope: IpcEnvelope<IpcRequest> = read_frame(stream)?;
    if let Err(err) = envelope.validate_version() {
        let reply = IpcEnvelope::new(
            envelope.request_id,
            IpcResponse::Error(map_ipc_validation_error(&err)),
        );
        write_frame(stream, &reply)?;
        return Ok(());
    }

    let response = if let Err(err) = validate_request(&envelope.payload) {
        IpcResponse::Error(map_ipc_validation_error(&err))
    } else {
        handler.handle(envelope.payload)
    };

    let reply = IpcEnvelope::new(envelope.request_id, response);
    write_frame(stream, &reply)?;
    Ok(())
}

#[cfg(windows)]
mod win32 {
    use core::mem::size_of;
    use std::io::{ErrorKind, Read, Write};
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use windows::core::{HRESULT, PCWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, LocalFree, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA,
        ERROR_OPERATION_ABORTED, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, HANDLE, HLOCAL,
        INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    };
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows::Win32::Storage::FileSystem::{
        ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
        PIPE_ACCESS_DUPLEX,
    };
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows::Win32::System::Threading::{
        CreateEventW, ResetEvent, SetEvent, WaitForMultipleObjects, WaitForSingleObject, INFINITE,
    };
    use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use zerovdd_proto::ipc::{IpcError, IPC_PIPE_NAME};

    use super::{handle_pipe_stream, IpcHandler, IPC_PIPE_SDDL};

    const PIPE_BUFFER_SIZE: u32 = 65_536;
    const PIPE_SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

    struct OwnedHandle(HANDLE);

    // SAFETY: Win32 kernel event/pipe handles owned uniquely by `OwnedHandle` may move across threads.
    unsafe impl Send for OwnedHandle {}
    // SAFETY: Win32 kernel handles are opaque kernel object references safe to share across threads.
    unsafe impl Sync for OwnedHandle {}

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_invalid() && self.0 != HANDLE::default() {
                // SAFETY: `self.0` was created by `CreateEventW` or `CreateNamedPipeW` and is closed once.
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    struct SecurityDescriptorGuard(PSECURITY_DESCRIPTOR);

    impl Drop for SecurityDescriptorGuard {
        fn drop(&mut self) {
            if !self.0 .0.is_null() {
                // SAFETY: `self.0.0` was allocated by `ConvertStringSecurityDescriptorToSecurityDescriptorW`.
                unsafe {
                    let _ = LocalFree(Some(HLOCAL(self.0 .0)));
                }
            }
        }
    }

    /// Thread-safe manual-reset Win32 event handle used to cancel the named-pipe accept loop.
    #[derive(Clone)]
    pub struct ShutdownEvent {
        raw: Arc<OwnedHandle>,
    }

    // SAFETY: `ShutdownEvent` wraps an immutable Win32 manual-reset event `HANDLE` signaled via `SetEvent`.
    unsafe impl Sync for ShutdownEvent {}

    impl ShutdownEvent {
        /// Creates a new unsignaled manual-reset Win32 shutdown event.
        pub fn new() -> std::io::Result<Self> {
            // SAFETY: Stack-free manual-reset event creation with default security attributes.
            let handle = unsafe { CreateEventW(None, true, false, None) }
                .map_err(|err| std::io::Error::from_raw_os_error(err.code().0))?;
            Ok(Self {
                raw: Arc::new(OwnedHandle(handle)),
            })
        }

        /// Signals shutdown, waking any blocked pipe accept or I/O wait immediately.
        pub fn signal(&self) {
            // SAFETY: `self.raw.0` is an open Win32 event handle kept alive by `Arc<OwnedHandle>`.
            unsafe {
                let _ = SetEvent(self.raw.0);
            }
        }

        /// Returns `true` if shutdown has already been signaled.
        pub fn is_signaled(&self) -> bool {
            // SAFETY: `self.raw.0` is a valid manual-reset Win32 event handle.
            unsafe { WaitForSingleObject(self.raw.0, 0) == WAIT_OBJECT_0 }
        }

        /// Returns the underlying raw Win32 event handle.
        pub fn handle(&self) -> HANDLE {
            self.raw.0
        }
    }

    struct ConnectedPipe {
        handle: HANDLE,
        shutdown: ShutdownEvent,
    }

    // SAFETY: `ConnectedPipe` uniquely owns the connected named-pipe handle until dropped.
    unsafe impl Send for ConnectedPipe {}

    impl Drop for ConnectedPipe {
        fn drop(&mut self) {
            if !self.handle.is_invalid() && self.handle != HANDLE::default() {
                // SAFETY: `self.handle` is a uniquely owned server pipe instance closed once on drop
                // without calling `DisconnectNamedPipe` so buffered response bytes stay readable.
                unsafe {
                    let _ = CloseHandle(self.handle);
                }
            }
        }
    }

    fn is_disconnect_hresult(code: HRESULT) -> bool {
        code == HRESULT::from_win32(ERROR_BROKEN_PIPE.0)
            || code == HRESULT::from_win32(ERROR_PIPE_NOT_CONNECTED.0)
            || code == HRESULT::from_win32(ERROR_NO_DATA.0)
            || code == HRESULT::from_win32(ERROR_OPERATION_ABORTED.0)
    }

    fn map_io_win32_error(err: &windows::core::Error) -> std::io::Error {
        let code = err.code();
        if is_disconnect_hresult(code) {
            return std::io::Error::new(ErrorKind::BrokenPipe, err.to_string());
        }
        std::io::Error::from_raw_os_error(code.0)
    }

    impl ConnectedPipe {
        fn wait_overlapped(&self, overlapped: &mut OVERLAPPED) -> std::io::Result<usize> {
            let handles = [overlapped.hEvent, self.shutdown.handle()];
            // SAFETY: Both event handles are valid Win32 event objects for the duration of this wait.
            let wait = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
            if wait == WAIT_OBJECT_0 {
                let mut transferred: u32 = 0;
                // SAFETY: `self.handle` and `overlapped` remain valid until `GetOverlappedResult` completes.
                unsafe { GetOverlappedResult(self.handle, overlapped, &mut transferred, false) }
                    .map_err(|err| map_io_win32_error(&err))?;
                return Ok(transferred as usize);
            }

            // SAFETY: Shutdown fired while overlapped I/O was pending; cancel and drain safely.
            unsafe {
                let _ = CancelIoEx(self.handle, Some(overlapped));
                let mut ignored: u32 = 0;
                let _ = GetOverlappedResult(self.handle, overlapped, &mut ignored, true);
            }
            Err(std::io::Error::new(
                ErrorKind::Interrupted,
                "service shutdown interrupted named pipe I/O",
            ))
        }
    }

    impl Read for ConnectedPipe {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if buf.is_empty() {
                return Ok(0);
            }
            // SAFETY: Creates a local manual-reset event closed when `io_event` drops.
            let io_event = OwnedHandle(
                unsafe { CreateEventW(None, true, false, None) }
                    .map_err(|err| std::io::Error::from_raw_os_error(err.code().0))?,
            );
            let mut overlapped = OVERLAPPED {
                hEvent: io_event.0,
                ..Default::default()
            };
            let mut bytes_read: u32 = 0;
            // SAFETY: `self.handle` is an overlapped pipe handle and `buf` lives until overlapped completion.
            let res = unsafe {
                ReadFile(
                    self.handle,
                    Some(buf),
                    Some(&mut bytes_read),
                    Some(&mut overlapped),
                )
            };
            match res {
                Ok(()) => {
                    if bytes_read == 0 {
                        return Err(std::io::Error::new(
                            ErrorKind::BrokenPipe,
                            "named pipe client closed stream",
                        ));
                    }
                    Ok(bytes_read as usize)
                }
                Err(err) if err.code() == HRESULT::from_win32(ERROR_IO_PENDING.0) => {
                    let count = self.wait_overlapped(&mut overlapped)?;
                    if count == 0 {
                        return Err(std::io::Error::new(
                            ErrorKind::BrokenPipe,
                            "named pipe client closed stream",
                        ));
                    }
                    Ok(count)
                }
                Err(err) => Err(map_io_win32_error(&err)),
            }
        }
    }

    impl Write for ConnectedPipe {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if buf.is_empty() {
                return Ok(0);
            }
            // SAFETY: Creates a local manual-reset event closed when `io_event` drops.
            let io_event = OwnedHandle(
                unsafe { CreateEventW(None, true, false, None) }
                    .map_err(|err| std::io::Error::from_raw_os_error(err.code().0))?,
            );
            let mut overlapped = OVERLAPPED {
                hEvent: io_event.0,
                ..Default::default()
            };
            let mut written: u32 = 0;
            // SAFETY: `self.handle` is an overlapped pipe handle and `buf` lives until overlapped completion.
            let res = unsafe {
                WriteFile(
                    self.handle,
                    Some(buf),
                    Some(&mut written),
                    Some(&mut overlapped),
                )
            };
            match res {
                Ok(()) => Ok(written as usize),
                Err(err) if err.code() == HRESULT::from_win32(ERROR_IO_PENDING.0) => {
                    self.wait_overlapped(&mut overlapped)
                }
                Err(err) => Err(map_io_win32_error(&err)),
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            // WHY: Overlapped `WriteFile` already commits the frame to the kernel pipe buffer;
            // calling `FlushFileBuffers` would block service shutdown on a stalled client reader.
            Ok(())
        }
    }

    fn create_acl_pipe_instance(first_instance: bool) -> std::io::Result<OwnedHandle> {
        let wide_sddl: Vec<u16> = IPC_PIPE_SDDL
            .encode_utf16()
            .chain(core::iter::once(0))
            .collect();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `wide_sddl` is NUL-terminated UTF-16 and `sd` receives a `LocalAlloc` descriptor freed by guard.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide_sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
        }
        .map_err(|err| std::io::Error::from_raw_os_error(err.code().0))?;
        let sd_guard = SecurityDescriptorGuard(sd);

        let sa = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd_guard.0 .0,
            bInheritHandle: false.into(),
        };

        let wide_pipe: Vec<u16> = IPC_PIPE_NAME
            .encode_utf16()
            .chain(core::iter::once(0))
            .collect();

        let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
        if first_instance {
            open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }

        // SAFETY: `wide_pipe` is NUL-terminated UTF-16 and `sa` points to a valid security descriptor.
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide_pipe.as_ptr()),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                PIPE_BUFFER_SIZE,
                PIPE_BUFFER_SIZE,
                0,
                Some(&sa),
            )
        };
        drop(sd_guard);

        if handle == INVALID_HANDLE_VALUE || handle.is_invalid() {
            return Err(std::io::Error::last_os_error());
        }
        Ok(OwnedHandle(handle))
    }

    struct ActiveWorkerGuard {
        tracker: Arc<(Mutex<usize>, Condvar)>,
    }

    impl Drop for ActiveWorkerGuard {
        fn drop(&mut self) {
            let (lock, cv) = &*self.tracker;
            let mut count = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *count = count.saturating_sub(1);
            cv.notify_all();
        }
    }

    /// Runs the blocking ACL'd named-pipe IPC accept loop until `shutdown` is signaled.
    pub fn serve_named_pipe(
        handler: Arc<dyn IpcHandler>,
        shutdown: ShutdownEvent,
    ) -> std::io::Result<()> {
        // SAFETY: Creates a reusable manual-reset event for overlapped `ConnectNamedPipe`.
        let connect_event = OwnedHandle(
            unsafe { CreateEventW(None, true, false, None) }
                .map_err(|err| std::io::Error::from_raw_os_error(err.code().0))?,
        );
        let worker_tracker = Arc::new((Mutex::new(0usize), Condvar::new()));
        let mut first_instance = true;

        while !shutdown.is_signaled() {
            let pipe_handle = create_acl_pipe_instance(first_instance)?;
            first_instance = false;
            // SAFETY: Reset manual-reset event before issuing the next overlapped `ConnectNamedPipe`.
            let _ = unsafe { ResetEvent(connect_event.0) };
            let mut overlapped = OVERLAPPED {
                hEvent: connect_event.0,
                ..Default::default()
            };

            // SAFETY: `pipe_handle.0` is an overlapped pipe instance and `overlapped.hEvent` is valid.
            let connect_res = unsafe { ConnectNamedPipe(pipe_handle.0, Some(&mut overlapped)) };
            let connected = match connect_res {
                Ok(()) => true,
                Err(err) if err.code() == HRESULT::from_win32(ERROR_PIPE_CONNECTED.0) => true,
                Err(err) if err.code() == HRESULT::from_win32(ERROR_NO_DATA.0) => false,
                Err(err) if err.code() == HRESULT::from_win32(ERROR_IO_PENDING.0) => {
                    let handles = [connect_event.0, shutdown.handle()];
                    // SAFETY: Both event handles remain valid until `WaitForMultipleObjects` returns.
                    let wait = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
                    if wait == WAIT_OBJECT_0 {
                        let mut ignored: u32 = 0;
                        // SAFETY: `pipe_handle.0` and `overlapped` are still valid for completion query.
                        match unsafe {
                            GetOverlappedResult(pipe_handle.0, &overlapped, &mut ignored, false)
                        } {
                            Ok(()) => true,
                            Err(res_err) if is_disconnect_hresult(res_err.code()) => false,
                            Err(res_err) => {
                                return Err(std::io::Error::from_raw_os_error(res_err.code().0));
                            }
                        }
                    } else {
                        // SAFETY: Shutdown fired; cancel pending `ConnectNamedPipe` and exit cleanly.
                        unsafe {
                            let _ = CancelIoEx(pipe_handle.0, Some(&overlapped));
                            let mut ignored: u32 = 0;
                            let _ =
                                GetOverlappedResult(pipe_handle.0, &overlapped, &mut ignored, true);
                        }
                        break;
                    }
                }
                Err(err) => {
                    return Err(std::io::Error::from_raw_os_error(err.code().0));
                }
            };

            if !connected {
                // SAFETY: Client connected and disconnected with zero bytes before service read.
                unsafe {
                    let _ = DisconnectNamedPipe(pipe_handle.0);
                }
                continue;
            }

            let raw_pipe = pipe_handle.0;
            core::mem::forget(pipe_handle);
            let mut client = ConnectedPipe {
                handle: raw_pipe,
                shutdown: shutdown.clone(),
            };
            let handler_clone = Arc::clone(&handler);
            {
                let (lock, _) = &*worker_tracker;
                let mut count = lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *count += 1;
            }
            let tracker_clone = Arc::clone(&worker_tracker);
            if thread::Builder::new()
                .name("zerovdd-ipc-worker".to_string())
                .spawn(move || {
                    let _guard = ActiveWorkerGuard {
                        tracker: tracker_clone,
                    };
                    match handle_pipe_stream(&mut client, handler_clone.as_ref()) {
                        Ok(()) => {}
                        Err(IpcError::Io(io_err))
                            if matches!(
                                io_err.kind(),
                                ErrorKind::BrokenPipe
                                    | ErrorKind::UnexpectedEof
                                    | ErrorKind::Interrupted
                            ) => {}
                        Err(err) => {
                            eprintln!("zerovdd-svc pipe request error: {err}");
                        }
                    }
                })
                .is_err()
            {
                let (lock, cv) = &*worker_tracker;
                let mut count = lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *count = count.saturating_sub(1);
                cv.notify_all();
            }
        }

        let (lock, cv) = &*worker_tracker;
        let deadline = Instant::now() + PIPE_SHUTDOWN_DRAIN_TIMEOUT;
        let mut count = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *count > 0 {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            let (next_guard, wait_res) = cv
                .wait_timeout(count, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            count = next_guard;
            if wait_res.timed_out() {
                break;
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
pub use win32::{serve_named_pipe, ShutdownEvent};
