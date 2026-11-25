//! Pure `#![no_std]` CTA-861-G HDR static metadata encoding and decoding.

/// Supported Electro-Optical Transfer Function (EOTF) bitmask for CTA-861-G.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EotfFlags(u8);

impl EotfFlags {
    /// Traditional gamma - SDR luminance range (`0x01`).
    pub const SDR_GAMMA: Self = Self(0x01);
    /// Traditional gamma - HDR luminance range (`0x02`).
    pub const HDR_GAMMA: Self = Self(0x02);
    /// SMPTE ST 2084 perceptual quantizer (PQ) (`0x04`).
    pub const PQ: Self = Self(0x04);
    /// Hybrid Log-Gamma (HLG) (`0x08`).
    pub const HLG: Self = Self(0x08);
    /// Default HDR10 + HLG EOTF mask (`PQ | HLG`).
    pub const PQ_AND_HLG: Self = Self(0x04 | 0x08);

    /// Creates an `EotfFlags` from raw CTA-861-G EOTF bits (masked to valid bits `0x0F`).
    #[must_use]
    pub const fn from_raw(raw: u8) -> Self {
        Self(raw & 0x0F)
    }

    /// Returns the raw CTA-861-G EOTF byte.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Returns the bitwise union of two EOTF flag sets.
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

/// HDR static metadata parameters encoded into CTA-861-G Extended Tag `0x06`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HdrStaticMetadata {
    /// Supported EOTF flags (for example `EotfFlags::PQ_AND_HLG`).
    pub eotf: EotfFlags,
    /// Whether Static Metadata Type 1 (`0x01`) is supported.
    pub static_metadata_type1: bool,
    /// Desired content maximum luminance in nits (`None` encodes `0` = unspecified).
    pub max_luminance_nits: Option<u16>,
    /// Desired content maximum frame-average luminance in nits (`None` encodes `0` = unspecified).
    pub max_fall_nits: Option<u16>,
    /// Desired content minimum luminance in `0.0001` nit units (`500` = `0.05` nits).
    pub min_luminance_ten_thousandths: Option<u32>,
}

impl Default for HdrStaticMetadata {
    fn default() -> Self {
        Self {
            eotf: EotfFlags::PQ_AND_HLG,
            static_metadata_type1: true,
            max_luminance_nits: Some(1000),
            max_fall_nits: Some(400),
            min_luminance_ten_thousandths: Some(500),
        }
    }
}

// WHY: Fixed-point Q20 lookup of 2^(k/32) for k in 0..32 enables exact CTA-861-G
// log/pow luminance encoding in pure #![no_std] integer arithmetic without libm.
const POW2_OVER_32_Q20: [u32; 32] = [
    1_048_576, 1_071_537, 1_095_000, 1_118_978, 1_143_480, 1_168_519, 1_194_106, 1_220_254,
    1_246_974, 1_274_279, 1_302_182, 1_330_696, 1_359_835, 1_389_611, 1_420_040, 1_451_135,
    1_482_910, 1_515_382, 1_548_564, 1_582_474, 1_617_125, 1_652_536, 1_688_722, 1_725_700,
    1_763_488, 1_802_103, 1_841_564, 1_881_889, 1_923_097, 1_965_207, 2_008_240, 2_052_214,
];

/// Decodes a CTA-861-G `Max Luminance` or `MaxFALL` code (`1..=255`) into millinits.
///
/// Implements `L = 50 * 2^(CV / 32)` nits (`50_000 * 2^(CV / 32)` millinits).
/// Returns `None` when `code == 0` (unspecified per CTA-861-G).
#[must_use]
pub const fn decode_max_luminance_millinits(code: u8) -> Option<u32> {
    if code == 0 {
        return None;
    }
    let shift = (code / 32) as u32;
    let rem = (code % 32) as usize;
    let q20 = POW2_OVER_32_Q20[rem] as u64;
    let scaled = (50_000u64 * q20) << shift;
    let rounded = (scaled + (1u64 << 19)) >> 20;
    Some(rounded as u32)
}

/// Encodes a luminance value in nits into the closest CTA-861-G 8-bit code (`1..=255`).
///
/// Returns `0` only if `nits == 0`.
#[must_use]
pub const fn encode_max_luminance_nits(nits: u16) -> u8 {
    if nits == 0 {
        return 0;
    }
    encode_max_luminance_millinits((nits as u32) * 1000)
}

/// Encodes a luminance value in millinits into the closest CTA-861-G 8-bit code (`1..=255`).
///
/// Returns `0` only if `millinits == 0`.
#[must_use]
pub const fn encode_max_luminance_millinits(millinits: u32) -> u8 {
    if millinits == 0 {
        return 0;
    }
    let mut best_code: u8 = 1;
    let mut best_err: u32 = u32::MAX;
    let mut lo: u16 = 1;
    let mut hi: u16 = 255;
    while lo <= hi {
        let mid = ((lo + hi) / 2) as u8;
        let Some(val) = decode_max_luminance_millinits(mid) else {
            break;
        };
        let err = val.abs_diff(millinits);
        if err < best_err {
            best_err = err;
            best_code = mid;
        }
        if val < millinits {
            lo = (mid as u16) + 1;
        } else if val > millinits {
            if mid == 1 {
                break;
            }
            hi = (mid as u16) - 1;
        } else {
            return mid;
        }
    }
    best_code
}

/// Decodes a CTA-861-G `Min Luminance` code (`1..=255`) into `0.0001` nit units.
///
/// Implements `L_min = L_max * (CV_min / 255)^2 / 100`.
/// Returns `None` when `code == 0` or `max_luminance_millinits == 0`.
#[must_use]
pub const fn decode_min_luminance_ten_thousandths(
    code: u8,
    max_luminance_millinits: u32,
) -> Option<u32> {
    if code == 0 || max_luminance_millinits == 0 {
        return None;
    }
    // WHY: 1 millinit = 10 ten-thousandths of a nit, so dividing by 100 * 255^2
    // converts L_max (in millinits) * 10 * code^2 directly to 0.0001-nit units.
    let denom: u64 = 255 * 255 * 100;
    let numer: u64 = (max_luminance_millinits as u64) * 10 * (code as u64) * (code as u64);
    let rounded = (numer + denom / 2) / denom;
    Some(rounded as u32)
}

/// Encodes a minimum luminance (`0.0001` nit units) into the closest CTA-861-G code (`1..=255`).
///
/// Returns `0` when `min_ten_thousandths == 0` or `max_luminance_millinits == 0`.
#[must_use]
pub const fn encode_min_luminance_ten_thousandths(
    min_ten_thousandths: u32,
    max_luminance_millinits: u32,
) -> u8 {
    if min_ten_thousandths == 0 || max_luminance_millinits == 0 {
        return 0;
    }
    let mut best_code: u8 = 1;
    let mut best_err: u32 = u32::MAX;
    let mut lo: u16 = 1;
    let mut hi: u16 = 255;
    while lo <= hi {
        let mid = ((lo + hi) / 2) as u8;
        let Some(val) = decode_min_luminance_ten_thousandths(mid, max_luminance_millinits) else {
            break;
        };
        let err = val.abs_diff(min_ten_thousandths);
        if err < best_err {
            best_err = err;
            best_code = mid;
        }
        if val < min_ten_thousandths {
            lo = (mid as u16) + 1;
        } else if val > min_ten_thousandths {
            if mid == 1 {
                break;
            }
            hi = (mid as u16) - 1;
        } else {
            return mid;
        }
    }
    best_code
}
