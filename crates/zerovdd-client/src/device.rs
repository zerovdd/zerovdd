//! Privileged Control Device discovery, protocol handshake, and typed IOCTL client.

use zerovdd_proto::ioctl::PROTOCOL_MAJOR;
use zerovdd_proto::types::{
    AddDisplayRequest, AddDisplayResponse, DriverStatusResponse, ListDisplaysResponse,
    ProtocolVersion, RemoveDisplayRequest, RenewLeaseRequest, SetRenderAdapterRequest,
    UpdateDisplayModeRequest, UpdateDisplayModeResponse,
};

use crate::error::ClientError;

/// Reference string appended by the UMDF driver when registering its control device interface.
pub const CONTROL_INTERFACE_REFERENCE_STRING: &str = "ZeroVddControl";

/// Portable abstraction over the `zerovdd` Control Device IOCTL interface.
pub trait ControlClient {
    /// Queries the driver's supported IOCTL protocol version.
    fn get_version(&self) -> Result<ProtocolVersion, ClientError>;

    /// Attaches or idempotently updates a virtual display via `IOCTL_ADD_DISPLAY`.
    fn add_display(&self, request: &AddDisplayRequest) -> Result<AddDisplayResponse, ClientError>;

    /// Departs and removes an active virtual display via `IOCTL_REMOVE_DISPLAY`.
    fn remove_display(&self, request: &RemoveDisplayRequest) -> Result<(), ClientError>;

    /// Enumerates all active virtual displays via `IOCTL_LIST_DISPLAYS`.
    fn list_displays(&self) -> Result<ListDisplaysResponse, ClientError>;

    /// Requests a runtime render GPU adapter LUID switch via `IOCTL_SET_RENDER_ADAPTER`.
    fn set_render_adapter(&self, request: &SetRenderAdapterRequest) -> Result<(), ClientError>;

    /// Extends the TTL lease on a leased virtual display via `IOCTL_RENEW_LEASE`.
    fn renew_lease(&self, request: &RenewLeaseRequest) -> Result<(), ClientError>;

    /// Queries driver health and adapter telemetry via `IOCTL_GET_STATUS`.
    fn get_status(&self) -> Result<DriverStatusResponse, ClientError>;

    /// Updates the supported target mode list on an active virtual display in place.
    fn update_display_mode(
        &self,
        request: &UpdateDisplayModeRequest,
    ) -> Result<UpdateDisplayModeResponse, ClientError>;
}

/// Validates that the driver's reported protocol major version matches [`PROTOCOL_MAJOR`].
pub fn validate_protocol_handshake(version: ProtocolVersion) -> Result<(), ClientError> {
    if version.major != PROTOCOL_MAJOR {
        return Err(ClientError::ProtocolMismatch {
            expected_major: PROTOCOL_MAJOR,
            actual_major: version.major,
        });
    }
    Ok(())
}

