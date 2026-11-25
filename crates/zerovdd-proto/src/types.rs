//! Fixed-layout `#[repr(C)]` IOCTL wire types and safe little-endian codecs.
//!
//! All wire structs intentionally avoid `bool` fields and implicit padding bytes,
//! and use explicit length fields alongside fixed arrays instead of NUL-terminated
//! `strlen` assumptions.

use crate::ioctl::{
    PROTOCOL_FLAG_COLOR_AND_PPI, PROTOCOL_FLAG_HDR10_METADATA, PROTOCOL_FLAG_LEASED_OWNERSHIP,
    PROTOCOL_FLAG_UPDATE_MODES, PROTOCOL_MAJOR, PROTOCOL_MINOR, PROTOCOL_PATCH,
};
use crate::validation::{
    validate_bounded_printable_ascii, validate_bounded_utf8_no_ctrl, ProtoError, Validate,
};

/// Maximum inline byte capacity for a client display name.
pub const MAX_CLIENT_NAME_LEN: usize = 64;

/// Maximum inline byte capacity for a monitor serial identifier.
pub const MAX_SERIAL_LEN: usize = 16;

/// Maximum inline byte capacity for a custom EDID blob (up to four 128-byte blocks).
pub const MAX_CUSTOM_EDID_LEN: usize = 512;

/// Maximum number of display entries returned in a single [`ListDisplaysResponse`].
pub const MAX_LIST_DISPLAYS: usize = 16;

/// Flag on [`RemoveDisplayRequest`] indicating removal should match `target_id`.
pub const REMOVE_FLAG_BY_TARGET_ID: u32 = 1 << 0;

/// Flag on [`UpdateDisplayModeRequest`] indicating update should match `target_id`.
pub const UPDATE_MODE_FLAG_BY_TARGET_ID: u32 = 1 << 0;

struct ByteWriter<'a> {
    buf: &'a mut [u8],
    offset: usize,
}

impl<'a> ByteWriter<'a> {
    fn new(buf: &'a mut [u8], expected_size: usize) -> Result<Self, ProtoError> {
        if buf.len() != expected_size {
            return Err(ProtoError::BufferSizeMismatch {
                expected: expected_size,
                actual: buf.len(),
            });
        }
        Ok(Self { buf, offset: 0 })
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        let end = self.offset + bytes.len();
        self.buf[self.offset..end].copy_from_slice(bytes);
        self.offset = end;
    }

    fn write_u8(&mut self, value: u8) {
        self.buf[self.offset] = value;
        self.offset += 1;
    }

    fn write_u16(&mut self, value: u16) {
        self.write_bytes(&value.to_le_bytes());
    }

    fn write_u32(&mut self, value: u32) {
        self.write_bytes(&value.to_le_bytes());
    }

    fn write_i32(&mut self, value: i32) {
        self.write_bytes(&value.to_le_bytes());
    }

    fn write_u64(&mut self, value: u64) {
        self.write_bytes(&value.to_le_bytes());
    }
}

struct ByteReader<'a> {
    buf: &'a [u8],
    offset: usize,
}

impl<'a> ByteReader<'a> {
    fn new(buf: &'a [u8], expected_size: usize) -> Result<Self, ProtoError> {
        if buf.len() < expected_size {
            return Err(ProtoError::BufferTooShort {
                expected: expected_size,
                actual: buf.len(),
            });
        }
        if buf.len() != expected_size {
            return Err(ProtoError::BufferSizeMismatch {
                expected: expected_size,
                actual: buf.len(),
            });
        }
        Ok(Self { buf, offset: 0 })
    }

    fn read_array<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0u8; N];
        let end = self.offset + N;
        out.copy_from_slice(&self.buf[self.offset..end]);
        self.offset = end;
        out
    }

    fn read_u8(&mut self) -> u8 {
        let v = self.buf[self.offset];
        self.offset += 1;
        v
    }

    fn read_u16(&mut self) -> u16 {
        u16::from_le_bytes(self.read_array::<2>())
    }

    fn read_u32(&mut self) -> u32 {
        u32::from_le_bytes(self.read_array::<4>())
    }

    fn read_i32(&mut self) -> i32 {
        i32::from_le_bytes(self.read_array::<4>())
    }

    fn read_u64(&mut self) -> u64 {
        u64::from_le_bytes(self.read_array::<8>())
    }
}

/// Wire protocol version exchanged in `IOCTL_GET_VERSION` and request headers.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolVersion {
    /// Incompatible ABI major version.
    pub major: u16,
    /// Backwards-compatible minor version.
    pub minor: u16,
    /// Patch revision.
    pub patch: u16,
    /// Bitmask of protocol capability flags.
    pub flags: u16,
}

impl ProtocolVersion {
    /// Exact serialized wire size in bytes.
    pub const WIRE_SIZE: usize = 8;

    /// Current protocol version supported by this crate.
    pub const CURRENT: Self = Self {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
        patch: PROTOCOL_PATCH,
        flags: PROTOCOL_FLAG_HDR10_METADATA
            | PROTOCOL_FLAG_LEASED_OWNERSHIP
            | PROTOCOL_FLAG_UPDATE_MODES
            | PROTOCOL_FLAG_COLOR_AND_PPI,
    };

    /// Constructs a [`ProtocolVersion`] from explicit version numbers and capability flags.
    pub const fn new(major: u16, minor: u16, patch: u16, flags: u16) -> Self {
        Self {
            major,
            minor,
            patch,
            flags,
        }
    }

    /// Encodes this version struct into a fixed-size little-endian byte array.
    pub fn to_bytes(&self) -> [u8; Self::WIRE_SIZE] {
        let mut out = [0u8; Self::WIRE_SIZE];
        // WHY: Buffer length equals WIRE_SIZE statically, so write_to cannot fail.
        let _ = self.write_to(&mut out);
        out
    }

    /// Writes this version struct in little-endian wire order into `buf`.
    pub fn write_to(&self, buf: &mut [u8]) -> Result<usize, ProtoError> {
        let mut w = ByteWriter::new(buf, Self::WIRE_SIZE)?;
        self.write_fields(&mut w);
        Ok(Self::WIRE_SIZE)
    }

