//! System diagnostics and ETW trace capture subcommand (`zerovdd doctor`).

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use serde::Serialize;
use zerovdd_client::gpu::GpuAdapterInfo;
use zerovdd_client::hdr::OsVersionInfo;
#[cfg(any(windows, test))]
use zerovdd_client::hdr::WIN11_24H2_BUILD;
use zerovdd_installer::InstallerStatus;
use zerovdd_proto::ipc::{IpcRequest, IpcResponse, ServiceHealth};

/// ETW TraceLogging provider name emitted by `zerovdd.dll`.
pub const ETW_PROVIDER_NAME: &str = "ZeroVdd.Driver";

/// ETW TraceLogging provider GUID for `ZeroVdd.Driver`.
pub const ETW_PROVIDER_GUID: &str = "{06714745-69DC-41FF-9684-C532D5183A74}";

/// Arguments for `zerovdd doctor`.
#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Emit full diagnostic report as pretty-printed JSON.
    #[arg(long)]
    pub json: bool,
    /// Capture an ETW trace (`.etl`) for `ZeroVdd.Driver` (`{06714745-69DC-41FF-9684-C532D5183A74}`) for `<SECS>` seconds.
    #[arg(long, value_name = "SECS")]
    pub capture_etw: Option<u64>,
}

/// Control Device accessibility and runtime telemetry state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[cfg_attr(
    not(windows),
    allow(
        dead_code,
        reason = "Windows Control Device variants are constructed on Windows targets and in tests"
    )
)]
pub enum ControlDeviceStatus {
    /// Control Device opened and responded to `IOCTL_GET_VERSION` / `IOCTL_GET_STATUS`.
    Accessible {
        /// Win32 device path used to open the Control Device.
        device_path: String,
        /// Driver IOCTL protocol version `(major, minor, patch)`.
        protocol_version: (u16, u16, u16),
        /// Whether the IddCx adapter is initialized and ready.
        adapter_ready: bool,
        /// Whether FP16 / HDR10 pipeline capability is enabled.
        hdr_capable: bool,
        /// Adapter uptime in seconds.
        uptime_secs: u64,
        /// Whether the most recent GPU render adapter switch failed.
        last_gpu_switch_failed: bool,
    },
    /// Opening `\\.\ZeroVddControl` failed with `ERROR_ACCESS_DENIED` (elevation required).
    AccessDenied,
    /// Control Device interface and DOS symlink were not found (`NotInstalled` / driver stopped).
    NotInstalled,
    /// Opening or querying the Control Device failed with an unexpected error.
    Error {
        /// Diagnostic error detail.
        message: String,
    },
    /// Diagnostic executed on a non-Windows host.
    #[cfg(not(windows))]
    UnsupportedPlatform,
}

/// Windows `WUDFHost.exe` process memory and handle metrics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WudfHostProcessInfo {
    /// Windows Process ID (`PID`).
    pub pid: u32,
    /// Private committed memory bytes (`PrivateUsage`), when available.
    pub private_memory_bytes: u64,
    /// Working set memory bytes (`WorkingSetSize`).
    pub working_set_bytes: u64,
    /// Open kernel handle count (`GetProcessHandleCount`).
    pub handle_count: u32,
    /// `true` if `zerovdd.dll` module was confirmed loaded in this `WUDFHost.exe` instance.
    pub hosts_zerovdd_dll: bool,
}

/// OS build and Windows 11 24H2 HDR/ACM API capability summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OsDiagnosticInfo {
    /// True Windows major/minor/build tuple from `ntdll!RtlGetVersion`.
    pub version: Option<OsVersionInfo>,
    /// Human-readable OS release classification (`Win11 24H2+ (split HDR/ACM)` vs `Win11 22H2/23H2`).
    pub hdr_api_profile: String,
    /// `true` when `build >= 26100` (Win11 24H2 split `SET_HDR_STATE`/`SET_WCG_STATE`).
    pub split_hdr_acm_supported: bool,
}