/// Parses a Windows `MULTI_SZ` UTF-16 buffer into individual non-empty device interface strings.
pub fn parse_multi_sz_utf16(buf: &[u16]) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < buf.len() {
        let Some(rel_end) = buf[start..].iter().position(|&ch| ch == 0) else {
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

/// Selects the privileged Control Device interface path ending in `\ZeroVddControl` (case-insensitive).
pub fn select_control_device_path(interfaces: &[String]) -> Option<String> {
    let expected_suffix = format!(r"\{CONTROL_INTERFACE_REFERENCE_STRING}");
    interfaces
        .iter()
        .find(|path| ends_with_ascii_ignore_case(path, &expected_suffix))
        .cloned()
}

fn ends_with_ascii_ignore_case(haystack: &str, suffix: &str) -> bool {
    if haystack.len() < suffix.len() {
        return false;
    }
    let start = haystack.len() - suffix.len();
    haystack.is_char_boundary(start) && haystack[start..].eq_ignore_ascii_case(suffix)
}

const HRESULT_ERROR_FILE_NOT_FOUND: u32 = 0x8007_0002;
const HRESULT_ERROR_PATH_NOT_FOUND: u32 = 0x8007_0003;
const HRESULT_ERROR_ACCESS_DENIED: u32 = 0x8007_0005;
const HRESULT_ERROR_NOT_FOUND: u32 = 0x8007_0490;

/// Maps a Win32 `HRESULT` failure from Control Device open or IOCTL calls into [`ClientError`].
pub fn map_win32_hresult(
    operation: &'static str,
    code: u32,
    path: &str,
    is_ioctl: bool,
) -> ClientError {
    match code {
        HRESULT_ERROR_ACCESS_DENIED => ClientError::AccessDenied,
        HRESULT_ERROR_FILE_NOT_FOUND | HRESULT_ERROR_PATH_NOT_FOUND => {
            ClientError::DeviceNotFound(path.to_string())
        }
        // WHY: The driver translates EngineError::MonitorNotFound to STATUS_NOT_FOUND, which
        // Windows surfaces from DeviceIoControl as ERROR_NOT_FOUND (1168 / 0x80070490).
        HRESULT_ERROR_NOT_FOUND if is_ioctl => ClientError::MonitorNotFound,
        _ => ClientError::Win32 { operation, code },
    }
}

#[cfg(windows)]
mod win32 {
    use core::ffi::c_void;

    use windows::core::{GUID, PCWSTR};
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_Device_Interface_ListW, CM_Get_Device_Interface_List_SizeW,
        CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CR_BUFFER_SMALL, CR_SUCCESS,
    };
    use windows::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::IO::DeviceIoControl;
    use zerovdd_proto::ioctl::{
        CONTROL_DEVICE_DOS_PATH, IOCTL_ADD_DISPLAY, IOCTL_GET_STATUS, IOCTL_GET_VERSION,
        IOCTL_LIST_DISPLAYS, IOCTL_REMOVE_DISPLAY, IOCTL_RENEW_LEASE, IOCTL_SET_RENDER_ADAPTER,
        IOCTL_UPDATE_DISPLAY_MODE, ZEROVDD_CONTROL_INTERFACE_GUID,
    };
    use zerovdd_proto::types::{
        AddDisplayRequest, AddDisplayResponse, DriverStatusResponse, ListDisplaysResponse,
        ProtocolVersion, RemoveDisplayRequest, RenewLeaseRequest, SetRenderAdapterRequest,
        UpdateDisplayModeRequest, UpdateDisplayModeResponse,
    };
    use zerovdd_proto::validation::{ProtoError, Validate};

    use super::{
        map_win32_hresult, parse_multi_sz_utf16, select_control_device_path,
        validate_protocol_handshake, ControlClient,
    };
    use crate::error::ClientError;

    struct OwnedHandle(HANDLE);

    // SAFETY: Win32 file handles are kernel object references safe to transfer between threads.
    unsafe impl Send for OwnedHandle {}
    // SAFETY: Synchronous DeviceIoControl calls on a shared handle serialize in kernel I/O manager.
    unsafe impl Sync for OwnedHandle {}

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_invalid() && self.0 != HANDLE::default() {
                // SAFETY: `self.0` was opened via `CreateFileW` and is closed exactly once on drop.
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    fn control_interface_guid() -> GUID {
        GUID::from_values(
            ZEROVDD_CONTROL_INTERFACE_GUID.data1,
            ZEROVDD_CONTROL_INTERFACE_GUID.data2,
            ZEROVDD_CONTROL_INTERFACE_GUID.data3,
            ZEROVDD_CONTROL_INTERFACE_GUID.data4,
        )
    }

    fn map_win32_error(
        operation: &'static str,
        err: &windows::core::Error,
        path: &str,
        is_ioctl: bool,
    ) -> ClientError {
        map_win32_hresult(operation, err.code().0 as u32, path, is_ioctl)
    }

    /// Enumerates present device interface paths registered for `ZEROVDD_CONTROL_INTERFACE_GUID`.
    pub fn enumerate_control_device_interfaces() -> Result<Vec<String>, ClientError> {
        let guid = control_interface_guid();
        // WHY: Device interfaces may arrive between querying buffer size and populating the list.
        for _ in 0..4 {
            let mut len: u32 = 0;
            // SAFETY: `len` points to a valid `u32`; `PCWSTR::null()` enumerates all devices for `guid`.
            let cr = unsafe {
                CM_Get_Device_Interface_List_SizeW(
                    &mut len,
                    &guid,
                    PCWSTR::null(),
                    CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
                )
            };
            if cr != CR_SUCCESS {
                return Err(ClientError::Win32 {
                    operation: "CM_Get_Device_Interface_List_SizeW",
                    code: cr.0,
                });
            }
            if len <= 1 {
                return Ok(Vec::new());
            }

            let mut buf = vec![0u16; len as usize];
            // SAFETY: `buf` is uniquely owned with capacity `len` UTF-16 code units.
            let cr = unsafe {
                CM_Get_Device_Interface_ListW(
                    &guid,
                    PCWSTR::null(),
                    &mut buf,
                    CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
                )
            };
            if cr == CR_SUCCESS {
                return Ok(parse_multi_sz_utf16(&buf));
            }
            if cr != CR_BUFFER_SMALL {
                return Err(ClientError::Win32 {
                    operation: "CM_Get_Device_Interface_ListW",
                    code: cr.0,
                });
            }
        }

        Err(ClientError::Win32 {
            operation: "CM_Get_Device_Interface_ListW",
            code: CR_BUFFER_SMALL.0,
        })
    }

    /// Open handle to the privileged `zerovdd` WDF Control Device.
    pub struct ControlDevice {
        handle: OwnedHandle,
        device_path: String,
        driver_version: ProtocolVersion,
    }

    impl ControlDevice {
        /// Discovers and opens the privileged `zerovdd` Control Device, then verifies `GET_VERSION`.
        ///
        /// Prefers a `CM_Get_Device_Interface_ListW` interface path ending in `\ZeroVddControl`,
        /// falling back to `\\.\ZeroVddControl` only when no matching device interface is present.
        pub fn open() -> Result<Self, ClientError> {
            let interfaces = enumerate_control_device_interfaces().unwrap_or_default();
            let path = select_control_device_path(&interfaces)
                .unwrap_or_else(|| CONTROL_DEVICE_DOS_PATH.to_string());
            Self::open_path(&path)
        }

        /// Opens a specific Win32 device path and immediately performs the `IOCTL_GET_VERSION` handshake.
        pub fn open_path(path: &str) -> Result<Self, ClientError> {
            let wide_path: Vec<u16> = path.encode_utf16().chain(core::iter::once(0)).collect();
            // SAFETY: `wide_path` is NUL-terminated UTF-16 and lives for the duration of `CreateFileW`.
            let raw_handle = unsafe {
                CreateFileW(
                    PCWSTR(wide_path.as_ptr()),
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                )
            }
            .map_err(|err| map_win32_error("CreateFileW", &err, path, false))?;

            if raw_handle == INVALID_HANDLE_VALUE || raw_handle.is_invalid() {
                return Err(ClientError::DeviceNotFound(path.to_string()));
            }

            let mut device = Self {
                handle: OwnedHandle(raw_handle),
                device_path: path.to_string(),
                driver_version: ProtocolVersion::CURRENT,
            };
            let version = device.get_version()?;
            validate_protocol_handshake(version)?;
            device.driver_version = version;
            Ok(device)
        }

        /// Returns the Win32 path used to open this Control Device handle.
        pub fn device_path(&self) -> &str {
            &self.device_path
        }

        /// Returns the [`ProtocolVersion`] reported by the driver during open handshake.
        pub fn driver_version(&self) -> ProtocolVersion {
            self.driver_version
        }

        fn ioctl_raw(
            &self,
            code: u32,
            input: Option<&[u8]>,
            output: Option<&mut [u8]>,
            op_name: &'static str,
        ) -> Result<usize, ClientError> {
            let (in_ptr, in_len) = match input {
                Some(bytes) => {
                    let len = u32::try_from(bytes.len()).map_err(|_| {
                        ClientError::InvalidInput(format!("{op_name} input buffer exceeds u32"))
                    })?;
                    (Some(bytes.as_ptr().cast::<c_void>()), len)
                }
                None => (None, 0),
            };

            let (out_ptr, out_len) = match output {
                Some(bytes) => {
                    let len = u32::try_from(bytes.len()).map_err(|_| {
                        ClientError::InvalidInput(format!("{op_name} output buffer exceeds u32"))
                    })?;
                    (Some(bytes.as_mut_ptr().cast::<c_void>()), len)
                }
                None => (None, 0),
            };

            let mut bytes_returned: u32 = 0;
            // SAFETY: `self.handle.0` is an open Win32 device handle; input/output pointers and
            // byte lengths match the slice bounds for the synchronous `DeviceIoControl` call.
            unsafe {
                DeviceIoControl(
                    self.handle.0,
                    code,
                    in_ptr,
                    in_len,
                    out_ptr,
                    out_len,
                    Some(&mut bytes_returned),
                    None,
                )
            }
            .map_err(|err| map_win32_error(op_name, &err, &self.device_path, true))?;

            Ok(bytes_returned as usize)
        }

        /// Queries the driver's supported IOCTL protocol version via `IOCTL_GET_VERSION`.
        pub fn get_version(&self) -> Result<ProtocolVersion, ClientError> {
            let mut out = [0u8; ProtocolVersion::WIRE_SIZE];
            let returned = self.ioctl_raw(
                IOCTL_GET_VERSION,
                None,
                Some(&mut out),
                "DeviceIoControl(IOCTL_GET_VERSION)",
            )?;
            let slice = out.get(..returned).ok_or(ProtoError::BufferSizeMismatch {
                expected: out.len(),
                actual: returned,
            })?;
            let version = ProtocolVersion::from_bytes(slice)?;
            validate_protocol_handshake(version)?;
            Ok(version)
        }

        /// Attaches or idempotently reuses a virtual display via `IOCTL_ADD_DISPLAY`.
        pub fn add_display(
            &self,
            request: &AddDisplayRequest,
        ) -> Result<AddDisplayResponse, ClientError> {
            request.validate()?;
            let in_bytes = request.to_bytes();
            let mut out = [0u8; AddDisplayResponse::WIRE_SIZE];
            let returned = self.ioctl_raw(
                IOCTL_ADD_DISPLAY,
                Some(&in_bytes),
                Some(&mut out),
                "DeviceIoControl(IOCTL_ADD_DISPLAY)",
            )?;
            let slice = out.get(..returned).ok_or(ProtoError::BufferSizeMismatch {
                expected: out.len(),
                actual: returned,
            })?;
            Ok(AddDisplayResponse::from_bytes(slice)?)
        }

        /// Departs and removes a virtual display via `IOCTL_REMOVE_DISPLAY`.
        pub fn remove_display(&self, request: &RemoveDisplayRequest) -> Result<(), ClientError> {
            request.validate()?;
            let in_bytes = request.to_bytes();
            let _ = self.ioctl_raw(
                IOCTL_REMOVE_DISPLAY,
                Some(&in_bytes),
                None,
                "DeviceIoControl(IOCTL_REMOVE_DISPLAY)",
            )?;
            Ok(())
        }

        /// Enumerates all active virtual displays via `IOCTL_LIST_DISPLAYS`.
        pub fn list_displays(&self) -> Result<ListDisplaysResponse, ClientError> {
            let mut out = vec![0u8; ListDisplaysResponse::WIRE_SIZE];
            let returned = self.ioctl_raw(
                IOCTL_LIST_DISPLAYS,
                None,
                Some(&mut out),
                "DeviceIoControl(IOCTL_LIST_DISPLAYS)",
            )?;
            let slice = out.get(..returned).ok_or(ProtoError::BufferSizeMismatch {
                expected: out.len(),
                actual: returned,
            })?;
            Ok(ListDisplaysResponse::from_bytes(slice)?)
        }

        /// Requests a runtime render GPU adapter switch via `IOCTL_SET_RENDER_ADAPTER`.
        pub fn set_render_adapter(
            &self,
            request: &SetRenderAdapterRequest,
        ) -> Result<(), ClientError> {
            request.validate()?;
            let in_bytes = request.to_bytes();
            let _ = self.ioctl_raw(
                IOCTL_SET_RENDER_ADAPTER,
                Some(&in_bytes),
                None,
                "DeviceIoControl(IOCTL_SET_RENDER_ADAPTER)",
            )?;
            Ok(())
        }

        /// Extends the lease expiration TTL on a leased virtual display via `IOCTL_RENEW_LEASE`.
        pub fn renew_lease(&self, request: &RenewLeaseRequest) -> Result<(), ClientError> {
            request.validate()?;
            let in_bytes = request.to_bytes();
            let _ = self.ioctl_raw(
                IOCTL_RENEW_LEASE,
                Some(&in_bytes),
                None,
                "DeviceIoControl(IOCTL_RENEW_LEASE)",
            )?;
            Ok(())
        }

        /// Queries current driver adapter health and counters via `IOCTL_GET_STATUS`.
        pub fn get_status(&self) -> Result<DriverStatusResponse, ClientError> {
            let mut out = [0u8; DriverStatusResponse::WIRE_SIZE];
            let returned = self.ioctl_raw(
                IOCTL_GET_STATUS,
                None,
                Some(&mut out),
                "DeviceIoControl(IOCTL_GET_STATUS)",
            )?;
            let slice = out.get(..returned).ok_or(ProtoError::BufferSizeMismatch {
                expected: out.len(),
                actual: returned,
            })?;
            Ok(DriverStatusResponse::from_bytes(slice)?)
        }

        /// Updates the supported target mode list on an active virtual display in place.
        pub fn update_display_mode(
            &self,
            request: &UpdateDisplayModeRequest,
        ) -> Result<UpdateDisplayModeResponse, ClientError> {
            request.validate()?;
            let in_bytes = request.to_bytes();
            let mut out = [0u8; UpdateDisplayModeResponse::WIRE_SIZE];
            let returned = self.ioctl_raw(
                IOCTL_UPDATE_DISPLAY_MODE,
                Some(&in_bytes),
                Some(&mut out),
                "DeviceIoControl(IOCTL_UPDATE_DISPLAY_MODE)",
            )?;
            let slice = out.get(..returned).ok_or(ProtoError::BufferSizeMismatch {
                expected: out.len(),
                actual: returned,
            })?;
            Ok(UpdateDisplayModeResponse::from_bytes(slice)?)
        }
    }

    impl ControlClient for ControlDevice {
        fn get_version(&self) -> Result<ProtocolVersion, ClientError> {
            Self::get_version(self)
        }

        fn add_display(
            &self,
            request: &AddDisplayRequest,
        ) -> Result<AddDisplayResponse, ClientError> {
            Self::add_display(self, request)
        }

        fn remove_display(&self, request: &RemoveDisplayRequest) -> Result<(), ClientError> {
            Self::remove_display(self, request)
        }

        fn list_displays(&self) -> Result<ListDisplaysResponse, ClientError> {
            Self::list_displays(self)
        }

        fn set_render_adapter(&self, request: &SetRenderAdapterRequest) -> Result<(), ClientError> {
            Self::set_render_adapter(self, request)
        }

        fn renew_lease(&self, request: &RenewLeaseRequest) -> Result<(), ClientError> {
            Self::renew_lease(self, request)
        }

        fn get_status(&self) -> Result<DriverStatusResponse, ClientError> {
            Self::get_status(self)
        }

        fn update_display_mode(
            &self,
            request: &UpdateDisplayModeRequest,
        ) -> Result<UpdateDisplayModeResponse, ClientError> {
            Self::update_display_mode(self, request)
        }
    }
}

#[cfg(windows)]
pub use win32::{enumerate_control_device_interfaces, ControlDevice};