    /// Decodes a [`ProtocolVersion`] from an exact-length little-endian wire buffer.
    pub fn from_bytes(buf: &[u8]) -> Result<Self, ProtoError> {
        let mut r = ByteReader::new(buf, Self::WIRE_SIZE)?;
        Ok(Self::read_fields(&mut r))
    }

    fn write_fields(&self, w: &mut ByteWriter<'_>) {
        w.write_u16(self.major);
        w.write_u16(self.minor);
        w.write_u16(self.patch);
        w.write_u16(self.flags);
    }

    fn read_fields(r: &mut ByteReader<'_>) -> Self {
        Self {
            major: r.read_u16(),
            minor: r.read_u16(),
            patch: r.read_u16(),
            flags: r.read_u16(),
        }
    }
}

impl Default for ProtocolVersion {
    fn default() -> Self {
        Self::CURRENT
    }
}

/// Lifetime ownership model for a virtual display.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnershipMode {
    /// Display stays alive only while the creating Control Device file handle remains open.
    HandleBound = 0,
    /// Display persists across handle close until its lease TTL expires or it is removed.
    Leased = 1,
}

impl OwnershipMode {
    /// Returns the wire `u8` discriminant for this ownership mode.
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for OwnershipMode {
    type Error = ProtoError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::HandleBound),
            1 => Ok(Self::Leased),
            other => Err(ProtoError::InvalidOwnershipMode(other)),
        }
    }
}

/// Color/HDR capability profile for a virtual display.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrProfile {
    /// Standard Dynamic Range (8-bit / 10-bit SDR, BT.709).
    Sdr = 0,
    /// High Dynamic Range (10-bit / 12-bit HDR10, SMPTE ST 2084 PQ, BT.2020).
    Hdr10 = 1,
}

impl HdrProfile {
    /// Returns the wire `u8` discriminant for this HDR profile.
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for HdrProfile {
    type Error = ProtoError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Sdr),
            1 => Ok(Self::Hdr10),
            other => Err(ProtoError::InvalidHdrProfile(other)),
        }
    }
}

/// Explicit color gamut preference for EDID synthesis and driver capabilities.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorSpacePreference {
    /// Automatically select BT.709 for SDR or BT.2020 for HDR10.
    #[default]
    Auto = 0,
    /// Explicit ITU-R BT.709 / sRGB primaries (valid for SDR).
    Bt709 = 1,
    /// Explicit ITU-R BT.2020 wide color gamut primaries (valid for HDR10).
    Bt2020 = 2,
    /// Explicit Display P3 / DCI-P3 primaries (valid for HDR10).
    DciP3 = 3,
}

impl ColorSpacePreference {
    /// Returns the wire `u8` discriminant for this color space preference.
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for ColorSpacePreference {
    type Error = ProtoError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Auto),
            1 => Ok(Self::Bt709),
            2 => Ok(Self::Bt2020),
            3 => Ok(Self::DciP3),
            other => Err(ProtoError::InvalidColorSpace(other)),
        }
    }
}

/// Explicit color bit-depth cap per color component (`bpc`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BitDepthPreference {
    /// Automatic defaults (`8|10` bpc for SDR, `10|12` bpc for HDR10).
    #[default]
    Auto = 0,
    /// Restrict output modes to 8 bpc (valid for SDR).
    Bpc8 = 1,
    /// Restrict output modes to 10 bpc.
    Bpc10 = 2,
    /// Restrict output modes to 12 bpc.
    Bpc12 = 3,
}

impl BitDepthPreference {
    /// Returns the wire `u8` discriminant for this bit-depth preference.
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for BitDepthPreference {
    type Error = ProtoError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Auto),
            1 => Ok(Self::Bpc8),
            2 => Ok(Self::Bpc10),
            3 => Ok(Self::Bpc12),
            other => Err(ProtoError::InvalidBitDepth(other)),
        }
    }
}

/// Optional HDR10 static mastering luminance metadata advertised in CTA-861-G.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HdrMasteringMetadata {
    /// Maximum content / mastering luminance in nits (`cd/m^2`); `0` selects profile defaults.
    pub max_luminance_nits: u16,
    /// Maximum Frame-Average Light Level in nits; `0` selects profile defaults.
    pub max_fall_nits: u16,
    /// Minimum mastering luminance in ten-thousandths of a nit (e.g. `500` = `0.05` nits).
    pub min_luminance_ten_thousandths: u32,
}

impl HdrMasteringMetadata {
    /// Exact serialized wire size in bytes.
    pub const WIRE_SIZE: usize = 8;

    /// Standard HDR10 mastering defaults (1000 nits peak, 400 nits MaxFALL, 0.05 nits min).
    pub const DEFAULT_HDR10: Self = Self {
        max_luminance_nits: 1000,
        max_fall_nits: 400,
        min_luminance_ten_thousandths: 500,
    };

    fn write_fields(&self, w: &mut ByteWriter<'_>) {
        w.write_u16(self.max_luminance_nits);
        w.write_u16(self.max_fall_nits);
        w.write_u32(self.min_luminance_ten_thousandths);
    }

    fn read_fields(r: &mut ByteReader<'_>) -> Self {
        Self {
            max_luminance_nits: r.read_u16(),
            max_fall_nits: r.read_u16(),
            min_luminance_ten_thousandths: r.read_u32(),
        }
    }
}