/// Summary of an active virtual display reported by `IOCTL_LIST_DISPLAYS`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorDisplaySummary {
    /// Monitor container UUID string.
    pub monitor_guid: String,
    /// Client/display label.
    pub client_name: String,
    /// Adapter LUID low part.
    pub adapter_luid_low: u32,
    /// Adapter LUID high part.
    pub adapter_luid_high: i32,
    /// VidPN target ID.
    pub target_id: u32,
    /// Active width in pixels.
    pub width: u32,
    /// Active height in pixels.
    pub height: u32,
    /// Rational refresh numerator.
    pub refresh_num: u32,
    /// Rational refresh denominator.
    pub refresh_den: u32,
    /// Remaining lease seconds (`0` for handle-bound displays).
    pub lease_remaining_secs: u32,
}

/// Windows service (`zerovdd-svc`) diagnostic state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServiceDiagnosticInfo {
    /// Service Control Manager state (`running`, `stopped`, `not_installed`, etc.).
    pub scm_state: String,
    /// Live health telemetry returned over `\\.\pipe\zerovdd-svc`, if reachable.
    pub ipc_health: Option<ServiceHealth>,
}

/// Result of an optional ETW `.etl` trace capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EtwCaptureReport {
    /// ETW provider name (`ZeroVdd.Driver`).
    pub provider_name: String,
    /// ETW provider GUID (`{06714745-69DC-41FF-9684-C532D5183A74}`).
    pub provider_guid: String,
    /// Output `.etl` file path.
    pub etl_path: PathBuf,
    /// Requested capture duration in seconds.
    pub duration_secs: u64,
    /// `true` if `logman` started and stopped the trace cleanly.
    pub captured: bool,
    /// Diagnostic message or tool error output.
    pub detail: String,
}

/// Complete diagnostic snapshot produced by `zerovdd doctor`.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    /// CLI package version.
    pub cli_version: &'static str,
    /// Host target architecture.
    pub arch: &'static str,
    /// Driver Store INF isolation, PnP device nodes, and tracked dev certificates.
    pub installer: Option<InstallerStatus>,
    /// Error message if querying driver installation state failed.
    pub installer_error: Option<String>,
    /// Detected `WUDFHost.exe` processes.
    pub wudf_host_processes: Vec<WudfHostProcessInfo>,
    /// OS build and HDR/ACM API profile.
    pub os: OsDiagnosticInfo,
    /// Enumerated DXGI GPUs and LUIDs.
    pub gpus: Vec<GpuAdapterInfo>,
    /// Error message if DXGI GPU enumeration failed.
    pub gpu_error: Option<String>,
    /// Control Device access and IOCTL health telemetry.
    pub control_device: ControlDeviceStatus,
    /// Active virtual displays returned by the driver.
    pub active_displays: Vec<DoctorDisplaySummary>,
    /// Windows service status and IPC health.
    pub service: ServiceDiagnosticInfo,
    /// Optional ETW capture summary when `--capture-etw <secs>` was requested.
    pub etw_capture: Option<EtwCaptureReport>,
}

/// Classifies an OS build number into the corresponding Windows HDR/ACM API profile.
#[cfg(any(windows, test))]
pub fn classify_hdr_api_profile(build: u32) -> (&'static str, bool) {
    if build >= WIN11_24H2_BUILD {
        (
            "Win11 24H2+ (split HDR/ACM via SET_HDR_STATE/SET_WCG_STATE)",
            true,
        )
    } else if build >= 22_621 {
        ("Win11 22H2/23H2 (legacy SET_ADVANCED_COLOR_STATE)", false)
    } else {
        ("Pre-22H2 Windows (legacy SET_ADVANCED_COLOR_STATE)", false)
    }
}

/// Executes `zerovdd doctor` and writes either JSON or human-readable output.
pub fn execute(args: DoctorArgs) -> Result<()> {
    let report = collect_report(args.capture_etw)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human_report(&report);
    }
    Ok(())
}

