//! Virtual root device-node (`Root\ZeroVdd`) enumeration, creation, removal, and service stop helpers.

use serde::{Deserialize, Serialize};

/// PnP hardware ID declared by `zerovdd.inf`.
pub const HARDWARE_ID: &str = r"Root\ZeroVdd";

/// Windows Display device setup class GUID string (`{4d36e968-e325-11ce-bfc1-08002be10318}`).
pub const DISPLAY_CLASS_GUID_STR: &str = "{4d36e968-e325-11ce-bfc1-08002be10318}";

/// Windows service name for the `zerovdd` Session-0 management service.
pub const SERVICE_NAME: &str = "zerovdd-svc";

/// Summary of an existing `Root\ZeroVdd` PnP device node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceNodeInfo {
    /// PnP device instance ID (for example `ROOT\ZEROVDD\0000` or `ROOT\DISPLAY\0000`).
    pub instance_id: String,
    /// Multi-SZ hardware IDs reported by `SPDRP_HARDWAREID`.
    pub hardware_ids: Vec<String>,
    /// Whether the device node is currently present (`DN_STARTED` / located in live PnP tree).
    pub present: bool,
    /// Whether Configuration Manager reports `DN_HAS_PROBLEM` on this node.
    pub has_problem: bool,
    /// Configuration Manager problem code (`0` when healthy).
    pub problem_code: u32,
    /// Raw Configuration Manager `DN_*` status bitmask.
    pub status_flags: u32,
}

/// Outcome of requesting a graceful stop of `zerovdd-svc` prior to driver replacement or removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStopOutcome {
    /// The service is not registered in the Windows Service Control Manager.
    NotInstalled,
    /// The service was already in `SERVICE_STOPPED` state.
    AlreadyStopped,
    /// The service was running or pending and transitioned cleanly to `SERVICE_STOPPED`.
    Stopped,
}

/// Parses a UTF-16 `REG_MULTI_SZ` buffer into individual non-empty strings.
pub fn parse_multi_sz_utf16(buf: &[u16]) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < buf.len() {
        let Some(rel_end) = buf[start..].iter().position(|&c| c == 0) else {
            let tail = String::from_utf16_lossy(&buf[start..]);
            if !tail.is_empty() {
                out.push(tail);
            }
            break;
        };
        if rel_end == 0 {
            break;
        }
        let end = start + rel_end;
        out.push(String::from_utf16_lossy(&buf[start..end]));
        start = end + 1;
    }
    out
}

/// Encodes a single hardware ID as a UTF-16LE `REG_MULTI_SZ` byte buffer (`...\0\0`).
pub fn encode_hardware_id_multi_sz(hardware_id: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity((hardware_id.len() + 2) * 2);
    for unit in hardware_id.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes
}

/// Returns `true` if any reported hardware ID matches [`HARDWARE_ID`] case-insensitively.
pub fn matches_zerovdd_hardware_id(hardware_ids: &[String]) -> bool {
    hardware_ids
        .iter()
        .any(|id| id.trim().eq_ignore_ascii_case(HARDWARE_ID))
}

#[cfg(windows)]
mod win32 {
    use core::mem::size_of;
    use std::time::{Duration, Instant};

