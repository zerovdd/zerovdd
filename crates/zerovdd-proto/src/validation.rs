//! Strict validation rules for `zerovdd` IOCTL payloads and wire values.

use core::str;
use thiserror::Error;

use crate::ioctl::PROTOCOL_MAJOR;
use crate::types::{
    AddDisplayRequest, AddDisplayResponse, BitDepthPreference, ColorSpacePreference, DisplayInfo,
    DriverStatusResponse, HdrMasteringMetadata, HdrProfile, ListDisplaysResponse, OwnershipMode,
    ProtocolVersion, RemoveDisplayRequest, RenewLeaseRequest, SetRenderAdapterRequest,
    UpdateDisplayModeRequest, UpdateDisplayModeResponse, MAX_CLIENT_NAME_LEN, MAX_CUSTOM_EDID_LEN,
    MAX_LIST_DISPLAYS, MAX_SERIAL_LEN, REMOVE_FLAG_BY_TARGET_ID, UPDATE_MODE_FLAG_BY_TARGET_ID,
};

/// Minimum allowed virtual display width or height in pixels.
pub const MIN_DISPLAY_DIM: u32 = 320;

/// Maximum allowed virtual display width or height in pixels.
pub const MAX_DISPLAY_DIM: u32 = 16_384;

/// Minimum allowed virtual display refresh rate in hertz.
pub const MIN_REFRESH_HZ: u64 = 10;

/// Maximum allowed virtual display refresh rate in hertz.
pub const MAX_REFRESH_HZ: u64 = 1_000;

/// Maximum allowed lease TTL in seconds for leased display ownership (24 hours).
pub const MAX_LEASE_TTL_SECS: u32 = 86_400;

/// Maximum allowed HDR peak mastering luminance in nits.
pub const MAX_HDR_LUMINANCE_NITS: u16 = 10_000;

/// Minimum allowed explicit virtual display pixel density in PPI (when non-zero).
pub const MIN_TARGET_PPI: u16 = 48;

/// Maximum allowed explicit virtual display pixel density in PPI.
pub const MAX_TARGET_PPI: u16 = 600;