fn print_human_report(report: &DoctorReport) {
    println!("zerovdd doctor (v{}, {})", report.cli_version, report.arch);
    println!("OS HDR/ACM profile: {}", report.os.hdr_api_profile);
    if let Some(v) = report.os.version {
        println!("  OS version: {}.{}.{}", v.major, v.minor, v.build);
    }

    println!("Driver Installation:");
    if let Some(ref st) = report.installer {
        println!("  Installed: {}", st.driver.installed);
        if let Some(ref oem) = st.driver.oem_inf_name {
            println!("  OEM INF: {oem}");
        }
        if let Some(ref p) = st.driver.driver_store_path {
            println!("  Driver Store INF: {}", p.display());
        }
        println!("  DIRID 13 isolated: {}", st.driver.dirid_13_isolated);
        if let Some(ref ver) = st.driver.driver_version {
            println!("  Version: {ver}");
        }
        if let Some(ref signer) = st.driver.signer_name {
            println!("  Signer: {signer}");
        }
        println!("  Device nodes: {}", st.device_nodes.len());
        for node in &st.device_nodes {
            println!(
                "    - {} (present={}, problem={})",
                node.instance_id, node.present, node.problem_code
            );
        }
    } else if let Some(ref err) = report.installer_error {
        println!("  Query error: {err}");
    }

    println!(
        "WUDFHost.exe processes: {}",
        report.wudf_host_processes.len()
    );
    for proc_info in &report.wudf_host_processes {
        println!(
            "  - PID {} | private={} MiB | ws={} MiB | handles={} | zerovdd.dll={}",
            proc_info.pid,
            proc_info.private_memory_bytes / (1024 * 1024),
            proc_info.working_set_bytes / (1024 * 1024),
            proc_info.handle_count,
            proc_info.hosts_zerovdd_dll
        );
    }

    println!("GPUs ({}):", report.gpus.len());
    if let Some(ref err) = report.gpu_error {
        println!("  GPU enumeration error: {err}");
    }
    for gpu in &report.gpus {
        println!(
            "  - {} | LUID={:#x}:{:#x} | VRAM={} MiB | software={}",
            gpu.name,
            gpu.adapter_luid_low,
            gpu.adapter_luid_high,
            gpu.dedicated_vram_bytes / (1024 * 1024),
            gpu.is_software
        );
    }

    println!("Control Device: {:?}", report.control_device);
    println!("Active virtual displays: {}", report.active_displays.len());
    for d in &report.active_displays {
        println!(
            "  - {} ({}) target={} {}x{}@{}/{} Hz lease={}s",
            d.monitor_guid,
            d.client_name,
            d.target_id,
            d.width,
            d.height,
            d.refresh_num,
            d.refresh_den,
            d.lease_remaining_secs
        );
    }

    println!(
        "Service ({}): state={}",
        zerovdd_installer::device_node::SERVICE_NAME,
        report.service.scm_state
    );
    if let Some(ref health) = report.service.ipc_health {
        println!(
            "  IPC health: v{} connected={} displays={} uptime={}s",
            health.service_version,
            health.driver_connected,
            health.active_displays,
            health.uptime_secs
        );
    }

    if let Some(ref etw) = report.etw_capture {
        println!(
            "ETW Capture: captured={} path={} ({})",
            etw.captured,
            etw.etl_path.display(),
            etw.detail
        );
    }
}

