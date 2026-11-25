//! Windows SCM service lifecycle (`zerovdd-svc`) and `--console` foreground mode.

/// Canonical Windows Service Control Manager name for `zerovdd-svc`.
pub const SERVICE_NAME: &str = "zerovdd-svc";

/// Runs `zerovdd-svc` under the Windows Service Control Manager dispatcher.
#[cfg(windows)]
pub fn run_service_dispatcher() -> anyhow::Result<()> {
    win32::dispatch()
}

/// Runs `zerovdd-svc` under the Windows Service Control Manager dispatcher.
#[cfg(not(windows))]
pub fn run_service_dispatcher() -> anyhow::Result<()> {
    anyhow::bail!("zerovdd-svc Windows SCM mode is only supported on Windows")
}

/// Runs `zerovdd-svc` in foreground interactive console mode (for local testing without SCM).
#[cfg(windows)]
pub fn run_console_mode() -> anyhow::Result<()> {
    win32::run_console()
}

/// Runs `zerovdd-svc` in foreground interactive console mode (for local testing without SCM).
#[cfg(not(windows))]
pub fn run_console_mode() -> anyhow::Result<()> {
    anyhow::bail!("zerovdd-svc foreground console mode is only supported on Windows")
}

#[cfg(windows)]
mod win32 {
    use std::ffi::OsString;
    use std::sync::mpsc;
    use std::sync::{Arc, OnceLock};
    use std::thread;
    use std::time::Duration;

    use windows::core::BOOL;
    use windows::Win32::System::Console::SetConsoleCtrlHandler;
    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType, SessionChangeReason,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::{define_windows_service, service_dispatcher};

    use super::SERVICE_NAME;
    use crate::console_launcher::WindowsConsoleLauncher;
    use crate::persistent::load_service_config;
    use crate::pipe::{serve_named_pipe, IpcHandler, ShutdownEvent};
    use crate::session::{
        ConsoleSessionEvent, ServiceCoordinator, WindowsDriverProvider, WindowsProcessWatcher,
    };

    enum ControlMessage {
        SessionChange(ConsoleSessionEvent),
        Stop,
    }

    static CONSOLE_SHUTDOWN: OnceLock<ShutdownEvent> = OnceLock::new();

    // SAFETY: Matches Win32 `PHANDLER_ROUTINE` ABI; signals the process-wide shutdown event only.
    unsafe extern "system" fn console_ctrl_handler(_ctrl_type: u32) -> BOOL {
        if let Some(ev) = CONSOLE_SHUTDOWN.get() {
            ev.signal();
            return BOOL(1);
        }
        BOOL(0)
    }

    const fn map_session_change_reason(reason: SessionChangeReason) -> ConsoleSessionEvent {
        match reason {
            SessionChangeReason::ConsoleConnect => ConsoleSessionEvent::ConsoleConnect,
            SessionChangeReason::SessionLogon => ConsoleSessionEvent::SessionLogon,
            SessionChangeReason::SessionUnlock => ConsoleSessionEvent::SessionUnlock,
            _ => ConsoleSessionEvent::Other,
        }
    }

    fn build_coordinator() -> anyhow::Result<Arc<ServiceCoordinator>> {
        let config = load_service_config(None)?;
        let driver = Arc::new(WindowsDriverProvider::new());
        let launcher = Arc::new(WindowsConsoleLauncher::new());
        let watcher = Arc::new(WindowsProcessWatcher);
        Ok(ServiceCoordinator::new(
            config, None, driver, launcher, watcher,
        ))
    }

    define_windows_service!(ffi_service_main, service_main);

    fn service_main(_arguments: Vec<OsString>) {
        if let Err(err) = run_scm_service() {
            eprintln!("zerovdd-svc failed: {err:#}");
        }
    }