/// Request payload for `IOCTL_ADD_DISPLAY`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddDisplayRequest {
    /// Preferred active width in pixels.
    pub width: u32,
    /// Preferred active height in pixels.
    pub height: u32,
    /// Preferred rational refresh rate numerator (e.g. `60000` for `59.94 Hz`).
    pub refresh_num: u32,
    /// Preferred rational refresh rate denominator (e.g. `1001` for `59.94 Hz`).
    pub refresh_den: u32,
    /// Requested lease duration in seconds when `ownership_mode == Leased`.
    pub lease_ttl_secs: u32,
    /// Optional HDR10 mastering luminance metadata.
    pub hdr_metadata: HdrMasteringMetadata,
    /// Caller protocol version header.
    pub protocol_version: ProtocolVersion,
    /// Byte length of valid custom EDID data in `custom_edid` (`0` for synthesized EDID).
    pub custom_edid_len: u16,
    /// Reserved request flags (must be `0` unless defined by a minor protocol flag).
    pub flags: u16,
    /// Explicit pixel density target in PPI (`0` = default 100 PPI, else `48..=600`).
    pub target_ppi: u16,
    /// Wire [`ColorSpacePreference`] discriminant byte.
    pub color_space: u8,
    /// Wire [`BitDepthPreference`] discriminant byte.
    pub bit_depth: u8,
    /// Stable 16-byte monitor container UUID for idempotent arrival and EDID identity.
    pub monitor_guid: [u8; 16],
    /// Wire [`HdrProfile`] discriminant byte.
    pub hdr_profile: u8,
    /// Wire [`OwnershipMode`] discriminant byte.
    pub ownership_mode: u8,
    /// Valid byte length of `client_name` (`0..=64`).
    pub client_name_len: u8,
    /// Valid byte length of `serial` (`0..=16`).
    pub serial_len: u8,
    /// Fixed UTF-8 client name buffer (bounded by `client_name_len`, never NUL-scanned).
    pub client_name: [u8; MAX_CLIENT_NAME_LEN],
    /// Fixed ASCII serial buffer (bounded by `serial_len`, never NUL-scanned).
    pub serial: [u8; MAX_SERIAL_LEN],
    /// Optional raw custom EDID blocks (bounded by `custom_edid_len`).
    pub custom_edid: [u8; MAX_CUSTOM_EDID_LEN],
}

impl AddDisplayRequest {
    /// Exact serialized wire size in bytes.
    pub const WIRE_SIZE: usize = 656;

    /// Constructs a handle-bound SDR display request with sensible defaults.
    pub fn new(
        monitor_guid: [u8; 16],
        width: u32,
        height: u32,
        refresh_num: u32,
        refresh_den: u32,
        hdr_profile: HdrProfile,
        ownership_mode: OwnershipMode,
    ) -> Self {
        Self {
            width,
            height,
            refresh_num,
            refresh_den,
            lease_ttl_secs: if matches!(ownership_mode, OwnershipMode::Leased) {
                60
            } else {
                0
            },
            hdr_metadata: if matches!(hdr_profile, HdrProfile::Hdr10) {
                HdrMasteringMetadata::DEFAULT_HDR10
            } else {
                HdrMasteringMetadata::default()
            },
            protocol_version: ProtocolVersion::CURRENT,
            custom_edid_len: 0,
            flags: 0,
            target_ppi: 0,
            color_space: ColorSpacePreference::Auto.as_u8(),
            bit_depth: BitDepthPreference::Auto.as_u8(),
            monitor_guid,
            hdr_profile: hdr_profile.as_u8(),
            ownership_mode: ownership_mode.as_u8(),
            client_name_len: 0,
            serial_len: 0,
            client_name: [0; MAX_CLIENT_NAME_LEN],
            serial: [0; MAX_SERIAL_LEN],
            custom_edid: [0; MAX_CUSTOM_EDID_LEN],
        }
    }

    /// Sets `client_name` and `client_name_len` safely without NUL-termination assumptions.
    pub fn set_client_name(&mut self, name: &str) -> Result<(), ProtoError> {
        let bytes = name.as_bytes();
        let _ =
            validate_bounded_utf8_no_ctrl("client_name", bytes, bytes.len(), MAX_CLIENT_NAME_LEN)?;
        self.client_name.fill(0);
        self.client_name[..bytes.len()].copy_from_slice(bytes);
        // WHY: Length was already checked <= MAX_CLIENT_NAME_LEN (64), so cast to u8 cannot truncate.
        self.client_name_len = bytes.len() as u8;
        Ok(())
    }

    /// Returns the validated `client_name` slice bounded strictly by `client_name_len`.
    pub fn client_name_str(&self) -> Result<&str, ProtoError> {
        validate_bounded_utf8_no_ctrl(
            "client_name",
            &self.client_name,
            usize::from(self.client_name_len),
            MAX_CLIENT_NAME_LEN,
        )
    }

    /// Sets `serial` and `serial_len` safely after validating printable 7-bit ASCII.
    pub fn set_serial(&mut self, serial: &str) -> Result<(), ProtoError> {
        let bytes = serial.as_bytes();
        let _ = validate_bounded_printable_ascii("serial", bytes, bytes.len(), MAX_SERIAL_LEN)?;
        self.serial.fill(0);
        self.serial[..bytes.len()].copy_from_slice(bytes);
        // WHY: Length was already checked <= MAX_SERIAL_LEN (16), so cast to u8 cannot truncate.
        self.serial_len = bytes.len() as u8;
        Ok(())
    }

    /// Returns the validated `serial` slice bounded strictly by `serial_len`.
    pub fn serial_str(&self) -> Result<&str, ProtoError> {
        validate_bounded_printable_ascii(
            "serial",
            &self.serial,
            usize::from(self.serial_len),
            MAX_SERIAL_LEN,
        )
    }

    /// Copies a custom EDID payload into `custom_edid` and updates `custom_edid_len`.
    pub fn set_custom_edid(&mut self, edid: &[u8]) -> Result<(), ProtoError> {
        if edid.len() > MAX_CUSTOM_EDID_LEN {
            return Err(ProtoError::FieldLengthOutOfBounds {
                field: "custom_edid",
                len: edid.len(),
                max: MAX_CUSTOM_EDID_LEN,
            });
        }
        if !edid.is_empty() && (edid.len() < 128 || !edid.len().is_multiple_of(128)) {
            return Err(ProtoError::InvalidCustomEdidLength(edid.len()));
        }
        self.custom_edid.fill(0);
        self.custom_edid[..edid.len()].copy_from_slice(edid);
        // WHY: Length was already checked <= MAX_CUSTOM_EDID_LEN (512), so cast to u16 cannot truncate.
        self.custom_edid_len = edid.len() as u16;
        Ok(())
    }