#[cfg(windows)]
fn collect_report(capture_etw_secs: Option<u64>) -> Result<DoctorReport> {
    use uuid::Uuid;
    use zerovdd_client::device::ControlDevice;
    use zerovdd_client::gpu::enumerate_dxgi_adapters;
    use zerovdd_client::hdr::query_os_version;
    use zerovdd_client::ClientError;

    let (installer, installer_error) = match zerovdd_installer::status() {
        Ok(st) => (Some(st), None),
        Err(err) => (None, Some(err.to_string())),
    };

    let os = match query_os_version() {
        Ok(v) => {
            let (profile, split) = classify_hdr_api_profile(v.build);
            OsDiagnosticInfo {
                version: Some(v),
                hdr_api_profile: profile.to_string(),
                split_hdr_acm_supported: split,
            }
        }
        Err(err) => OsDiagnosticInfo {
            version: None,
            hdr_api_profile: format!("unknown ({err})"),
            split_hdr_acm_supported: false,
        },
    };

    let (gpus, gpu_error) = match enumerate_dxgi_adapters() {
        Ok(list) => (list, None),
        Err(err) => (Vec::new(), Some(err.to_string())),
    };

    let wudf_host_processes = query_wudf_host_processes();

    let mut active_displays = Vec::new();
    let control_device = match ControlDevice::open() {
        Ok(dev) => {
            let ver = dev.driver_version();
            if let Ok(list) = dev.list_displays() {
                if let Ok(entries) = list.entries() {
                    for d in entries {
                        active_displays.push(DoctorDisplaySummary {
                            monitor_guid: Uuid::from_bytes(d.monitor_guid).hyphenated().to_string(),
                            client_name: d.client_name_str().unwrap_or("<invalid>").to_string(),
                            adapter_luid_low: d.adapter_luid_low,
                            adapter_luid_high: d.adapter_luid_high,
                            target_id: d.target_id,
                            width: d.width,
                            height: d.height,
                            refresh_num: d.refresh_num,
                            refresh_den: d.refresh_den,
                            lease_remaining_secs: d.lease_remaining_secs,
                        });
                    }
                }
            }
            match dev.get_status() {
                Ok(st) => ControlDeviceStatus::Accessible {
                    device_path: dev.device_path().to_string(),
                    protocol_version: (ver.major, ver.minor, ver.patch),
                    adapter_ready: st.adapter_ready != 0,
                    hdr_capable: st.hdr_capable != 0,
                    uptime_secs: st.uptime_secs,
                    last_gpu_switch_failed: st.last_gpu_switch_failed != 0,
                },
                Err(err) => ControlDeviceStatus::Error {
                    message: err.to_string(),
                },
            }
        }
        Err(ClientError::AccessDenied) => {
            // WHY: Standard/non-elevated users cannot open `\ZeroVddControl` directly, so fall back
            // to `zerovdd-svc` IPC to report active virtual displays accurately.
            if let Ok(Some(IpcResponse::DisplayList { displays })) =
                crate::cmd::display::try_send_ipc_request(&IpcRequest::DisplayList)
            {
                for d in displays {
                    active_displays.push(DoctorDisplaySummary {
                        monitor_guid: d.monitor_guid,
                        client_name: d.client_name,
                        adapter_luid_low: d.adapter_luid_low,
                        adapter_luid_high: d.adapter_luid_high,
                        target_id: d.target_id,
                        width: d.width,
                        height: d.height,
                        refresh_num: d.refresh_num,
                        refresh_den: d.refresh_den,
                        lease_remaining_secs: d.lease_remaining_secs,
                    });
                }
            }
            ControlDeviceStatus::AccessDenied
        }
        Err(ClientError::DeviceNotFound(_)) => ControlDeviceStatus::NotInstalled,
        Err(err) => ControlDeviceStatus::Error {
            message: err.to_string(),
        },
    };

    let service = query_service_diagnostic();
    let etw_capture = capture_etw_secs.map(run_etw_capture);

    Ok(DoctorReport {
        cli_version: env!("CARGO_PKG_VERSION"),
        arch: std::env::consts::ARCH,
        installer,
        installer_error,
        wudf_host_processes,
        os,
        gpus,
        gpu_error,
        control_device,
        active_displays,
        service,
        etw_capture,
    })
}