/// Errors returned when decoding or validating IOCTL wire payloads.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProtoError {
    /// Input buffer is smaller than the required wire struct size.
    #[error("buffer too short: expected at least {expected} bytes, got {actual}")]
    BufferTooShort {
        /// Minimum required byte length.
        expected: usize,
        /// Actual provided byte length.
        actual: usize,
    },

    /// Input or output buffer length does not match the exact expected wire size.
    #[error("buffer size mismatch: expected {expected} bytes, got {actual}")]
    BufferSizeMismatch {
        /// Exact expected byte length.
        expected: usize,
        /// Actual provided byte length.
        actual: usize,
    },

    /// Protocol major version does not match the supported ABI major version.
    #[error("unsupported protocol major version {actual}; expected {expected}")]
    UnsupportedMajorVersion {
        /// Expected major version.
        expected: u16,
        /// Received major version.
        actual: u16,
    },

    /// Display width or height is zero or outside the supported pixel range.
    #[error("invalid display dimensions {width}x{height}")]
    InvalidDimensions {
        /// Requested width in pixels.
        width: u32,
        /// Requested height in pixels.
        height: u32,
    },

    /// Refresh rate numerator or denominator is zero or outside the supported range.
    #[error("invalid rational refresh rate {num}/{den}")]
    InvalidRefreshRate {
        /// Refresh rate numerator.
        num: u32,
        /// Refresh rate denominator.
        den: u32,
    },

    /// Ownership mode discriminant byte is unrecognized.
    #[error("invalid ownership mode byte: {0}")]
    InvalidOwnershipMode(u8),

    /// HDR profile discriminant byte is unrecognized.
    #[error("invalid HDR profile byte: {0}")]
    InvalidHdrProfile(u8),

    /// Lease TTL is zero for a leased request or exceeds the maximum bound.
    #[error("invalid lease TTL {0} seconds")]
    InvalidLeaseTtl(u32),

    /// Explicit byte-length field exceeds the fixed inline array capacity.
    #[error("field `{field}` length {len} exceeds maximum capacity {max}")]
    FieldLengthOutOfBounds {
        /// Name of the bounded wire field.
        field: &'static str,
        /// Declared byte length.
        len: usize,
        /// Maximum allowed byte length.
        max: usize,
    },

    /// Byte slice in a string field is not valid UTF-8.
    #[error("field `{field}` contains invalid UTF-8")]
    InvalidUtf8 {
        /// Name of the string field.
        field: &'static str,
    },

    /// String field contains control characters or disallowed non-ASCII bytes.
    #[error("field `{field}` contains disallowed control or non-printable ASCII characters")]
    NonPrintableAscii {
        /// Name of the string field.
        field: &'static str,
    },

    /// Custom EDID payload length is non-zero and invalid (not a 128-byte block multiple).
    #[error("invalid custom EDID length {0}: must be 0 or a 128-byte multiple up to {MAX_CUSTOM_EDID_LEN}")]
    InvalidCustomEdidLength(usize),

    /// HDR mastering luminance metadata is internally inconsistent or out of range.
    #[error("invalid HDR mastering metadata (max={max_nits}, max_fall={max_fall_nits})")]
    InvalidHdrMetadata {
        /// Requested peak luminance in nits.
        max_nits: u16,
        /// Requested maximum frame-average light level in nits.
        max_fall_nits: u16,
    },

    /// Display removal selector has neither a non-zero GUID nor a non-zero target ID.
    #[error("request must specify a valid non-zero monitor_guid or target_id")]
    InvalidSelector,

    /// Request flags contain reserved or unrecognized bits.
    #[error("unsupported request flags: {0:#x}")]
    InvalidFlags(u32),

    /// Display count in a list response exceeds the inline array capacity.
    #[error("display count {count} exceeds maximum {max}")]
    ListCountOutOfBounds {
        /// Declared display count.
        count: u32,
        /// Maximum supported inline entries.
        max: usize,
    },

    /// Wire byte used as a boolean flag is neither 0 nor 1.
    #[error("field `{field}` boolean byte must be 0 or 1, got {value}")]
    InvalidBooleanByte {
        /// Name of the wire field.
        field: &'static str,
        /// Invalid wire byte value.
        value: u8,
    },

    /// Color space discriminant byte is unrecognized.
    #[error("invalid color space preference byte: {0}")]
    InvalidColorSpace(u8),

    /// Bit depth discriminant byte is unrecognized.
    #[error("invalid bit depth preference byte: {0}")]
    InvalidBitDepth(u8),

    /// Requested target PPI is non-zero and outside `48..=600`.
    #[error("invalid target PPI {0}: expected 0 (default) or {MIN_TARGET_PPI}..={MAX_TARGET_PPI}")]
    InvalidTargetPpi(u16),

    /// Combination of HDR profile, color space, and bit depth is semantically invalid.
    #[error(
        "incompatible color settings (hdr_profile={hdr_profile}, color_space={color_space}, bit_depth={bit_depth})"
    )]
    IncompatibleColorSettings {
        /// Wire [`HdrProfile`] byte.
        hdr_profile: u8,
        /// Wire [`ColorSpacePreference`] byte.
        color_space: u8,
        /// Wire [`BitDepthPreference`] byte.
        bit_depth: u8,
    },
}

/// Trait implemented by wire payloads that support strict semantic validation.
pub trait Validate {
    /// Validates all wire fields and invariants before driver or client use.
    fn validate(&self) -> Result<(), ProtoError>;
}

/// Validates that the protocol major version matches [`PROTOCOL_MAJOR`].
pub fn validate_protocol_version(version: ProtocolVersion) -> Result<(), ProtoError> {
    if version.major != PROTOCOL_MAJOR {
        return Err(ProtoError::UnsupportedMajorVersion {
            expected: PROTOCOL_MAJOR,
            actual: version.major,
        });
    }
    Ok(())
}