    fn run_scm_service() -> anyhow::Result<()> {
        let shutdown = ShutdownEvent::new()?;
        let shutdown_handler = shutdown.clone();
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<ControlMessage>();

        let status_handle = service_control_handler::register(
            SERVICE_NAME,
            move |control_event| match control_event {
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    shutdown_handler.signal();
                    let _ = ctrl_tx.send(ControlMessage::Stop);
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                ServiceControl::SessionChange(param) => {
                    let mapped = map_session_change_reason(param.reason);
                    let _ = ctrl_tx.send(ControlMessage::SessionChange(mapped));
                    ServiceControlHandlerResult::NoError
                }
                _ => ServiceControlHandlerResult::NotImplemented,
            },
        )?;

        let set_status = |current_state: ServiceState,
                          controls_accepted: ServiceControlAccept,
                          checkpoint: u32,
                          wait_hint: Duration,
                          exit_code: ServiceExitCode| {
            let _ = status_handle.set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state,
                controls_accepted,
                exit_code,
                checkpoint,
                wait_hint,
                process_id: None,
            });
        };

        set_status(
            ServiceState::StartPending,
            ServiceControlAccept::empty(),
            1,
            Duration::from_secs(30),
            ServiceExitCode::Win32(0),
        );

        let coordinator = match build_coordinator() {
            Ok(coord) => coord,
            Err(err) => {
                set_status(
                    ServiceState::Stopped,
                    ServiceControlAccept::empty(),
                    0,
                    Duration::ZERO,
                    ServiceExitCode::Win32(1),
                );
                return Err(err);
            }
        };

        set_status(
            ServiceState::StartPending,
            ServiceControlAccept::empty(),
            2,
            Duration::from_secs(30),
            ServiceExitCode::Win32(0),
        );

        if let Err(err) = coordinator.initialize_on_startup() {
            eprintln!("zerovdd-svc startup recovery warning: {err:#}");
        }

        set_status(
            ServiceState::StartPending,
            ServiceControlAccept::empty(),
            3,
            Duration::from_secs(30),
            ServiceExitCode::Win32(0),
        );

        let pipe_coordinator = Arc::clone(&coordinator);
        let pipe_shutdown = shutdown.clone();
        let pipe_thread = thread::Builder::new()
            .name("zerovdd-ipc-pipe".to_string())
            .spawn(move || {
                if let Err(err) = serve_named_pipe(pipe_coordinator, pipe_shutdown) {
                    eprintln!("zerovdd-svc named pipe server exited with error: {err:#}");
                }
            })?;

        set_status(
            ServiceState::Running,
            ServiceControlAccept::STOP
                | ServiceControlAccept::SHUTDOWN
                | ServiceControlAccept::SESSION_CHANGE,
            0,
            Duration::ZERO,
            ServiceExitCode::Win32(0),
        );

        while let Ok(msg) = ctrl_rx.recv() {
            match msg {
                ControlMessage::SessionChange(event) => {
                    if let Err(err) = coordinator.on_session_change(event) {
                        eprintln!("zerovdd-svc session change recovery warning: {err:#}");
                    }
                }
                ControlMessage::Stop => break,
            }
        }

        set_status(
            ServiceState::StopPending,
            ServiceControlAccept::empty(),
            1,
            Duration::from_secs(30),
            ServiceExitCode::Win32(0),
        );

        shutdown.signal();
        let _ = pipe_thread.join();
        coordinator.shutdown();

        set_status(
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
            0,
            Duration::ZERO,
            ServiceExitCode::Win32(0),
        );

        Ok(())
    }

    pub(super) fn dispatch() -> anyhow::Result<()> {
        service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
        Ok(())
    }

    pub(super) fn run_console() -> anyhow::Result<()> {
        let shutdown = ShutdownEvent::new()?;
        let _ = CONSOLE_SHUTDOWN.set(shutdown.clone());
        // SAFETY: `console_ctrl_handler` matches the Win32 `PHANDLER_ROUTINE` signature.
        unsafe {
            SetConsoleCtrlHandler(Some(console_ctrl_handler), true)?;
        }

        let coordinator = build_coordinator()?;
        if let Err(err) = coordinator.initialize_on_startup() {
            eprintln!("zerovdd-svc startup recovery warning: {err:#}");
        }

        eprintln!("zerovdd-svc running in console mode (press Ctrl+C to stop)");
        let pipe_handler: Arc<dyn IpcHandler> = Arc::clone(&coordinator) as Arc<dyn IpcHandler>;
        let pipe_res = serve_named_pipe(pipe_handler, shutdown);
        coordinator.shutdown();
        pipe_res?;
        Ok(())
    }
}
