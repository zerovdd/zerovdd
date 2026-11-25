//! `#![no_std]` EDID 1.4 and CTA-861-G synthesizer, HDR10 metadata encoder, and validator.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use thiserror::Error;

/// EDID 1.4 Block 0 synthesis, chromaticity, and physical dimensions.
pub mod block0;
/// CTA-861-G Extension Block (Block 1) synthesis.
pub mod cta861;
/// CTA-861-G HDR static metadata encoding and decoding.
pub mod hdr;
/// Bounds-checked EDID parser and custom EDID validator.
pub mod validate;

pub use block0::{
    compute_physical_dimensions, derive_monitor_identity, synthesize_block0, Block0Params,
    Chromaticity10Bit, ColorProfile, MonitorIdentity, PhysicalDimensions, DEFAULT_TARGET_PPI,
    EDID_HEADER,
};
pub use cta861::{
    synthesize_cta861_block, ColorimetryFlags, Cta861Params, CTA861_EXTENSION_TAG, CTA861_REVISION,
};
pub use hdr::{
    decode_max_luminance_millinits, decode_min_luminance_ten_thousandths,
    encode_max_luminance_millinits, encode_max_luminance_nits,
    encode_min_luminance_ten_thousandths, EotfFlags, HdrStaticMetadata,
};
pub use validate::{
    validate_custom_edid, Cta861Summary, DetailedTimingSummary, EdidSummary, HdrSummary,
};

/// Errors returned during EDID synthesis or custom EDID validation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EdidError {
    /// Custom EDID byte slice is shorter than the minimum 128-byte base block.
    #[error("EDID length {len} is smaller than the minimum 128-byte Block 0")]
    TooShort {
        /// Actual byte length provided.
        len: usize,
    },
    /// Custom EDID byte slice is not an exact multiple of 128 bytes.
    #[error("EDID length {len} is not a multiple of 128 bytes")]
    InvalidLength {
        /// Actual byte length provided.
        len: usize,
    },
    /// Block 0 does not begin with the standard 8-byte EDID header.
    #[error("invalid 8-byte EDID header signature")]
    InvalidHeader,
    /// EDID structure version is unsupported (expected version 1).
    #[error("unsupported EDID version {version}.{revision}")]
    UnsupportedVersion {
        /// Parsed EDID version byte.
        version: u8,
        /// Parsed EDID revision byte.
        revision: u8,
    },
    /// Declared extension block count does not match the slice block count.
    #[error("EDID extension count mismatch: header declares {declared} extension(s) but buffer has {actual_blocks} total block(s)")]
    ExtensionCountMismatch {
        /// Extension block count stored at byte 126.
        declared: u8,
        /// Actual number of 128-byte blocks in the buffer.
        actual_blocks: usize,
    },
    /// Block checksum byte failed modulo-256 verification.
    #[error("invalid checksum in EDID block {block_index}: expected 0x{expected:02X}, got 0x{actual:02X}")]
    InvalidChecksum {
        /// Zero-based 128-byte block index.
        block_index: usize,
        /// Computed expected checksum byte.
        expected: u8,
        /// Actual checksum byte stored at offset 127.
        actual: u8,
    },
    /// Extension block structure contains invalid offsets or truncated data blocks.
    #[error("malformed CTA-861 extension block at index {block_index}")]
    MalformedExtensionBlock {
        /// Zero-based 128-byte block index.
        block_index: usize,
    },
    /// Preferred resolution contains zero width or height.
    #[error("invalid monitor dimensions {width}x{height}")]
    InvalidDimensions {
        /// Requested width in pixels.
        width: u16,
        /// Requested height in pixels.
        height: u16,
    },
    /// Preferred refresh rate in millihertz is zero.
    #[error("invalid preferred refresh rate {0} millihz")]
    InvalidRefreshRate(u32),
    /// Requested target PPI is zero.
    #[error("invalid target PPI {0}")]
    InvalidTargetPpi(u16),
    /// Requested EDID Block 0 bit depth is unsupported (expected `8`, `10`, or `12` bpc).
    #[error("invalid bit depth {0} bpc (expected 8, 10, or 12)")]
    InvalidBitDepth(u8),
}