    use windows::core::{BOOL, GUID, HRESULT, PCWSTR, PWSTR};
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_DevNode_Status, CM_Locate_DevNodeW, DiInstallDevice, DiUninstallDevice,
        SetupDiCallClassInstaller, SetupDiCreateDeviceInfoList, SetupDiCreateDeviceInfoW,
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
        SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceRegistryPropertyW,
        SetupDiSetDeviceRegistryPropertyW, CM_DEVNODE_STATUS_FLAGS, CM_LOCATE_DEVNODE_PHANTOM,
        CM_PROB, CR_SUCCESS, DICD_GENERATE_ID, DIF_REGISTERDEVICE, DIINSTALLDEVICE_FLAGS,
        DI_NEEDREBOOT, DI_NEEDRESTART, DN_HAS_PROBLEM, HDEVINFO, SETUP_DI_GET_CLASS_DEVS_FLAGS,
        SPDRP_HARDWAREID, SP_DEVINFO_DATA,
    };
    use windows::Win32::Foundation::{
        GetLastError, ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_ITEMS,
        ERROR_SERVICE_CANNOT_ACCEPT_CTRL, ERROR_SERVICE_DOES_NOT_EXIST,
        ERROR_SERVICE_MARKED_FOR_DELETE, ERROR_SERVICE_NOT_ACTIVE, ERROR_SUCCESS_REBOOT_REQUIRED,
        HWND,
    };
    use windows::Win32::System::Services::{
        CloseServiceHandle, ControlService, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx,
        SC_HANDLE, SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO, SERVICE_CONTROL_STOP,
        SERVICE_QUERY_STATUS, SERVICE_STATUS, SERVICE_STATUS_PROCESS, SERVICE_STOP,
        SERVICE_STOPPED, SERVICE_STOP_PENDING,
    };

    use super::{
        encode_hardware_id_multi_sz, matches_zerovdd_hardware_id, parse_multi_sz_utf16,
        DeviceNodeInfo, ServiceStopOutcome, HARDWARE_ID,
    };
    use crate::InstallerError;

    const DISPLAY_CLASS_GUID: GUID = GUID::from_values(
        0x4d36_e968,
        0xe325,
        0x11ce,
        [0xbf, 0xc1, 0x08, 0x00, 0x2b, 0xe1, 0x03, 0x18],
    );

    struct OwnedDevInfoSet(HDEVINFO);

    impl Drop for OwnedDevInfoSet {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: `self.0` was created by SetupAPI and is destroyed once on drop.
                unsafe {
                    let _ = SetupDiDestroyDeviceInfoList(self.0);
                }
            }
        }
    }

    struct OwnedServiceHandle(SC_HANDLE);

    impl Drop for OwnedServiceHandle {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: `self.0` is an open SCM/service handle closed once on drop.
                unsafe {
                    let _ = CloseServiceHandle(self.0);
                }
            }
        }
    }

    fn utf16_nul_to_string(buf: &[u16]) -> String {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len])
    }

    fn get_device_instance_id(
        dev_info: HDEVINFO,
        dev_data: &SP_DEVINFO_DATA,
    ) -> Result<String, InstallerError> {
        let mut required_chars: u32 = 0;
        // SAFETY: Querying required UTF-16 buffer length with `None` output buffer.
        let _ = unsafe {
            SetupDiGetDeviceInstanceIdW(dev_info, dev_data, None, Some(&mut required_chars))
        };
        if required_chars == 0 {
            return Ok(String::new());
        }
        let mut buf = vec![0u16; required_chars as usize];
        // SAFETY: `buf` has capacity `required_chars` UTF-16 code units.
        unsafe {
            SetupDiGetDeviceInstanceIdW(
                dev_info,
                dev_data,
                Some(&mut buf),
                Some(&mut required_chars),
            )
        }
        .map_err(|err| InstallerError::Win32 {
            operation: "SetupDiGetDeviceInstanceIdW",
            code: err.code().0 as u32,
        })?;
        Ok(utf16_nul_to_string(&buf))
    }

    fn get_hardware_ids(dev_info: HDEVINFO, dev_data: &SP_DEVINFO_DATA) -> Vec<String> {
        let mut required_bytes: u32 = 0;
        // SAFETY: First call queries `SPDRP_HARDWAREID` byte length into `required_bytes`.
        let first_res = unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                dev_info,
                dev_data,
                SPDRP_HARDWAREID,
                None,
                None,
                Some(&mut required_bytes),
            )
        };
        if first_res.is_ok() && required_bytes == 0 {
            return Vec::new();
        }
        if let Err(err) = first_res {
            if err.code() != HRESULT::from_win32(ERROR_INSUFFICIENT_BUFFER.0) || required_bytes == 0
            {
                return Vec::new();
            }
        }

        let mut raw = vec![0u8; required_bytes as usize];
        // SAFETY: `raw` is uniquely owned with `required_bytes` capacity.
        let ok = unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                dev_info,
                dev_data,
                SPDRP_HARDWAREID,
                None,
                Some(&mut raw),
                Some(&mut required_bytes),
            )
        };
        if ok.is_err() {
            return Vec::new();
        }
        let (word_pairs, _) = raw.as_chunks::<2>();
        let words: Vec<u16> = word_pairs
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        parse_multi_sz_utf16(&words)
    }

    fn query_devnode_status(instance_id: &str, devinst: u32) -> (bool, bool, u32, u32) {
        let mut status = CM_DEVNODE_STATUS_FLAGS(0);
        let mut problem = CM_PROB(0);
        // SAFETY: `status` and `problem` are initialized stack pointers.
        let cr = unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, devinst, 0) };
        if cr == CR_SUCCESS {
            let has_problem = (status.0 & DN_HAS_PROBLEM.0) != 0 || problem.0 != 0;
            return (true, has_problem, problem.0, status.0);
        }

        let wide_id: Vec<u16> = instance_id
            .encode_utf16()
            .chain(core::iter::once(0))
            .collect();
        let mut phantom_inst: u32 = 0;
        // SAFETY: `wide_id` is NUL-terminated UTF-16 and `phantom_inst` receives the devnode handle.
        let phantom_cr = unsafe {
            CM_Locate_DevNodeW(
                &mut phantom_inst,
                PCWSTR(wide_id.as_ptr()),
                CM_LOCATE_DEVNODE_PHANTOM,
            )
        };
        let _ = phantom_cr;
        (false, false, 0, 0)
    }

    /// Enumerates all existing `Root\ZeroVdd` PnP device nodes (present and non-present).
    pub fn enumerate_zerovdd_nodes() -> Result<Vec<DeviceNodeInfo>, InstallerError> {
        // SAFETY: Querying all Display-class devices (including non-present/disabled nodes)
        // prevents duplicate virtual adapters during reinstall or upgrade.
        let raw_set = unsafe {
            SetupDiGetClassDevsW(
                Some(&DISPLAY_CLASS_GUID),
                None,
                None,
                SETUP_DI_GET_CLASS_DEVS_FLAGS(0),
            )
        }
        .map_err(|err| InstallerError::Win32 {
            operation: "SetupDiGetClassDevsW",
            code: err.code().0 as u32,
        })?;
        let dev_info = OwnedDevInfoSet(raw_set);

        let mut nodes = Vec::new();
        let mut index: u32 = 0;
        loop {
            let mut dev_data = SP_DEVINFO_DATA {
                cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };
            // SAFETY: `dev_info.0` is valid and `dev_data.cbSize` is initialized.
            let res = unsafe { SetupDiEnumDeviceInfo(dev_info.0, index, &mut dev_data) };
            index = index.saturating_add(1);
            if let Err(err) = res {
                if err.code() == HRESULT::from_win32(ERROR_NO_MORE_ITEMS.0) {
                    break;
                }
                return Err(InstallerError::Win32 {
                    operation: "SetupDiEnumDeviceInfo",
                    code: err.code().0 as u32,
                });
            }

            let hardware_ids = get_hardware_ids(dev_info.0, &dev_data);
            if !matches_zerovdd_hardware_id(&hardware_ids) {
                continue;
            }
            let instance_id = get_device_instance_id(dev_info.0, &dev_data)?;
            let (present, has_problem, problem_code, status_flags) =
                query_devnode_status(&instance_id, dev_data.DevInst);

            nodes.push(DeviceNodeInfo {
                instance_id,
                hardware_ids,
                present,
                has_problem,
                problem_code,
                status_flags,
            });
        }

        Ok(nodes)
    }

    /// Creates a single `Root\ZeroVdd` PnP device node and binds the staged driver to it.
    pub fn create_root_device_node() -> Result<(String, bool), InstallerError> {
        // SAFETY: Creates an empty Display-class device info list.
        let raw_set = unsafe { SetupDiCreateDeviceInfoList(Some(&DISPLAY_CLASS_GUID), None) }
            .map_err(|err| InstallerError::Win32 {
                operation: "SetupDiCreateDeviceInfoList",
                code: err.code().0 as u32,
            })?;
        let dev_info = OwnedDevInfoSet(raw_set);

        let dev_name: Vec<u16> = "ZeroVdd"
            .encode_utf16()
            .chain(core::iter::once(0))
            .collect();
        let mut dev_data = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };

        // SAFETY: `dev_info.0` is valid, `dev_name` is NUL-terminated UTF-16, and `DICD_GENERATE_ID`
        // yields a unique root-enumerated instance under Display class.
        unsafe {
            SetupDiCreateDeviceInfoW(
                dev_info.0,
                PCWSTR(dev_name.as_ptr()),
                &DISPLAY_CLASS_GUID,
                None,
                None,
                DICD_GENERATE_ID,
                Some(&mut dev_data),
            )
        }
        .map_err(|err| InstallerError::Win32 {
            operation: "SetupDiCreateDeviceInfoW",
            code: err.code().0 as u32,
        })?;

        let hw_id_bytes = encode_hardware_id_multi_sz(HARDWARE_ID);
        // SAFETY: `hw_id_bytes` is a double-NUL-terminated UTF-16LE `REG_MULTI_SZ` buffer.
        unsafe {
            SetupDiSetDeviceRegistryPropertyW(
                dev_info.0,
                &mut dev_data,
                SPDRP_HARDWAREID,
                Some(&hw_id_bytes),
            )
        }
        .map_err(|err| InstallerError::Win32 {
            operation: "SetupDiSetDeviceRegistryPropertyW(SPDRP_HARDWAREID)",
            code: err.code().0 as u32,
        })?;

        // SAFETY: Registers the newly created root device node in PnP.
        unsafe { SetupDiCallClassInstaller(DIF_REGISTERDEVICE, dev_info.0, Some(&dev_data)) }
            .map_err(|err| InstallerError::Win32 {
                operation: "SetupDiCallClassInstaller(DIF_REGISTERDEVICE)",
                code: err.code().0 as u32,
            })?;

        let mut need_reboot = BOOL(0);
        // SAFETY: Installs the best matching driver from Driver Store onto the registered node.
        let install_res = unsafe {
            DiInstallDevice(
                None,
                dev_info.0,
                &dev_data,
                None,
                DIINSTALLDEVICE_FLAGS(0),
                Some(&mut need_reboot),
            )
        };
        // SAFETY: `GetLastError` reads thread-local error/reboot status immediately after `DiInstallDevice`.
        let last_err = unsafe { GetLastError() };
        if let Err(err) = install_res {
            if err.code() != HRESULT::from_win32(ERROR_SUCCESS_REBOOT_REQUIRED.0) {
                return Err(InstallerError::Win32 {
                    operation: "DiInstallDevice",
                    code: err.code().0 as u32,
                });
            }
            need_reboot = BOOL(1);
        } else if last_err == ERROR_SUCCESS_REBOOT_REQUIRED {
            need_reboot = BOOL(1);
        }

        let reboot_flag = need_reboot.as_bool() || last_err == ERROR_SUCCESS_REBOOT_REQUIRED;
        let _ = (DI_NEEDREBOOT, DI_NEEDRESTART, PWSTR::null());
        let instance_id = get_device_instance_id(dev_info.0, &dev_data)?;
        Ok((instance_id, reboot_flag))
    }

    /// Uninstalls all PnP device nodes matching `Root\ZeroVdd` via `DiUninstallDevice`.
    pub fn uninstall_all_zerovdd_nodes() -> Result<(Vec<String>, bool), InstallerError> {
        // SAFETY: Enumerates all Display-class devices so present and phantom `Root\ZeroVdd` nodes are removed.
        let raw_set = unsafe {
            SetupDiGetClassDevsW(
                Some(&DISPLAY_CLASS_GUID),
                None,
                None,
                SETUP_DI_GET_CLASS_DEVS_FLAGS(0),
            )
        }
        .map_err(|err| InstallerError::Win32 {
            operation: "SetupDiGetClassDevsW",
            code: err.code().0 as u32,
        })?;
        let dev_info = OwnedDevInfoSet(raw_set);

        let mut removed = Vec::new();
        let mut reboot_required = false;
        let mut index: u32 = 0;

        loop {
            let mut dev_data = SP_DEVINFO_DATA {
                cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };
            // SAFETY: `dev_info.0` is valid and `dev_data.cbSize` is initialized.
            let res = unsafe { SetupDiEnumDeviceInfo(dev_info.0, index, &mut dev_data) };
            index = index.saturating_add(1);
            if let Err(err) = res {
                if err.code() == HRESULT::from_win32(ERROR_NO_MORE_ITEMS.0) {
                    break;
                }
                return Err(InstallerError::Win32 {
                    operation: "SetupDiEnumDeviceInfo",
                    code: err.code().0 as u32,
                });
            }

            let hardware_ids = get_hardware_ids(dev_info.0, &dev_data);
            if !matches_zerovdd_hardware_id(&hardware_ids) {
                continue;
            }
            let instance_id = get_device_instance_id(dev_info.0, &dev_data)?;

            let mut need_reboot = BOOL(0);
            // SAFETY: Uninstalls the matched `Root\ZeroVdd` device node from PnP.
            let uninst_res = unsafe {
                DiUninstallDevice(
                    HWND::default(),
                    dev_info.0,
                    &dev_data,
                    0,
                    Some(&mut need_reboot),
                )
            };
            // SAFETY: `GetLastError` reads thread-local error/reboot status immediately after `DiUninstallDevice`.
            let last_err = unsafe { GetLastError() };
            if let Err(err) = uninst_res {
                if err.code() != HRESULT::from_win32(ERROR_SUCCESS_REBOOT_REQUIRED.0) {
                    return Err(InstallerError::Win32 {
                        operation: "DiUninstallDevice",
                        code: err.code().0 as u32,
                    });
                }
                reboot_required = true;
            } else if need_reboot.as_bool() || last_err == ERROR_SUCCESS_REBOOT_REQUIRED {
                reboot_required = true;
            }

            removed.push(instance_id);
        }

        Ok((removed, reboot_required))
    }

    fn query_service_state(svc: SC_HANDLE) -> Result<u32, InstallerError> {
        let mut status = SERVICE_STATUS_PROCESS::default();
        let mut needed: u32 = 0;
        // SAFETY: `status` is stack-allocated with exact `size_of::<SERVICE_STATUS_PROCESS>()`.
        let buf = unsafe {
            core::slice::from_raw_parts_mut(
                (&raw mut status).cast::<u8>(),
                size_of::<SERVICE_STATUS_PROCESS>(),
            )
        };
        // SAFETY: `svc` is an open `SC_HANDLE` with `SERVICE_QUERY_STATUS` access.
        unsafe { QueryServiceStatusEx(svc, SC_STATUS_PROCESS_INFO, Some(buf), &mut needed) }
            .map_err(|err| InstallerError::Win32 {
                operation: "QueryServiceStatusEx",
                code: err.code().0 as u32,
            })?;
        Ok(status.dwCurrentState.0)
    }

    /// Gracefully stops `service_name` if installed and running, waiting up to 15 seconds for stop.
    pub fn stop_service_if_running(
        service_name: &str,
    ) -> Result<ServiceStopOutcome, InstallerError> {
        // SAFETY: Opens the local Service Control Manager with connect permission.
        let scm_raw = unsafe { OpenSCManagerW(None, None, SC_MANAGER_CONNECT) }.map_err(|err| {
            InstallerError::Win32 {
                operation: "OpenSCManagerW",
                code: err.code().0 as u32,
            }
        })?;
        let scm = OwnedServiceHandle(scm_raw);

        let wide_name: Vec<u16> = service_name
            .encode_utf16()
            .chain(core::iter::once(0))
            .collect();
        // SAFETY: `scm.0` is valid and `wide_name` is NUL-terminated UTF-16.
        let svc_raw = match unsafe {
            OpenServiceW(
                scm.0,
                PCWSTR(wide_name.as_ptr()),
                SERVICE_QUERY_STATUS | SERVICE_STOP,
            )
        } {
            Ok(h) => h,
            Err(err) => {
                let code = err.code();
                if code == HRESULT::from_win32(ERROR_SERVICE_DOES_NOT_EXIST.0)
                    || code == HRESULT::from_win32(ERROR_SERVICE_MARKED_FOR_DELETE.0)
                {
                    return Ok(ServiceStopOutcome::NotInstalled);
                }
                return Err(InstallerError::Win32 {
                    operation: "OpenServiceW",
                    code: code.0 as u32,
                });
            }
        };
        let svc = OwnedServiceHandle(svc_raw);

        let current_state = query_service_state(svc.0)?;
        if current_state == SERVICE_STOPPED.0 {
            return Ok(ServiceStopOutcome::AlreadyStopped);
        }

        if current_state != SERVICE_STOP_PENDING.0 {
            let mut control_status = SERVICE_STATUS::default();
            // SAFETY: Sends `SERVICE_CONTROL_STOP` to `svc.0` so the service restores physical displays cleanly.
            if let Err(err) =
                unsafe { ControlService(svc.0, SERVICE_CONTROL_STOP, &mut control_status) }
            {
                let code = err.code();
                if code == HRESULT::from_win32(ERROR_SERVICE_NOT_ACTIVE.0)
                    || code == HRESULT::from_win32(ERROR_SERVICE_MARKED_FOR_DELETE.0)
                {
                    return Ok(ServiceStopOutcome::AlreadyStopped);
                }
                if code != HRESULT::from_win32(ERROR_SERVICE_CANNOT_ACCEPT_CTRL.0) {
                    return Err(InstallerError::Win32 {
                        operation: "ControlService(SERVICE_CONTROL_STOP)",
                        code: code.0 as u32,
                    });
                }
            }
        }

        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if query_service_state(svc.0)? == SERVICE_STOPPED.0 {
                return Ok(ServiceStopOutcome::Stopped);
            }
            if Instant::now() >= deadline {
                return Err(InstallerError::ServiceStopTimeout(service_name.to_string()));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[cfg(windows)]
pub use win32::{
    create_root_device_node, enumerate_zerovdd_nodes, stop_service_if_running,
    uninstall_all_zerovdd_nodes,
};