#[cfg(not(windows))]
fn collect_report(capture_etw_secs: Option<u64>) -> Result<DoctorReport> {
    let etw_capture = capture_etw_secs.map(|duration_secs| EtwCaptureReport {
        provider_name: ETW_PROVIDER_NAME.to_string(),
        provider_guid: ETW_PROVIDER_GUID.to_string(),
        etl_path: std::env::temp_dir().join("zerovdd-driver.etl"),
        duration_secs: duration_secs.max(1),
        captured: false,
        detail: "ETW capture is only supported on Windows".to_string(),
    });
    let _ = (IpcRequest::GetHealth, core::mem::size_of::<IpcResponse>());

    Ok(DoctorReport {
        cli_version: env!("CARGO_PKG_VERSION"),
        arch: std::env::consts::ARCH,
        installer: None,
        installer_error: Some(
            "Windows Driver Store inspection is only supported on Windows".to_string(),
        ),
        wudf_host_processes: Vec::new(),
        os: OsDiagnosticInfo {
            version: None,
            hdr_api_profile: "non-Windows host".to_string(),
            split_hdr_acm_supported: false,
        },
        gpus: Vec::new(),
        gpu_error: Some("DXGI GPU enumeration is only supported on Windows".to_string()),
        control_device: ControlDeviceStatus::UnsupportedPlatform,
        active_displays: Vec::new(),
        service: ServiceDiagnosticInfo {
            scm_state: "unsupported_platform".to_string(),
            ipc_health: None,
        },
        etw_capture,
    })
}

#[cfg(windows)]
fn query_service_diagnostic() -> ServiceDiagnosticInfo {
    use core::mem::size_of;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, ERROR_SERVICE_DOES_NOT_EXIST};
    use windows::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_HANDLE,
        SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO, SERVICE_PAUSED, SERVICE_QUERY_STATUS,
        SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STATUS_PROCESS, SERVICE_STOPPED,
        SERVICE_STOP_PENDING,
    };

    struct ScHandleGuard(SC_HANDLE);
    impl Drop for ScHandleGuard {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: `self.0` is a valid SCM/service handle closed once on drop.
                unsafe {
                    let _ = CloseServiceHandle(self.0);
                }
            }
        }
    }

    let _ = CloseHandle;
    let scm_state = (|| -> String {
        // SAFETY: Opens local SCM with read/connect access.
        let Ok(scm_raw) = (unsafe { OpenSCManagerW(None, None, SC_MANAGER_CONNECT) }) else {
            return "scm_unavailable".to_string();
        };
        let scm = ScHandleGuard(scm_raw);
        let wide: Vec<u16> = zerovdd_installer::device_node::SERVICE_NAME
            .encode_utf16()
            .chain(core::iter::once(0))
            .collect();
        // SAFETY: `scm.0` is valid and `wide` is NUL-terminated UTF-16.
        let svc_raw = match unsafe {
            OpenServiceW(scm.0, PCWSTR(wide.as_ptr()), SERVICE_QUERY_STATUS)
        } {
            Ok(h) => h,
            Err(err) => {
                if err.code() == windows::core::HRESULT::from_win32(ERROR_SERVICE_DOES_NOT_EXIST.0)
                {
                    return "not_installed".to_string();
                }
                return format!("open_error({:#x})", err.code().0);
            }
        };
        let svc = ScHandleGuard(svc_raw);
        let mut status = SERVICE_STATUS_PROCESS::default();
        let mut needed: u32 = 0;
        // SAFETY: `status` is stack-allocated with exact `size_of::<SERVICE_STATUS_PROCESS>()`.
        let buf = unsafe {
            core::slice::from_raw_parts_mut(
                (&raw mut status).cast::<u8>(),
                size_of::<SERVICE_STATUS_PROCESS>(),
            )
        };
        // SAFETY: `svc.0` has `SERVICE_QUERY_STATUS` access.
        if unsafe { QueryServiceStatusEx(svc.0, SC_STATUS_PROCESS_INFO, Some(buf), &mut needed) }
            .is_err()
        {
            return "query_failed".to_string();
        }
        match status.dwCurrentState {
            SERVICE_RUNNING => "running".to_string(),
            SERVICE_STOPPED => "stopped".to_string(),
            SERVICE_START_PENDING => "start_pending".to_string(),
            SERVICE_STOP_PENDING => "stop_pending".to_string(),
            SERVICE_PAUSED => "paused".to_string(),
            other => format!("state({})", other.0),
        }
    })();

    let ipc_health = match crate::cmd::display::try_send_ipc_request(&IpcRequest::GetHealth) {
        Ok(Some(IpcResponse::Health(health))) => Some(health),
        _ => None,
    };

    ServiceDiagnosticInfo {
        scm_state,
        ipc_health,
    }
}