/// Validates display dimensions against supported minimum and maximum bounds.
pub fn validate_dimensions(width: u32, height: u32) -> Result<(), ProtoError> {
    if width < MIN_DISPLAY_DIM
        || height < MIN_DISPLAY_DIM
        || width > MAX_DISPLAY_DIM
        || height > MAX_DISPLAY_DIM
    {
        return Err(ProtoError::InvalidDimensions { width, height });
    }
    Ok(())
}

/// Validates a rational refresh rate (`num / den`) using overflow-proof 64-bit arithmetic.
pub fn validate_refresh_rate(num: u32, den: u32) -> Result<(), ProtoError> {
    if num == 0 || den == 0 {
        return Err(ProtoError::InvalidRefreshRate { num, den });
    }
    let num64 = u64::from(num);
    let den64 = u64::from(den);
    // WHY: Compare cross-multiplied 64-bit integers instead of dividing to preserve exact
    // fractional bounds (e.g., 23.976 Hz vs 10 Hz) and avoid any rounding ambiguity.
    if num64 < den64 * MIN_REFRESH_HZ || num64 > den64 * MAX_REFRESH_HZ {
        return Err(ProtoError::InvalidRefreshRate { num, den });
    }
    Ok(())
}

/// Validates a bounded slice as valid UTF-8 without ASCII control characters.
pub fn validate_bounded_utf8_no_ctrl<'a>(
    field: &'static str,
    buf: &'a [u8],
    declared_len: usize,
    max_len: usize,
) -> Result<&'a str, ProtoError> {
    if declared_len > max_len || declared_len > buf.len() {
        return Err(ProtoError::FieldLengthOutOfBounds {
            field,
            len: declared_len,
            max: max_len,
        });
    }
    let slice = &buf[..declared_len];
    let text = str::from_utf8(slice).map_err(|_| ProtoError::InvalidUtf8 { field })?;
    if slice.iter().any(|&b| b < 0x20 || b == 0x7F) {
        return Err(ProtoError::NonPrintableAscii { field });
    }
    Ok(text)
}

/// Validates a bounded slice as strict printable 7-bit ASCII (`0x20..=0x7E`).
pub fn validate_bounded_printable_ascii<'a>(
    field: &'static str,
    buf: &'a [u8],
    declared_len: usize,
    max_len: usize,
) -> Result<&'a str, ProtoError> {
    let text = validate_bounded_utf8_no_ctrl(field, buf, declared_len, max_len)?;
    if !text.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
        return Err(ProtoError::NonPrintableAscii { field });
    }
    Ok(text)
}

/// Validates HDR mastering luminance metadata values.
pub fn validate_hdr_metadata(meta: HdrMasteringMetadata) -> Result<(), ProtoError> {
    let max_limit = if meta.max_luminance_nits == 0 {
        u32::from(MAX_HDR_LUMINANCE_NITS) * 10_000
    } else {
        u32::from(meta.max_luminance_nits) * 10_000
    };
    if meta.max_luminance_nits > MAX_HDR_LUMINANCE_NITS
        || (meta.max_luminance_nits > 0 && meta.max_fall_nits > meta.max_luminance_nits)
        || (meta.max_luminance_nits == 0 && meta.max_fall_nits > MAX_HDR_LUMINANCE_NITS)
        || meta.min_luminance_ten_thousandths > max_limit
    {
        return Err(ProtoError::InvalidHdrMetadata {
            max_nits: meta.max_luminance_nits,
            max_fall_nits: meta.max_fall_nits,
        });
    }
    Ok(())
}

/// Validates a wire boolean byte (`0` or `1`).
pub fn validate_bool_byte(field: &'static str, value: u8) -> Result<(), ProtoError> {
    if value > 1 {
        return Err(ProtoError::InvalidBooleanByte { field, value });
    }
    Ok(())
}

/// Validates an optional target pixel density (`0` = default 100 PPI, else `48..=600`).
pub fn validate_target_ppi(target_ppi: u16) -> Result<(), ProtoError> {
    if target_ppi != 0 && !(MIN_TARGET_PPI..=MAX_TARGET_PPI).contains(&target_ppi) {
        return Err(ProtoError::InvalidTargetPpi(target_ppi));
    }
    Ok(())
}