    /// Returns the active slice of `custom_edid` bounded by `custom_edid_len`.
    pub fn custom_edid_slice(&self) -> Result<&[u8], ProtoError> {
        let len = usize::from(self.custom_edid_len);
        if len > MAX_CUSTOM_EDID_LEN {
            return Err(ProtoError::FieldLengthOutOfBounds {
                field: "custom_edid",
                len,
                max: MAX_CUSTOM_EDID_LEN,
            });
        }
        if len != 0 && (len < 128 || !len.is_multiple_of(128)) {
            return Err(ProtoError::InvalidCustomEdidLength(len));
        }
        Ok(&self.custom_edid[..len])
    }

    /// Computes the integer millihertz refresh rate using 64-bit arithmetic, or `0` if invalid.
    pub fn refresh_mhz(&self) -> u32 {
        if self.refresh_den == 0 {
            return 0;
        }
        let mhz = (u64::from(self.refresh_num) * 1000) / u64::from(self.refresh_den);
        u32::try_from(mhz).unwrap_or(u32::MAX)
    }

    /// Encodes this request into a fixed-size little-endian wire array.
    pub fn to_bytes(&self) -> [u8; Self::WIRE_SIZE] {
        let mut out = [0u8; Self::WIRE_SIZE];
        // WHY: Buffer length equals WIRE_SIZE statically, so write_to cannot fail.
        let _ = self.write_to(&mut out);
        out
    }

    /// Writes this request in little-endian wire order into `buf`.
    pub fn write_to(&self, buf: &mut [u8]) -> Result<usize, ProtoError> {
        let mut w = ByteWriter::new(buf, Self::WIRE_SIZE)?;
        w.write_u32(self.width);
        w.write_u32(self.height);
        w.write_u32(self.refresh_num);
        w.write_u32(self.refresh_den);
        w.write_u32(self.lease_ttl_secs);
        self.hdr_metadata.write_fields(&mut w);
        self.protocol_version.write_fields(&mut w);
        w.write_u16(self.custom_edid_len);
        w.write_u16(self.flags);
        w.write_u16(self.target_ppi);
        w.write_u8(self.color_space);
        w.write_u8(self.bit_depth);
        w.write_bytes(&self.monitor_guid);
        w.write_u8(self.hdr_profile);
        w.write_u8(self.ownership_mode);
        w.write_u8(self.client_name_len);
        w.write_u8(self.serial_len);
        w.write_bytes(&self.client_name);
        w.write_bytes(&self.serial);
        w.write_bytes(&self.custom_edid);
        Ok(Self::WIRE_SIZE)
    }

    /// Decodes an [`AddDisplayRequest`] from an exact-length wire buffer and validates it.
    pub fn from_bytes(buf: &[u8]) -> Result<Self, ProtoError> {
        let mut r = ByteReader::new(buf, Self::WIRE_SIZE)?;
        let req = Self {
            width: r.read_u32(),
            height: r.read_u32(),
            refresh_num: r.read_u32(),
            refresh_den: r.read_u32(),
            lease_ttl_secs: r.read_u32(),
            hdr_metadata: HdrMasteringMetadata::read_fields(&mut r),
            protocol_version: ProtocolVersion::read_fields(&mut r),
            custom_edid_len: r.read_u16(),
            flags: r.read_u16(),
            target_ppi: r.read_u16(),
            color_space: r.read_u8(),
            bit_depth: r.read_u8(),
            monitor_guid: r.read_array::<16>(),
            hdr_profile: r.read_u8(),
            ownership_mode: r.read_u8(),
            client_name_len: r.read_u8(),
            serial_len: r.read_u8(),
            client_name: r.read_array::<MAX_CLIENT_NAME_LEN>(),
            serial: r.read_array::<MAX_SERIAL_LEN>(),
            custom_edid: r.read_array::<MAX_CUSTOM_EDID_LEN>(),
        };
        req.validate()?;
        Ok(req)
    }
}

/// Response payload for `IOCTL_ADD_DISPLAY`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddDisplayResponse {
    /// Low 32 bits of the Windows `LUID` for the virtual display adapter.
    pub adapter_luid_low: u32,
    /// High 32 bits of the Windows `LUID` for the virtual display adapter.
    pub adapter_luid_high: i32,
    /// OS VidPN target identifier returned by IddCx upon monitor arrival.
    pub target_id: u32,
    /// Zero-based internal driver connector slot index.
    pub connector_index: u32,
    /// `1` if a new monitor arrived, `0` if an existing monitor was idempotently reused.
    pub created_new: u8,
    /// Explicit zero padding for 4-byte alignment.
    pub reserved: [u8; 3],
}

impl AddDisplayResponse {
    /// Exact serialized wire size in bytes.
    pub const WIRE_SIZE: usize = 20;

    /// Encodes this response into a fixed-size little-endian wire array.
    pub fn to_bytes(&self) -> [u8; Self::WIRE_SIZE] {
        let mut out = [0u8; Self::WIRE_SIZE];
        let _ = self.write_to(&mut out);
        out
    }

    /// Writes this response in little-endian wire order into `buf`.
    pub fn write_to(&self, buf: &mut [u8]) -> Result<usize, ProtoError> {
        let mut w = ByteWriter::new(buf, Self::WIRE_SIZE)?;
        w.write_u32(self.adapter_luid_low);
        w.write_i32(self.adapter_luid_high);
        w.write_u32(self.target_id);
        w.write_u32(self.connector_index);
        w.write_u8(self.created_new);
        w.write_bytes(&self.reserved);
        Ok(Self::WIRE_SIZE)
    }

    /// Decodes an [`AddDisplayResponse`] from an exact-length wire buffer and validates it.
    pub fn from_bytes(buf: &[u8]) -> Result<Self, ProtoError> {
        let mut r = ByteReader::new(buf, Self::WIRE_SIZE)?;
        let resp = Self {
            adapter_luid_low: r.read_u32(),
            adapter_luid_high: r.read_i32(),
            target_id: r.read_u32(),
            connector_index: r.read_u32(),
            created_new: r.read_u8(),
            reserved: r.read_array::<3>(),
        };
        resp.validate()?;
        Ok(resp)
    }
}

