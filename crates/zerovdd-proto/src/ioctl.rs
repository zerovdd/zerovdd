//! Control device paths, interface GUID, protocol version constants, and IOCTL codes.

/// Win32 user-mode path to the `zerovdd` WDF Control Device.
pub const CONTROL_DEVICE_DOS_PATH: &str = r"\\.\ZeroVddControl";

/// Kernel/NT device object path for the `zerovdd` WDF Control Device.
pub const CONTROL_DEVICE_NT_PATH: &str = r"\Device\ZeroVddControl";

/// Symbolic link path exposed by the WDF Control Device.
pub const CONTROL_DEVICE_SYMBOLIC_LINK: &str = r"\DosDevices\ZeroVddControl";

/// String representation of the `zerovdd` Control Device interface GUID.
pub const ZEROVDD_CONTROL_INTERFACE_GUID_STR: &str = "{2963AE53-4CF3-4385-929A-A8348595217E}";

/// Raw Windows `GUID` layout compatible with `#[repr(C)]` device interface registration.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterfaceGuid {
    /// First 32-bit group of the GUID.
    pub data1: u32,
    /// Second 16-bit group of the GUID.
    pub data2: u16,
    /// Third 16-bit group of the GUID.
    pub data3: u16,
    /// Final 8-byte node/clock sequence group of the GUID.
    pub data4: [u8; 8],
}

/// Device interface GUID for opening the `zerovdd` Control Device via SetupAPI/CfgMgr32.
pub const ZEROVDD_CONTROL_INTERFACE_GUID: InterfaceGuid = InterfaceGuid {
    data1: 0x2963_AE53,
    data2: 0x4CF3,
    data3: 0x4385,
    data4: [0x92, 0x9A, 0xA8, 0x34, 0x85, 0x95, 0x21, 0x7E],
};

/// Current binary IOCTL protocol major version.
pub const PROTOCOL_MAJOR: u16 = 1;

/// Current binary IOCTL protocol minor version.
pub const PROTOCOL_MINOR: u16 = 1;

/// Current binary IOCTL protocol patch version.
pub const PROTOCOL_PATCH: u16 = 0;

/// Protocol capability flag: no extra flags set.
pub const PROTOCOL_FLAG_NONE: u16 = 0;

/// Protocol capability flag: HDR10 mastering metadata supported.
pub const PROTOCOL_FLAG_HDR10_METADATA: u16 = 1 << 0;

/// Protocol capability flag: leased monitor ownership supported.
pub const PROTOCOL_FLAG_LEASED_OWNERSHIP: u16 = 1 << 1;

/// Protocol capability flag: in-place monitor mode update supported.
pub const PROTOCOL_FLAG_UPDATE_MODES: u16 = 1 << 2;

/// Protocol capability flag: explicit color space, bit-depth cap, and `target_ppi` supported.
pub const PROTOCOL_FLAG_COLOR_AND_PPI: u16 = 1 << 3;

/// Windows `FILE_DEVICE_UNKNOWN` constant (`0x0022`).
pub const FILE_DEVICE_UNKNOWN: u32 = 0x0022;

/// Windows `METHOD_BUFFERED` transfer type (`0`).
pub const METHOD_BUFFERED: u32 = 0;

/// Windows `FILE_ANY_ACCESS` access mask (`0`).
pub const FILE_ANY_ACCESS: u32 = 0;

/// Windows `FILE_READ_ACCESS` access mask (`0x0001`).
pub const FILE_READ_ACCESS: u32 = 0x0001;

/// Windows `FILE_WRITE_ACCESS` access mask (`0x0002`).
pub const FILE_WRITE_ACCESS: u32 = 0x0002;

/// Combined Windows `FILE_READ_ACCESS | FILE_WRITE_ACCESS` access mask (`0x0003`).
pub const FILE_READ_WRITE_ACCESS: u32 = FILE_READ_ACCESS | FILE_WRITE_ACCESS;

/// Computes a Windows `CTL_CODE` IOCTL control code.
///
/// Matches the Win32 `CTL_CODE(DeviceType, Function, Method, Access)` macro layout.
pub const fn ctl_code(device_type: u32, function: u32, method: u32, access: u32) -> u32 {
    ((device_type & 0xFFFF) << 16)
        | ((access & 0x3) << 14)
        | ((function & 0x0FFF) << 2)
        | (method & 0x3)
}

/// Extracts the 16-bit device type field from a Windows IOCTL control code.
pub const fn ioctl_device_type(code: u32) -> u16 {
    ((code >> 16) & 0xFFFF) as u16
}

/// Extracts the 12-bit function code field from a Windows IOCTL control code.
pub const fn ioctl_function(code: u32) -> u16 {
    ((code >> 2) & 0x0FFF) as u16
}

/// Extracts the 2-bit transfer method field from a Windows IOCTL control code.
pub const fn ioctl_method(code: u32) -> u8 {
    (code & 0x3) as u8
}

/// Extracts the 2-bit required access field from a Windows IOCTL control code.
pub const fn ioctl_access(code: u32) -> u8 {
    ((code >> 14) & 0x3) as u8
}

/// IOCTL code to query the driver's supported [`crate::types::ProtocolVersion`].
pub const IOCTL_GET_VERSION: u32 = ctl_code(
    FILE_DEVICE_UNKNOWN,
    0x800,
    METHOD_BUFFERED,
    FILE_READ_ACCESS,
);

/// IOCTL code to create or idempotently attach a virtual display.
pub const IOCTL_ADD_DISPLAY: u32 = ctl_code(
    FILE_DEVICE_UNKNOWN,
    0x801,
    METHOD_BUFFERED,
    FILE_READ_WRITE_ACCESS,
);

/// IOCTL code to depart and remove an active virtual display.
pub const IOCTL_REMOVE_DISPLAY: u32 = ctl_code(
    FILE_DEVICE_UNKNOWN,
    0x802,
    METHOD_BUFFERED,
    FILE_READ_WRITE_ACCESS,
);

/// IOCTL code to enumerate currently active virtual displays.
pub const IOCTL_LIST_DISPLAYS: u32 = ctl_code(
    FILE_DEVICE_UNKNOWN,
    0x803,
    METHOD_BUFFERED,
    FILE_READ_ACCESS,
);

/// IOCTL code to request a runtime render GPU adapter LUID change.
pub const IOCTL_SET_RENDER_ADAPTER: u32 = ctl_code(
    FILE_DEVICE_UNKNOWN,
    0x804,
    METHOD_BUFFERED,
    FILE_READ_WRITE_ACCESS,
);

/// IOCTL code to renew the expiration lease on a leased virtual display.
pub const IOCTL_RENEW_LEASE: u32 = ctl_code(
    FILE_DEVICE_UNKNOWN,
    0x805,
    METHOD_BUFFERED,
    FILE_READ_WRITE_ACCESS,
);

/// IOCTL code to query current driver health and adapter telemetry.
pub const IOCTL_GET_STATUS: u32 = ctl_code(
    FILE_DEVICE_UNKNOWN,
    0x806,
    METHOD_BUFFERED,
    FILE_READ_ACCESS,
);

/// IOCTL code to update the supported target mode list on an active virtual display in place.
pub const IOCTL_UPDATE_DISPLAY_MODE: u32 = ctl_code(
    FILE_DEVICE_UNKNOWN,
    0x807,
    METHOD_BUFFERED,
    FILE_READ_WRITE_ACCESS,
);
