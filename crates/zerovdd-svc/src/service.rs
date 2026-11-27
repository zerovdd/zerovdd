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
    use core::ffi::c_void;
    use core::mem::size_of;
    use std::ffi::OsString;
    use std::sync::mpsc;
    use std::sync::{Arc, OnceLock};
    use std::thread;
    use std::time::{Duration, Instant};

    use windows::core::{BOOL, GUID};
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Register_Notification, CM_Unregister_Notification, CM_NOTIFY_ACTION,
        CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL, CM_NOTIFY_ACTION_DEVICEINTERFACEREMOVAL,
        CM_NOTIFY_EVENT_DATA, CM_NOTIFY_FILTER, CM_NOTIFY_FILTER_0, CM_NOTIFY_FILTER_0_0,
        CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE, CONFIGRET, CR_SUCCESS, HCMNOTIFICATION,
    };
    use windows::Win32::System::Console::SetConsoleCtrlHandler;
    use windows_service::service::{
        PowerEventParam, ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState,
        ServiceStatus, ServiceType, SessionChangeReason,
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

    const GUID_DEVINTERFACE_MONITOR: GUID = GUID::from_u128(0xe6f07b5f_ee97_4a90_b076_33f57bf4eaa7);
    const TOPOLOGY_ATTACH_DEBOUNCE: Duration = Duration::from_secs(4);
    const TOPOLOGY_DEPART_DEBOUNCE: Duration = Duration::from_millis(500);

    enum ControlMessage {
        SessionChange(ConsoleSessionEvent),
        DisplayTopologyChanged,
        CancelPendingTopologyCheck,
        Stop,
    }

    struct MonitorPnpWatcher {
        handle: HCMNOTIFICATION,
        context_ptr: *mut mpsc::Sender<ControlMessage>,
    }

    impl MonitorPnpWatcher {
        fn register(tx: mpsc::Sender<ControlMessage>) -> Option<Self> {
            let context_ptr = Box::into_raw(Box::new(tx));
            let filter = CM_NOTIFY_FILTER {
                cbSize: size_of::<CM_NOTIFY_FILTER>() as u32,
                Flags: 0,
                FilterType: CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE,
                Reserved: 0,
                u: CM_NOTIFY_FILTER_0 {
                    DeviceInterface: CM_NOTIFY_FILTER_0_0 {
                        ClassGuid: GUID_DEVINTERFACE_MONITOR,
                    },
                },
            };
            let mut handle = HCMNOTIFICATION::default();
            // SAFETY: `filter` has valid `cbSize`, `context_ptr` remains alive until after
            // `CM_Unregister_Notification` finishes in `Drop`, and `monitor_pnp_callback` matches
            // the `PCM_NOTIFY_CALLBACK` ABI.
            let cr: CONFIGRET = unsafe {
                CM_Register_Notification(
                    &filter,
                    Some(context_ptr.cast::<c_void>()),
                    Some(monitor_pnp_callback),
                    &mut handle,
                )
            };
            if cr != CR_SUCCESS {
                // SAFETY: Registration failed so Windows will never invoke the callback; reclaim box.
                unsafe {
                    drop(Box::from_raw(context_ptr));
                }
                eprintln!(
                    "zerovdd-svc monitor PnP notification registration warning: CONFIGRET({:#x})",
                    cr.0
                );
                return None;
            }
            Some(Self {
                handle,
                context_ptr,
            })
        }
    }

    impl Drop for MonitorPnpWatcher {
        fn drop(&mut self) {
            // SAFETY: `self.handle` was created by `CM_Register_Notification`; unregistering
            // synchronously waits for any in-flight callback to exit before freeing `context_ptr`.
            unsafe {
                let _ = CM_Unregister_Notification(self.handle);
                drop(Box::from_raw(self.context_ptr));
            }
        }
    }

    // SAFETY: Matches Win32 `PCM_NOTIFY_CALLBACK` ABI; `context` points to a live
    // `mpsc::Sender<ControlMessage>` kept alive until `CM_Unregister_Notification` returns.
    unsafe extern "system" fn monitor_pnp_callback(
        _notify: HCMNOTIFICATION,
        context: *const c_void,
        action: CM_NOTIFY_ACTION,
        _event_data: *const CM_NOTIFY_EVENT_DATA,
        _event_data_size: u32,
    ) -> u32 {
        if (action == CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL
            || action == CM_NOTIFY_ACTION_DEVICEINTERFACEREMOVAL)
            && !context.is_null()
        {
            // SAFETY: `context` was allocated by `Box::into_raw` in `MonitorPnpWatcher::register`
            // and remains valid for the entire callback invocation.
            let tx = unsafe { &*context.cast::<mpsc::Sender<ControlMessage>>() };
            let _ = tx.send(ControlMessage::DisplayTopologyChanged);
        }
        0
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
            SessionChangeReason::SessionLock => ConsoleSessionEvent::SessionLock,
            _ => ConsoleSessionEvent::Other,
        }
    }

    const fn map_power_event(param: &PowerEventParam) -> Option<ControlMessage> {
        // WHY: Cancel any armed standby attach timer when Windows enters Suspend/QuerySuspend so
        // pre-sleep monitor removals never attach a virtual display mid-suspend.
        match param {
            PowerEventParam::Suspend | PowerEventParam::QuerySuspend => {
                Some(ControlMessage::CancelPendingTopologyCheck)
            }
            PowerEventParam::ResumeAutomatic
            | PowerEventParam::ResumeSuspend
            | PowerEventParam::ResumeCritical
            | PowerEventParam::PowerSettingChange(_) => {
                Some(ControlMessage::DisplayTopologyChanged)
            }
            _ => None,
        }
    }

    fn run_control_loop(
        coordinator: &ServiceCoordinator,
        ctrl_rx: &mpsc::Receiver<ControlMessage>,
    ) {
        let mut pending_deadline: Option<Instant> = None;
        loop {
            let recv = if let Some(deadline) = pending_deadline {
                let remaining = deadline.saturating_duration_since(Instant::now());
                ctrl_rx.recv_timeout(remaining)
            } else {
                ctrl_rx
                    .recv()
                    .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
            };
            match recv {
                Ok(ControlMessage::SessionChange(event)) => {
                    if let Err(err) = coordinator.on_session_change(event) {
                        eprintln!("zerovdd-svc session change recovery warning: {err:#}");
                    }
                }
                Ok(ControlMessage::DisplayTopologyChanged) => {
                    // WHY: Depart standby rapidly (500 ms) once a physical monitor wakes up, but
                    // wait 4 seconds before attaching standby so DisplayPort link re-training,
                    // HDR toggles, or fullscreen mode switches never trigger false flicker.
                    let window = if coordinator.is_headless_standby_active() {
                        TOPOLOGY_DEPART_DEBOUNCE
                    } else {
                        TOPOLOGY_ATTACH_DEBOUNCE
                    };
                    pending_deadline = Some(Instant::now() + window);
                }
                Ok(ControlMessage::CancelPendingTopologyCheck) => {
                    pending_deadline = None;
                }
                Ok(ControlMessage::Stop) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    pending_deadline = None;
                    coordinator.on_display_topology_changed();
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
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
        let scm_ctrl_tx = ctrl_tx.clone();

        let status_handle = service_control_handler::register(
            SERVICE_NAME,
            move |control_event| match control_event {
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    shutdown_handler.signal();
                    let _ = scm_ctrl_tx.send(ControlMessage::Stop);
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                ServiceControl::SessionChange(param) => {
                    let mapped = map_session_change_reason(param.reason);
                    let _ = scm_ctrl_tx.send(ControlMessage::SessionChange(mapped));
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::PowerEvent(param) => {
                    if let Some(msg) = map_power_event(&param) {
                        let _ = scm_ctrl_tx.send(msg);
                    }
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

        let pnp_watcher = MonitorPnpWatcher::register(ctrl_tx);

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
                | ServiceControlAccept::SESSION_CHANGE
                | ServiceControlAccept::POWER_EVENT,
            0,
            Duration::ZERO,
            ServiceExitCode::Win32(0),
        );

        run_control_loop(&coordinator, &ctrl_rx);

        set_status(
            ServiceState::StopPending,
            ServiceControlAccept::empty(),
            1,
            Duration::from_secs(30),
            ServiceExitCode::Win32(0),
        );

        drop(pnp_watcher);
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

        let (ctrl_tx, ctrl_rx) = mpsc::channel::<ControlMessage>();
        let pnp_watcher = MonitorPnpWatcher::register(ctrl_tx.clone());
        let loop_coordinator = Arc::clone(&coordinator);
        let control_thread = thread::Builder::new()
            .name("zerovdd-pnp-reconcile".to_string())
            .spawn(move || run_control_loop(&loop_coordinator, &ctrl_rx))?;

        eprintln!("zerovdd-svc running in console mode (press Ctrl+C to stop)");
        let pipe_handler: Arc<dyn IpcHandler> = Arc::clone(&coordinator) as Arc<dyn IpcHandler>;
        let pipe_res = serve_named_pipe(pipe_handler, shutdown);
        drop(pnp_watcher);
        let _ = ctrl_tx.send(ControlMessage::Stop);
        let _ = control_thread.join();
        coordinator.shutdown();
        pipe_res?;
        Ok(())
    }
}