/// Request payload for `IOCTL_REMOVE_DISPLAY`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoveDisplayRequest {
    /// Caller protocol version header.
    pub protocol_version: ProtocolVersion,
    /// Monitor container GUID to depart (when `REMOVE_FLAG_BY_TARGET_ID` is unset).
    pub monitor_guid: [u8; 16],
    /// VidPN target ID to depart (when `REMOVE_FLAG_BY_TARGET_ID` is set).
    pub target_id: u32,
    /// Selector bitmask (see [`REMOVE_FLAG_BY_TARGET_ID`]).
    pub flags: u32,
}

impl RemoveDisplayRequest {
    /// Exact serialized wire size in bytes.
    pub const WIRE_SIZE: usize = 32;

    /// Constructs a removal request targeting a specific `monitor_guid`.
    pub const fn by_guid(monitor_guid: [u8; 16]) -> Self {
        Self {
            protocol_version: ProtocolVersion::CURRENT,
            monitor_guid,
            target_id: 0,
            flags: 0,
        }
    }

    /// Constructs a removal request targeting a specific VidPN `target_id`.
    pub const fn by_target_id(target_id: u32) -> Self {
        Self {
            protocol_version: ProtocolVersion::CURRENT,
            monitor_guid: [0; 16],
            target_id,
            flags: REMOVE_FLAG_BY_TARGET_ID,
        }
    }

    /// Encodes this request into a fixed-size little-endian wire array.
    pub fn to_bytes(&self) -> [u8; Self::WIRE_SIZE] {
        let mut out = [0u8; Self::WIRE_SIZE];
        let _ = self.write_to(&mut out);
        out
    }

    /// Writes this request in little-endian wire order into `buf`.
    pub fn write_to(&self, buf: &mut [u8]) -> Result<usize, ProtoError> {
        let mut w = ByteWriter::new(buf, Self::WIRE_SIZE)?;
        self.protocol_version.write_fields(&mut w);
        w.write_bytes(&self.monitor_guid);
        w.write_u32(self.target_id);
        w.write_u32(self.flags);
        Ok(Self::WIRE_SIZE)
    }

    /// Decodes a [`RemoveDisplayRequest`] from an exact-length wire buffer and validates it.
    pub fn from_bytes(buf: &[u8]) -> Result<Self, ProtoError> {
        let mut r = ByteReader::new(buf, Self::WIRE_SIZE)?;
        let req = Self {
            protocol_version: ProtocolVersion::read_fields(&mut r),
            monitor_guid: r.read_array::<16>(),
            target_id: r.read_u32(),
            flags: r.read_u32(),
        };
        req.validate()?;
        Ok(req)
    }
}

/// Summary descriptor for an active virtual display in [`ListDisplaysResponse`].
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayInfo {
    /// Monitor container GUID.
    pub monitor_guid: [u8; 16],
    /// Adapter LUID low part.
    pub adapter_luid_low: u32,
    /// Adapter LUID high part.
    pub adapter_luid_high: i32,
    /// OS VidPN target identifier.
    pub target_id: u32,
    /// Internal connector slot index.
    pub connector_index: u32,
    /// Preferred width in pixels.
    pub width: u32,
    /// Preferred height in pixels.
    pub height: u32,
    /// Preferred rational refresh numerator.
    pub refresh_num: u32,
    /// Preferred rational refresh denominator.
    pub refresh_den: u32,
    /// Remaining lease seconds (`0` for handle-bound displays).
    pub lease_remaining_secs: u32,
    /// Wire [`HdrProfile`] discriminant byte.
    pub hdr_profile: u8,
    /// Wire [`OwnershipMode`] discriminant byte.
    pub ownership_mode: u8,
    /// Valid byte length of `client_name`.
    pub client_name_len: u8,
    /// `1` if arrived and active, `0` otherwise.
    pub active: u8,
    /// Fixed client name byte array (bounded by `client_name_len`).
    pub client_name: [u8; MAX_CLIENT_NAME_LEN],
}

impl DisplayInfo {
    /// Exact serialized wire size in bytes.
    pub const WIRE_SIZE: usize = 120;

    /// Returns a zeroed empty [`DisplayInfo`] slot suitable for unused array elements.
    pub const fn empty() -> Self {
        Self {
            monitor_guid: [0; 16],
            adapter_luid_low: 0,
            adapter_luid_high: 0,
            target_id: 0,
            connector_index: 0,
            width: 0,
            height: 0,
            refresh_num: 0,
            refresh_den: 0,
            lease_remaining_secs: 0,
            hdr_profile: 0,
            ownership_mode: 0,
            client_name_len: 0,
            active: 0,
            client_name: [0; MAX_CLIENT_NAME_LEN],
        }
    }

    /// Returns the validated `client_name` slice bounded strictly by `client_name_len`.
    pub fn client_name_str(&self) -> Result<&str, ProtoError> {
        validate_bounded_utf8_no_ctrl(
            "client_name",
            &self.client_name,
            usize::from(self.client_name_len),
            MAX_CLIENT_NAME_LEN,
        )
    }

    fn write_fields(&self, w: &mut ByteWriter<'_>) {
        w.write_bytes(&self.monitor_guid);
        w.write_u32(self.adapter_luid_low);
        w.write_i32(self.adapter_luid_high);
        w.write_u32(self.target_id);
        w.write_u32(self.connector_index);
        w.write_u32(self.width);
        w.write_u32(self.height);
        w.write_u32(self.refresh_num);
        w.write_u32(self.refresh_den);
        w.write_u32(self.lease_remaining_secs);
        w.write_u8(self.hdr_profile);
        w.write_u8(self.ownership_mode);
        w.write_u8(self.client_name_len);
        w.write_u8(self.active);
        w.write_bytes(&self.client_name);
    }

    fn read_fields(r: &mut ByteReader<'_>) -> Self {
        Self {
            monitor_guid: r.read_array::<16>(),
            adapter_luid_low: r.read_u32(),
            adapter_luid_high: r.read_i32(),
            target_id: r.read_u32(),
            connector_index: r.read_u32(),
            width: r.read_u32(),
            height: r.read_u32(),
            refresh_num: r.read_u32(),
            refresh_den: r.read_u32(),
            lease_remaining_secs: r.read_u32(),
            hdr_profile: r.read_u8(),
            ownership_mode: r.read_u8(),
            client_name_len: r.read_u8(),
            active: r.read_u8(),
            client_name: r.read_array::<MAX_CLIENT_NAME_LEN>(),
        }
    }
}