/// Validates semantic compatibility across HDR profile, color space, and bit-depth preferences.
pub fn validate_color_and_bit_depth(
    hdr_profile: HdrProfile,
    color_space: ColorSpacePreference,
    bit_depth: BitDepthPreference,
) -> Result<(), ProtoError> {
    let compat_color = match (hdr_profile, color_space) {
        (_, ColorSpacePreference::Auto)
        | (HdrProfile::Sdr, ColorSpacePreference::Bt709)
        | (HdrProfile::Hdr10, ColorSpacePreference::Bt2020 | ColorSpacePreference::DciP3) => true,
        (HdrProfile::Sdr, ColorSpacePreference::Bt2020 | ColorSpacePreference::DciP3)
        | (HdrProfile::Hdr10, ColorSpacePreference::Bt709) => false,
    };
    let compat_depth = !matches!(
        (hdr_profile, bit_depth),
        (HdrProfile::Hdr10, BitDepthPreference::Bpc8)
    );
    if !compat_color || !compat_depth {
        return Err(ProtoError::IncompatibleColorSettings {
            hdr_profile: hdr_profile.as_u8(),
            color_space: color_space.as_u8(),
            bit_depth: bit_depth.as_u8(),
        });
    }
    Ok(())
}

impl Validate for ProtocolVersion {
    fn validate(&self) -> Result<(), ProtoError> {
        validate_protocol_version(*self)
    }
}

impl Validate for AddDisplayRequest {
    fn validate(&self) -> Result<(), ProtoError> {
        validate_protocol_version(self.protocol_version)?;
        if self.monitor_guid == [0; 16] {
            return Err(ProtoError::InvalidSelector);
        }
        validate_dimensions(self.width, self.height)?;
        validate_refresh_rate(self.refresh_num, self.refresh_den)?;
        let hdr_profile = HdrProfile::try_from(self.hdr_profile)?;
        let color_space = ColorSpacePreference::try_from(self.color_space)?;
        let bit_depth = BitDepthPreference::try_from(self.bit_depth)?;
        validate_target_ppi(self.target_ppi)?;
        validate_color_and_bit_depth(hdr_profile, color_space, bit_depth)?;
        validate_hdr_metadata(self.hdr_metadata)?;

        let ownership = OwnershipMode::try_from(self.ownership_mode)?;
        match ownership {
            OwnershipMode::HandleBound => {
                if self.lease_ttl_secs > MAX_LEASE_TTL_SECS {
                    return Err(ProtoError::InvalidLeaseTtl(self.lease_ttl_secs));
                }
            }
            OwnershipMode::Leased => {
                if self.lease_ttl_secs == 0 || self.lease_ttl_secs > MAX_LEASE_TTL_SECS {
                    return Err(ProtoError::InvalidLeaseTtl(self.lease_ttl_secs));
                }
            }
        }

        let _ = validate_bounded_utf8_no_ctrl(
            "client_name",
            &self.client_name,
            usize::from(self.client_name_len),
            MAX_CLIENT_NAME_LEN,
        )?;
        let _ = validate_bounded_printable_ascii(
            "serial",
            &self.serial,
            usize::from(self.serial_len),
            MAX_SERIAL_LEN,
        )?;

        let edid_len = usize::from(self.custom_edid_len);
        if edid_len > MAX_CUSTOM_EDID_LEN {
            return Err(ProtoError::FieldLengthOutOfBounds {
                field: "custom_edid",
                len: edid_len,
                max: MAX_CUSTOM_EDID_LEN,
            });
        }
        if edid_len != 0 && (edid_len < 128 || !edid_len.is_multiple_of(128)) {
            return Err(ProtoError::InvalidCustomEdidLength(edid_len));
        }

        Ok(())
    }
}

impl Validate for AddDisplayResponse {
    fn validate(&self) -> Result<(), ProtoError> {
        validate_bool_byte("created_new", self.created_new)
    }
}