#[cfg(windows)]
fn query_wudf_host_processes() -> Vec<WudfHostProcessInfo> {
    use core::mem::size_of;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Module32FirstW, Module32NextW, Process32FirstW, Process32NextW,
        MODULEENTRY32W, PROCESSENTRY32W, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::Threading::{
        GetProcessHandleCount, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    };

    struct SnapGuard(HANDLE);
    impl Drop for SnapGuard {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: `self.0` is a valid Win32 handle closed once on drop.
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    fn utf16_nul_to_string(buf: &[u16]) -> String {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len])
    }

    fn has_zerovdd_module(pid: u32) -> bool {
        // SAFETY: Queries modules of `pid`; may fail with access denied when un-elevated.
        let Ok(mod_snap_raw) =
            (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid) })
        else {
            return false;
        };
        let mod_snap = SnapGuard(mod_snap_raw);
        let mut me = MODULEENTRY32W {
            dwSize: size_of::<MODULEENTRY32W>() as u32,
            ..Default::default()
        };
        // SAFETY: `mod_snap.0` is valid and `me.dwSize` is initialized.
        if unsafe { Module32FirstW(mod_snap.0, &mut me) }.is_err() {
            return false;
        }
        loop {
            let name = utf16_nul_to_string(&me.szModule);
            if name.eq_ignore_ascii_case(zerovdd_installer::driver_store::DRIVER_DLL_FILENAME) {
                return true;
            }
            // SAFETY: `mod_snap.0` is valid and `me.dwSize` remains initialized.
            if unsafe { Module32NextW(mod_snap.0, &mut me) }.is_err() {
                break;
            }
        }
        false
    }

    // SAFETY: Creates a process snapshot across the local machine.
    let Ok(snap_raw) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return Vec::new();
    };
    let snap = SnapGuard(snap_raw);
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: `snap.0` is valid and `entry.dwSize` is initialized.
    if unsafe { Process32FirstW(snap.0, &mut entry) }.is_err() {
        return Vec::new();
    }

    let mut out = Vec::new();
    loop {
        let exe_name = utf16_nul_to_string(&entry.szExeFile);
        if exe_name.eq_ignore_ascii_case("WUDFHost.exe") {
            let pid = entry.th32ProcessID;
            let mut private_memory_bytes = 0u64;
            let mut working_set_bytes = 0u64;
            let mut handle_count = 0u32;

            // SAFETY: Opens `pid` with `PROCESS_QUERY_LIMITED_INFORMATION` for memory and handle metrics.
            let proc_handle = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
                    false,
                    pid,
                )
            }
            .or_else(|_| {
                // SAFETY: Fallback opens `pid` with query-limited permission when VM_READ is denied.
                unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            });

            if let Ok(h_raw) = proc_handle {
                let h = SnapGuard(h_raw);
                let mut pmc_ex = PROCESS_MEMORY_COUNTERS_EX {
                    cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
                    ..Default::default()
                };
                // SAFETY: `PROCESS_MEMORY_COUNTERS_EX` starts with `PROCESS_MEMORY_COUNTERS` layout and `cb` is set.
                let ok = unsafe {
                    K32GetProcessMemoryInfo(
                        h.0,
                        (&raw mut pmc_ex).cast::<PROCESS_MEMORY_COUNTERS>(),
                        size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
                    )
                };
                if ok.as_bool() {
                    private_memory_bytes = u64::try_from(pmc_ex.PrivateUsage).unwrap_or(0);
                    working_set_bytes = u64::try_from(pmc_ex.WorkingSetSize).unwrap_or(0);
                }
                // SAFETY: `h.0` is a valid process handle and `handle_count` is a stack `u32`.
                let _ = unsafe { GetProcessHandleCount(h.0, &mut handle_count) };
            }

            out.push(WudfHostProcessInfo {
                pid,
                private_memory_bytes,
                working_set_bytes,
                handle_count,
                hosts_zerovdd_dll: has_zerovdd_module(pid),
            });
        }

        // SAFETY: `snap.0` is valid and `entry.dwSize` remains initialized.
        if unsafe { Process32NextW(snap.0, &mut entry) }.is_err() {
            break;
        }
    }

    out
}