/// Response payload for `IOCTL_LIST_DISPLAYS`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListDisplaysResponse {
    /// Driver protocol version header.
    pub protocol_version: ProtocolVersion,
    /// Number of valid entries populated in `displays[..count]`.
    pub count: u32,
    /// Total number of active displays tracked by the driver.
    pub total_active: u32,
    /// Inline fixed array of active display descriptors.
    pub displays: [DisplayInfo; MAX_LIST_DISPLAYS],
}

impl ListDisplaysResponse {
    /// Exact serialized wire size in bytes.
    pub const WIRE_SIZE: usize = 16 + MAX_LIST_DISPLAYS * DisplayInfo::WIRE_SIZE;

    /// Constructs an empty [`ListDisplaysResponse`] stamped with [`ProtocolVersion::CURRENT`].
    pub const fn empty() -> Self {
        Self {
            protocol_version: ProtocolVersion::CURRENT,
            count: 0,
            total_active: 0,
            displays: [DisplayInfo::empty(); MAX_LIST_DISPLAYS],
        }
    }

    /// Returns the valid slice of active display entries (`displays[..count]`).
    pub fn entries(&self) -> Result<&[DisplayInfo], ProtoError> {
        let count = self.count as usize;
        if count > MAX_LIST_DISPLAYS {
            return Err(ProtoError::ListCountOutOfBounds {
                count: self.count,
                max: MAX_LIST_DISPLAYS,
            });
        }
        Ok(&self.displays[..count])
    }

    /// Encodes this response into a fixed-size little-endian wire array.
    pub fn to_bytes(&self) -> [u8; Self::WIRE_SIZE] {
        let mut out = [0u8; Self::WIRE_SIZE];
        let _ = self.write_to(&mut out);
        out
    }

    /// Writes this response in little-endian wire order into `buf`.
    pub fn write_to(&self, buf: &mut [u8]) -> Result<usize, ProtoError> {
        let mut w = ByteWriter::new(buf, Self::WIRE_SIZE)?;
        self.protocol_version.write_fields(&mut w);
        w.write_u32(self.count);
        w.write_u32(self.total_active);
        for info in &self.displays {
            info.write_fields(&mut w);
        }
        Ok(Self::WIRE_SIZE)
    }

    /// Decodes a [`ListDisplaysResponse`] from an exact-length wire buffer and validates it.
    pub fn from_bytes(buf: &[u8]) -> Result<Self, ProtoError> {
        let mut r = ByteReader::new(buf, Self::WIRE_SIZE)?;
        let protocol_version = ProtocolVersion::read_fields(&mut r);
        let count = r.read_u32();
        let total_active = r.read_u32();
        let mut displays = [DisplayInfo::empty(); MAX_LIST_DISPLAYS];
        for slot in &mut displays {
            *slot = DisplayInfo::read_fields(&mut r);
        }
        let resp = Self {
            protocol_version,
            count,
            total_active,
            displays,
        };
        resp.validate()?;
        Ok(resp)
    }
}

/// Request payload for `IOCTL_SET_RENDER_ADAPTER`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetRenderAdapterRequest {
    /// Caller protocol version header.
    pub protocol_version: ProtocolVersion,
    /// Target DXGI adapter LUID low part (`0` + `0` requests automatic selection).
    pub adapter_luid_low: u32,
    /// Target DXGI adapter LUID high part.
    pub adapter_luid_high: i32,
    /// Reserved flags (must be `0`).
    pub flags: u32,
}

impl SetRenderAdapterRequest {
    /// Exact serialized wire size in bytes.
    pub const WIRE_SIZE: usize = 20;

    /// Constructs a render adapter switch request for the specified `LUID`.
    pub const fn new(adapter_luid_low: u32, adapter_luid_high: i32) -> Self {
        Self {
            protocol_version: ProtocolVersion::CURRENT,
            adapter_luid_low,
            adapter_luid_high,
            flags: 0,
        }
    }

    /// Encodes this request into a fixed-size little-endian wire array.
    pub fn to_bytes(&self) -> [u8; Self::WIRE_SIZE] {
        let mut out = [0u8; Self::WIRE_SIZE];
        let _ = self.write_to(&mut out);
        out
    }

    /// Writes this request in little-endian wire order into `buf`.
    pub fn write_to(&self, buf: &mut [u8]) -> Result<usize, ProtoError> {
        let mut w = ByteWriter::new(buf, Self::WIRE_SIZE)?;
        self.protocol_version.write_fields(&mut w);
        w.write_u32(self.adapter_luid_low);
        w.write_i32(self.adapter_luid_high);
        w.write_u32(self.flags);
        Ok(Self::WIRE_SIZE)
    }

    /// Decodes a [`SetRenderAdapterRequest`] from an exact-length wire buffer and validates it.
    pub fn from_bytes(buf: &[u8]) -> Result<Self, ProtoError> {
        let mut r = ByteReader::new(buf, Self::WIRE_SIZE)?;
        let req = Self {
            protocol_version: ProtocolVersion::read_fields(&mut r),
            adapter_luid_low: r.read_u32(),
            adapter_luid_high: r.read_i32(),
            flags: r.read_u32(),
        };
        req.validate()?;
        Ok(req)
    }
}

/// Request payload for `IOCTL_RENEW_LEASE`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenewLeaseRequest {
    /// Caller protocol version header.
    pub protocol_version: ProtocolVersion,
    /// Monitor container GUID whose lease should be extended.
    pub monitor_guid: [u8; 16],
    /// New lease TTL in seconds from receipt.
    pub lease_ttl_secs: u32,
    /// Reserved flags (must be `0`).
    pub flags: u32,
}

impl RenewLeaseRequest {
    /// Exact serialized wire size in bytes.
    pub const WIRE_SIZE: usize = 32;

    /// Constructs a lease renewal request for `monitor_guid` with `lease_ttl_secs`.
    pub const fn new(monitor_guid: [u8; 16], lease_ttl_secs: u32) -> Self {
        Self {
            protocol_version: ProtocolVersion::CURRENT,
            monitor_guid,
            lease_ttl_secs,
            flags: 0,
        }
    }