impl Validate for RemoveDisplayRequest {
    fn validate(&self) -> Result<(), ProtoError> {
        validate_protocol_version(self.protocol_version)?;
        if self.flags & !REMOVE_FLAG_BY_TARGET_ID != 0 {
            return Err(ProtoError::InvalidFlags(self.flags));
        }
        if (self.flags & REMOVE_FLAG_BY_TARGET_ID) != 0 {
            if self.target_id == 0 {
                return Err(ProtoError::InvalidSelector);
            }
        } else if self.monitor_guid == [0; 16] {
            return Err(ProtoError::InvalidSelector);
        }
        Ok(())
    }
}

impl Validate for DisplayInfo {
    fn validate(&self) -> Result<(), ProtoError> {
        validate_dimensions(self.width, self.height)?;
        validate_refresh_rate(self.refresh_num, self.refresh_den)?;
        let _ = HdrProfile::try_from(self.hdr_profile)?;
        let _ = OwnershipMode::try_from(self.ownership_mode)?;
        validate_bool_byte("active", self.active)?;
        let _ = validate_bounded_utf8_no_ctrl(
            "client_name",
            &self.client_name,
            usize::from(self.client_name_len),
            MAX_CLIENT_NAME_LEN,
        )?;
        Ok(())
    }
}

impl Validate for ListDisplaysResponse {
    fn validate(&self) -> Result<(), ProtoError> {
        validate_protocol_version(self.protocol_version)?;
        let count = self.count as usize;
        if count > MAX_LIST_DISPLAYS {
            return Err(ProtoError::ListCountOutOfBounds {
                count: self.count,
                max: MAX_LIST_DISPLAYS,
            });
        }
        for info in &self.displays[..count] {
            info.validate()?;
        }
        Ok(())
    }
}

impl Validate for SetRenderAdapterRequest {
    fn validate(&self) -> Result<(), ProtoError> {
        validate_protocol_version(self.protocol_version)
    }
}

impl Validate for RenewLeaseRequest {
    fn validate(&self) -> Result<(), ProtoError> {
        validate_protocol_version(self.protocol_version)?;
        if self.monitor_guid == [0; 16] {
            return Err(ProtoError::InvalidSelector);
        }
        if self.lease_ttl_secs == 0 || self.lease_ttl_secs > MAX_LEASE_TTL_SECS {
            return Err(ProtoError::InvalidLeaseTtl(self.lease_ttl_secs));
        }
        Ok(())
    }
}

impl Validate for DriverStatusResponse {
    fn validate(&self) -> Result<(), ProtoError> {
        validate_protocol_version(self.protocol_version)?;
        validate_bool_byte("adapter_ready", self.adapter_ready)?;
        validate_bool_byte("hdr_capable", self.hdr_capable)?;
        validate_bool_byte("last_gpu_switch_failed", self.last_gpu_switch_failed)?;
        Ok(())
    }
}

impl Validate for UpdateDisplayModeRequest {
    fn validate(&self) -> Result<(), ProtoError> {
        validate_protocol_version(self.protocol_version)?;
        if self.flags & !UPDATE_MODE_FLAG_BY_TARGET_ID != 0 {
            return Err(ProtoError::InvalidFlags(self.flags));
        }
        if (self.flags & UPDATE_MODE_FLAG_BY_TARGET_ID) != 0 {
            if self.target_id == 0 {
                return Err(ProtoError::InvalidSelector);
            }
        } else if self.monitor_guid == [0; 16] {
            return Err(ProtoError::InvalidSelector);
        }
        validate_dimensions(self.width, self.height)?;
        validate_refresh_rate(self.refresh_num, self.refresh_den)?;
        let hdr_profile = HdrProfile::try_from(self.hdr_profile)?;
        let color_space = ColorSpacePreference::try_from(self.color_space)?;
        let bit_depth = BitDepthPreference::try_from(self.bit_depth)?;
        validate_color_and_bit_depth(hdr_profile, color_space, bit_depth)?;
        validate_hdr_metadata(self.hdr_metadata)?;
        Ok(())
    }
}

impl Validate for UpdateDisplayModeResponse {
    fn validate(&self) -> Result<(), ProtoError> {
        Ok(())
    }
}