#[cfg(windows)]
fn run_etw_capture(duration_secs: u64) -> EtwCaptureReport {
    use std::process::Command;
    use std::time::Duration;

    let effective_secs = duration_secs.max(1);
    let session_name = "zerovdd-doctor";
    let etl_path = std::env::temp_dir().join("zerovdd-driver.etl");
    let _ = Command::new("logman")
        .args(["stop", session_name, "-ets"])
        .output();

    let start = Command::new("logman")
        .args([
            "create",
            "trace",
            session_name,
            "-p",
            ETW_PROVIDER_GUID,
            "0xffffffffffffffff",
            "5",
            "-o",
        ])
        .arg(&etl_path)
        .arg("-ets")
        .output();

    match start {
        Ok(out) if out.status.success() => {
            std::thread::sleep(Duration::from_secs(effective_secs));
            let stop = Command::new("logman")
                .args(["stop", session_name, "-ets"])
                .output();
            match stop {
                Ok(stop_out) if stop_out.status.success() => EtwCaptureReport {
                    provider_name: ETW_PROVIDER_NAME.to_string(),
                    provider_guid: ETW_PROVIDER_GUID.to_string(),
                    etl_path,
                    duration_secs: effective_secs,
                    captured: true,
                    detail: "ETW trace captured via logman".to_string(),
                },
                Ok(stop_out) => EtwCaptureReport {
                    provider_name: ETW_PROVIDER_NAME.to_string(),
                    provider_guid: ETW_PROVIDER_GUID.to_string(),
                    etl_path,
                    duration_secs: effective_secs,
                    captured: false,
                    detail: format!(
                        "logman stop failed: {}",
                        String::from_utf8_lossy(&stop_out.stderr).trim()
                    ),
                },
                Err(err) => EtwCaptureReport {
                    provider_name: ETW_PROVIDER_NAME.to_string(),
                    provider_guid: ETW_PROVIDER_GUID.to_string(),
                    etl_path,
                    duration_secs: effective_secs,
                    captured: false,
                    detail: format!("failed to stop logman: {err}"),
                },
            }
        }
        Ok(out) => EtwCaptureReport {
            provider_name: ETW_PROVIDER_NAME.to_string(),
            provider_guid: ETW_PROVIDER_GUID.to_string(),
            etl_path,
            duration_secs: effective_secs,
            captured: false,
            detail: format!(
                "logman start failed (administrator privileges required): {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        },
        Err(err) => EtwCaptureReport {
            provider_name: ETW_PROVIDER_NAME.to_string(),
            provider_guid: ETW_PROVIDER_GUID.to_string(),
            etl_path,
            duration_secs: effective_secs,
            captured: false,
            detail: format!("failed to launch logman: {err}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_24h2_vs_earlier_hdr_api_profiles() {
        let (label_24h2, split_24h2) = classify_hdr_api_profile(26_100);
        assert!(split_24h2);
        assert!(label_24h2.contains("24H2"));

        let (label_23h2, split_23h2) = classify_hdr_api_profile(22_631);
        assert!(!split_23h2);
        assert!(label_23h2.contains("23H2"));
    }
}