    /// Encodes this request into a fixed-size little-endian wire array.
    pub fn to_bytes(&self) -> [u8; Self::WIRE_SIZE] {
        let mut out = [0u8; Self::WIRE_SIZE];
        let _ = self.write_to(&mut out);
        out
    }

    /// Writes this request in little-endian wire order into `buf`.
    pub fn write_to(&self, buf: &mut [u8]) -> Result<usize, ProtoError> {
        let mut w = ByteWriter::new(buf, Self::WIRE_SIZE)?;
        self.protocol_version.write_fields(&mut w);
        w.write_bytes(&self.monitor_guid);
        w.write_u32(self.lease_ttl_secs);
        w.write_u32(self.flags);
        Ok(Self::WIRE_SIZE)
    }

    /// Decodes a [`RenewLeaseRequest`] from an exact-length wire buffer and validates it.
    pub fn from_bytes(buf: &[u8]) -> Result<Self, ProtoError> {
        let mut r = ByteReader::new(buf, Self::WIRE_SIZE)?;
        let req = Self {
            protocol_version: ProtocolVersion::read_fields(&mut r),
            monitor_guid: r.read_array::<16>(),
            lease_ttl_secs: r.read_u32(),
            flags: r.read_u32(),
        };
        req.validate()?;
        Ok(req)
    }
}

/// Response payload for `IOCTL_GET_STATUS`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverStatusResponse {
    /// Driver protocol version header.
    pub protocol_version: ProtocolVersion,
    /// Cumulative number of monitor arrivals processed by the adapter.
    pub total_arrivals: u64,
    /// Cumulative number of monitor departures processed by the adapter.
    pub total_departures: u64,
    /// Adapter uptime in seconds since initialization.
    pub uptime_secs: u64,
    /// Active render adapter LUID low part.
    pub render_adapter_luid_low: u32,
    /// Active render adapter LUID high part.
    pub render_adapter_luid_high: i32,
    /// Current count of active virtual monitors.
    pub active_display_count: u32,
    /// Configured maximum virtual monitors supported simultaneously.
    pub max_displays: u32,
    /// `1` if the IddCx adapter is initialized and ready, `0` otherwise.
    pub adapter_ready: u8,
    /// `1` if FP16 / HDR10 pipeline capability is enabled, `0` otherwise.
    pub hdr_capable: u8,
    /// `1` if the most recent GPU render adapter switch tripped the circuit breaker.
    pub last_gpu_switch_failed: u8,
    /// Explicit zero padding for 8-byte alignment.
    pub reserved: [u8; 5],
}

impl DriverStatusResponse {
    /// Exact serialized wire size in bytes.
    pub const WIRE_SIZE: usize = 56;

    /// Encodes this status response into a fixed-size little-endian wire array.
    pub fn to_bytes(&self) -> [u8; Self::WIRE_SIZE] {
        let mut out = [0u8; Self::WIRE_SIZE];
        let _ = self.write_to(&mut out);
        out
    }

    /// Writes this status response in little-endian wire order into `buf`.
    pub fn write_to(&self, buf: &mut [u8]) -> Result<usize, ProtoError> {
        let mut w = ByteWriter::new(buf, Self::WIRE_SIZE)?;
        self.protocol_version.write_fields(&mut w);
        w.write_u64(self.total_arrivals);
        w.write_u64(self.total_departures);
        w.write_u64(self.uptime_secs);
        w.write_u32(self.render_adapter_luid_low);
        w.write_i32(self.render_adapter_luid_high);
        w.write_u32(self.active_display_count);
        w.write_u32(self.max_displays);
        w.write_u8(self.adapter_ready);
        w.write_u8(self.hdr_capable);
        w.write_u8(self.last_gpu_switch_failed);
        w.write_bytes(&self.reserved);
        Ok(Self::WIRE_SIZE)
    }

    /// Decodes a [`DriverStatusResponse`] from an exact-length wire buffer and validates it.
    pub fn from_bytes(buf: &[u8]) -> Result<Self, ProtoError> {
        let mut r = ByteReader::new(buf, Self::WIRE_SIZE)?;
        let resp = Self {
            protocol_version: ProtocolVersion::read_fields(&mut r),
            total_arrivals: r.read_u64(),
            total_departures: r.read_u64(),
            uptime_secs: r.read_u64(),
            render_adapter_luid_low: r.read_u32(),
            render_adapter_luid_high: r.read_i32(),
            active_display_count: r.read_u32(),
            max_displays: r.read_u32(),
            adapter_ready: r.read_u8(),
            hdr_capable: r.read_u8(),
            last_gpu_switch_failed: r.read_u8(),
            reserved: r.read_array::<5>(),
        };
        resp.validate()?;
        Ok(resp)
    }
}

/// Request payload for `IOCTL_UPDATE_DISPLAY_MODE`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateDisplayModeRequest {
    /// Caller protocol version header.
    pub protocol_version: ProtocolVersion,
    /// Monitor container GUID to update (when `UPDATE_MODE_FLAG_BY_TARGET_ID` is unset).
    pub monitor_guid: [u8; 16],
    /// VidPN target ID to update (when `UPDATE_MODE_FLAG_BY_TARGET_ID` is set).
    pub target_id: u32,
    /// Selector bitmask (see [`UPDATE_MODE_FLAG_BY_TARGET_ID`]).
    pub flags: u32,
    /// Requested preferred width in pixels.
    pub width: u32,
    /// Requested preferred height in lines.
    pub height: u32,
    /// Requested rational refresh rate numerator.
    pub refresh_num: u32,
    /// Requested rational refresh rate denominator.
    pub refresh_den: u32,
    /// Optional HDR10 mastering luminance metadata (`0` selects defaults).
    pub hdr_metadata: HdrMasteringMetadata,
    /// Wire [`HdrProfile`] discriminant byte.
    pub hdr_profile: u8,
    /// Wire [`ColorSpacePreference`] discriminant byte.
    pub color_space: u8,
    /// Wire [`BitDepthPreference`] discriminant byte.
    pub bit_depth: u8,
    /// Explicit zero padding for 4-byte alignment.
    pub reserved: u8,
}

