//! CTA-861-G Extension Block (Block 1) synthesis for Video, Speaker, Colorimetry, and HDR10.

use crate::block0::compute_block_checksum;
use crate::hdr::{
    decode_max_luminance_millinits, encode_max_luminance_nits,
    encode_min_luminance_ten_thousandths, HdrStaticMetadata,
};

/// CTA-861 Extension Block tag byte (`0x02`).
pub const CTA861_EXTENSION_TAG: u8 = 0x02;

/// CTA-861 Version 3 (CTA-861-G) revision byte (`0x03`).
pub const CTA861_REVISION: u8 = 0x03;

/// CTA-861-G Colorimetry Data Block flags (Extended Tag `0x05`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorimetryFlags(u8);

impl ColorimetryFlags {
    /// No extended colorimetry flags set.
    pub const NONE: Self = Self(0x00);
    /// xvYCC601 (`0x01`).
    pub const XVYCC601: Self = Self(0x01);
    /// xvYCC709 (`0x02`).
    pub const XVYCC709: Self = Self(0x02);
    /// sYCC601 (`0x04`).
    pub const SYCC601: Self = Self(0x04);
    /// opYCC601 (`0x08`).
    pub const OP_YCC601: Self = Self(0x08);
    /// opRGB (`0x10`).
    pub const OP_RGB: Self = Self(0x10);
    /// BT.2020 constant luminance YCC (`0x20`).
    pub const BT2020_CYCC: Self = Self(0x20);
    /// BT.2020 non-constant luminance YCC (`0x40`).
    pub const BT2020_YCC: Self = Self(0x40);
    /// BT.2020 RGB (`0x80`).
    pub const BT2020_RGB: Self = Self(0x80);
    /// Standard wide-color-gamut BT.2020 flags (`BT2020_RGB | BT2020_YCC`).
    pub const BT2020_RGB_AND_YCC: Self = Self(0x80 | 0x40);

    /// Creates a `ColorimetryFlags` from a raw CTA-861 byte.
    #[must_use]
    pub const fn from_raw(raw: u8) -> Self {
        Self(raw)
    }

    /// Returns the raw CTA-861 colorimetry byte.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Returns the bitwise union of two colorimetry flag sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Returns `true` if all bits in `other` are present in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

/// Parameters controlling CTA-861-G Block 1 synthesis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cta861Params {
    /// Colorimetry Data Block flags (omitted if `ColorimetryFlags::NONE`).
    pub colorimetry: ColorimetryFlags,
    /// Optional HDR Static Metadata Data Block (`Extended Tag 0x06`).
    pub hdr: Option<HdrStaticMetadata>,
}

impl Default for Cta861Params {
    fn default() -> Self {
        Self {
            colorimetry: ColorimetryFlags::BT2020_RGB_AND_YCC,
            hdr: Some(HdrStaticMetadata::default()),
        }
    }
}

/// Synthesizes a 128-byte CTA-861-G extension block with valid checksum.
#[must_use]
pub fn synthesize_cta861_block(params: &Cta861Params) -> [u8; 128] {
    let mut block = [0u8; 128];
    block[0] = CTA861_EXTENSION_TAG;
    block[1] = CTA861_REVISION;
    // WHY: Advertise YCbCr 4:4:4 (0x20) + YCbCr 4:2:2 (0x10) support with 0 native DTDs in Block 1.
    block[3] = 0x30;

    let mut cursor = 4usize;

    // WHY: Video Data Block (tag 2, len 4) lists common CEA VICs: 2160p60, 1080p60, 720p60, 1080p50.
    let video_vics: [u8; 4] = [97, 16, 4, 31];
    block[cursor] = (2 << 5) | (video_vics.len() as u8);
    cursor += 1;
    block[cursor..cursor + video_vics.len()].copy_from_slice(&video_vics);
    cursor += video_vics.len();

    // WHY: Speaker Allocation Data Block (tag 4, len 3) advertises standard stereo FL/FR (0x01).
    block[cursor] = (4 << 5) | 3;
    block[cursor + 1] = 0x01;
    block[cursor + 2] = 0x00;
    block[cursor + 3] = 0x00;
    cursor += 4;

    if params.colorimetry.bits() != 0 {
        block[cursor] = (7 << 5) | 3;
        block[cursor + 1] = 0x05;
        block[cursor + 2] = params.colorimetry.bits();
        block[cursor + 3] = 0x00;
        cursor += 4;
    }

    if let Some(hdr) = params.hdr {
        let max_code = hdr
            .max_luminance_nits
            .map(encode_max_luminance_nits)
            .unwrap_or(0);
        let max_fall_code = hdr
            .max_fall_nits
            .map(encode_max_luminance_nits)
            .unwrap_or(0);
        // WHY: CTA-861-G derives min luminance relative to encoded max luminance; default to
        // 1000 nits (1_000_000 millinits) if max luminance is unspecified.
        let ref_max_millinits = decode_max_luminance_millinits(max_code).unwrap_or(1_000_000);
        let min_code = hdr
            .min_luminance_ten_thousandths
            .map(|val| encode_min_luminance_ten_thousandths(val, ref_max_millinits))
            .unwrap_or(0);

        block[cursor] = (7 << 5) | 6;
        block[cursor + 1] = 0x06;
        block[cursor + 2] = hdr.eotf.bits();
        block[cursor + 3] = u8::from(hdr.static_metadata_type1);
        block[cursor + 4] = max_code;
        block[cursor + 5] = max_fall_code;
        block[cursor + 6] = min_code;
        cursor += 7;
    }

    block[2] = cursor as u8;

    let mut prefix = [0u8; 127];
    prefix.copy_from_slice(&block[..127]);
    block[127] = compute_block_checksum(&prefix);

    block
}