/// Complete specification for synthesizing a 256-byte EDID 1.4 + CTA-861-G descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdidSpec<'a> {
    /// Deterministic per-monitor GUID (`MonitorContainerId`).
    pub monitor_guid: [u8; 16],
    /// Client display/device name used to derive product name and serial.
    pub client_name: &'a str,
    /// Preferred horizontal resolution in pixels.
    pub preferred_width: u16,
    /// Preferred vertical resolution in pixels.
    pub preferred_height: u16,
    /// Preferred refresh rate in millihertz (for example `60_000` or `59_940`).
    pub preferred_refresh_millihz: u32,
    /// Optional target pixel density in PPI (defaults to `100` PPI).
    pub target_ppi: Option<u16>,
    /// Color space preset (BT.709 for SDR, BT.2020 or DCI-P3 for HDR/WCG).
    pub color_profile: ColorProfile,
    /// Optional explicit color depth per channel (`8`, `10`, or `12` bpc).
    pub bit_depth_bpc: Option<u8>,
    /// Optional CTA-861-G colorimetry override (auto-selected from `color_profile` when `None`).
    pub colorimetry_override: Option<ColorimetryFlags>,
    /// Optional CTA-861-G HDR static metadata override (auto-populated for HDR profiles when `None`).
    pub hdr_override: Option<Option<HdrStaticMetadata>>,
}

impl<'a> EdidSpec<'a> {
    /// Creates an `EdidSpec` with sensible defaults for the given monitor identity and mode.
    #[must_use]
    pub fn new(
        monitor_guid: [u8; 16],
        client_name: &'a str,
        preferred_width: u16,
        preferred_height: u16,
        preferred_refresh_millihz: u32,
    ) -> Self {
        Self {
            monitor_guid,
            client_name,
            preferred_width,
            preferred_height,
            preferred_refresh_millihz,
            target_ppi: None,
            color_profile: ColorProfile::SdrBt709,
            bit_depth_bpc: None,
            colorimetry_override: None,
            hdr_override: None,
        }
    }
}

/// Synthesizes a 256-byte EDID 1.4 + CTA-861-G binary blob for a virtual monitor.
pub fn synthesize_edid(spec: &EdidSpec<'_>) -> Result<[u8; 256], EdidError> {
    let block0 = synthesize_block0(&Block0Params {
        monitor_guid: spec.monitor_guid,
        client_name: spec.client_name,
        preferred_width: spec.preferred_width,
        preferred_height: spec.preferred_height,
        preferred_refresh_millihz: spec.preferred_refresh_millihz,
        target_ppi: spec.target_ppi,
        color_profile: spec.color_profile,
        bit_depth_bpc: spec.bit_depth_bpc,
        extension_count: 1,
    })?;

    let colorimetry = spec.colorimetry_override.unwrap_or_else(|| {
        if spec.color_profile.is_hdr() {
            ColorimetryFlags::BT2020_RGB_AND_YCC
        } else {
            ColorimetryFlags::NONE
        }
    });
    let hdr = spec.hdr_override.unwrap_or_else(|| {
        if spec.color_profile.is_hdr() {
            Some(HdrStaticMetadata::default())
        } else {
            None
        }
    });

    let block1 = synthesize_cta861_block(&Cta861Params { colorimetry, hdr });

    let mut out = [0u8; 256];
    out[..128].copy_from_slice(&block0);
    out[128..256].copy_from_slice(&block1);
    Ok(out)
}

/// Synthesizes a 256-byte EDID 1.4 + CTA-861-G blob into an owned `Vec<u8>`.
pub fn synthesize_edid_vec(spec: &EdidSpec<'_>) -> Result<Vec<u8>, EdidError> {
    let bytes = synthesize_edid(spec)?;
    Ok(bytes.to_vec())
}