impl UpdateDisplayModeRequest {
    /// Exact serialized wire size in bytes.
    pub const WIRE_SIZE: usize = 60;

    /// Constructs an in-place mode update request targeting `monitor_guid`.
    pub const fn by_guid(
        monitor_guid: [u8; 16],
        width: u32,
        height: u32,
        refresh_num: u32,
        refresh_den: u32,
        hdr_profile: HdrProfile,
    ) -> Self {
        Self {
            protocol_version: ProtocolVersion::CURRENT,
            monitor_guid,
            target_id: 0,
            flags: 0,
            width,
            height,
            refresh_num,
            refresh_den,
            hdr_metadata: HdrMasteringMetadata {
                max_luminance_nits: 0,
                max_fall_nits: 0,
                min_luminance_ten_thousandths: 0,
            },
            hdr_profile: hdr_profile.as_u8(),
            color_space: ColorSpacePreference::Auto.as_u8(),
            bit_depth: BitDepthPreference::Auto.as_u8(),
            reserved: 0,
        }
    }

    /// Constructs an in-place mode update request targeting VidPN `target_id`.
    pub const fn by_target_id(
        target_id: u32,
        width: u32,
        height: u32,
        refresh_num: u32,
        refresh_den: u32,
        hdr_profile: HdrProfile,
    ) -> Self {
        Self {
            protocol_version: ProtocolVersion::CURRENT,
            monitor_guid: [0; 16],
            target_id,
            flags: UPDATE_MODE_FLAG_BY_TARGET_ID,
            width,
            height,
            refresh_num,
            refresh_den,
            hdr_metadata: HdrMasteringMetadata {
                max_luminance_nits: 0,
                max_fall_nits: 0,
                min_luminance_ten_thousandths: 0,
            },
            hdr_profile: hdr_profile.as_u8(),
            color_space: ColorSpacePreference::Auto.as_u8(),
            bit_depth: BitDepthPreference::Auto.as_u8(),
            reserved: 0,
        }
    }

    /// Encodes this request into a fixed-size little-endian wire array.
    pub fn to_bytes(&self) -> [u8; Self::WIRE_SIZE] {
        let mut out = [0u8; Self::WIRE_SIZE];
        let _ = self.write_to(&mut out);
        out
    }

    /// Writes this request in little-endian wire order into `buf`.
    pub fn write_to(&self, buf: &mut [u8]) -> Result<usize, ProtoError> {
        let mut w = ByteWriter::new(buf, Self::WIRE_SIZE)?;
        self.protocol_version.write_fields(&mut w);
        w.write_bytes(&self.monitor_guid);
        w.write_u32(self.target_id);
        w.write_u32(self.flags);
        w.write_u32(self.width);
        w.write_u32(self.height);
        w.write_u32(self.refresh_num);
        w.write_u32(self.refresh_den);
        self.hdr_metadata.write_fields(&mut w);
        w.write_u8(self.hdr_profile);
        w.write_u8(self.color_space);
        w.write_u8(self.bit_depth);
        w.write_u8(self.reserved);
        Ok(Self::WIRE_SIZE)
    }

    /// Decodes an [`UpdateDisplayModeRequest`] from an exact-length wire buffer and validates it.
    pub fn from_bytes(buf: &[u8]) -> Result<Self, ProtoError> {
        let mut r = ByteReader::new(buf, Self::WIRE_SIZE)?;
        let req = Self {
            protocol_version: ProtocolVersion::read_fields(&mut r),
            monitor_guid: r.read_array::<16>(),
            target_id: r.read_u32(),
            flags: r.read_u32(),
            width: r.read_u32(),
            height: r.read_u32(),
            refresh_num: r.read_u32(),
            refresh_den: r.read_u32(),
            hdr_metadata: HdrMasteringMetadata::read_fields(&mut r),
            hdr_profile: r.read_u8(),
            color_space: r.read_u8(),
            bit_depth: r.read_u8(),
            reserved: r.read_u8(),
        };
        req.validate()?;
        Ok(req)
    }
}

/// Response payload for `IOCTL_UPDATE_DISPLAY_MODE`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateDisplayModeResponse {
    /// Low 32 bits of the Windows `LUID` for the virtual display adapter.
    pub adapter_luid_low: u32,
    /// High 32 bits of the Windows `LUID` for the virtual display adapter.
    pub adapter_luid_high: i32,
    /// OS VidPN target identifier of the updated monitor.
    pub target_id: u32,
    /// Zero-based internal driver connector slot index.
    pub connector_index: u32,
    /// Number of target modes published to `IddCxMonitorUpdateModes[2]`.
    pub mode_count: u32,
}

impl UpdateDisplayModeResponse {
    /// Exact serialized wire size in bytes.
    pub const WIRE_SIZE: usize = 20;

    /// Encodes this response into a fixed-size little-endian wire array.
    pub fn to_bytes(&self) -> [u8; Self::WIRE_SIZE] {
        let mut out = [0u8; Self::WIRE_SIZE];
        let _ = self.write_to(&mut out);
        out
    }

    /// Writes this response in little-endian wire order into `buf`.
    pub fn write_to(&self, buf: &mut [u8]) -> Result<usize, ProtoError> {
        let mut w = ByteWriter::new(buf, Self::WIRE_SIZE)?;
        w.write_u32(self.adapter_luid_low);
        w.write_i32(self.adapter_luid_high);
        w.write_u32(self.target_id);
        w.write_u32(self.connector_index);
        w.write_u32(self.mode_count);
        Ok(Self::WIRE_SIZE)
    }

    /// Decodes an [`UpdateDisplayModeResponse`] from an exact-length wire buffer and validates it.
    pub fn from_bytes(buf: &[u8]) -> Result<Self, ProtoError> {
        let mut r = ByteReader::new(buf, Self::WIRE_SIZE)?;
        let resp = Self {
            adapter_luid_low: r.read_u32(),
            adapter_luid_high: r.read_i32(),
            target_id: r.read_u32(),
            connector_index: r.read_u32(),
            mode_count: r.read_u32(),
        };
        resp.validate()?;
        Ok(resp)
    }
}
