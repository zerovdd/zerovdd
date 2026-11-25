//! EDID 1.4 Block 0 synthesis, chromaticity coordinates, and physical dimensions.

use crate::EdidError;

/// Standard 8-byte EDID 1.4 header signature (`00 FF FF FF FF FF FF 00`).
pub const EDID_HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

/// Default target pixel density (PPI) used for virtual display physical sizing.
pub const DEFAULT_TARGET_PPI: u16 = 100;

/// 10-bit CIE 1931 chromaticity primaries and white point (`0..=1023` representing `0.0..1.0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chromaticity10Bit {
    /// Red primary X coordinate (`round(x * 1024)`).
    pub rx: u16,
    /// Red primary Y coordinate (`round(y * 1024)`).
    pub ry: u16,
    /// Green primary X coordinate (`round(x * 1024)`).
    pub gx: u16,
    /// Green primary Y coordinate (`round(y * 1024)`).
    pub gy: u16,
    /// Blue primary X coordinate (`round(x * 1024)`).
    pub bx: u16,
    /// Blue primary Y coordinate (`round(y * 1024)`).
    pub by: u16,
    /// White point X coordinate (`round(x * 1024)`).
    pub wx: u16,
    /// White point Y coordinate (`round(y * 1024)`).
    pub wy: u16,
}

impl Chromaticity10Bit {
    /// Standard ITU-R BT.709 / sRGB primaries with D65 white point.
    pub const BT709: Self = Self {
        rx: 655,
        ry: 338,
        gx: 307,
        gy: 614,
        bx: 154,
        by: 61,
        wx: 320,
        wy: 337,
    };

    /// ITU-R BT.2020 wide color gamut primaries with D65 white point.
    pub const BT2020: Self = Self {
        rx: 725,
        ry: 299,
        gx: 174,
        gy: 816,
        bx: 134,
        by: 47,
        wx: 320,
        wy: 337,
    };

    /// Display P3 / DCI-P3 primaries with D65 white point.
    pub const DCI_P3: Self = Self {
        rx: 696,
        ry: 328,
        gx: 271,
        gy: 707,
        bx: 154,
        by: 61,
        wx: 320,
        wy: 337,
    };

    /// Returns `true` if all eight 10-bit chromaticity coordinates are zero.
    #[must_use]
    pub const fn is_all_zero(&self) -> bool {
        self.rx == 0
            && self.ry == 0
            && self.gx == 0
            && self.gy == 0
            && self.bx == 0
            && self.by == 0
            && self.wx == 0
            && self.wy == 0
    }

    /// Encodes the 10-bit chromaticity coordinates into EDID bytes `25..35`.
    #[must_use]
    pub const fn encode(&self) -> [u8; 10] {
        let rg_low = (((self.rx & 0x03) << 6)
            | ((self.ry & 0x03) << 4)
            | ((self.gx & 0x03) << 2)
            | (self.gy & 0x03)) as u8;
        let bw_low = (((self.bx & 0x03) << 6)
            | ((self.by & 0x03) << 4)
            | ((self.wx & 0x03) << 2)
            | (self.wy & 0x03)) as u8;
        [
            rg_low,
            bw_low,
            ((self.rx >> 2) & 0xFF) as u8,
            ((self.ry >> 2) & 0xFF) as u8,
            ((self.gx >> 2) & 0xFF) as u8,
            ((self.gy >> 2) & 0xFF) as u8,
            ((self.bx >> 2) & 0xFF) as u8,
            ((self.by >> 2) & 0xFF) as u8,
            ((self.wx >> 2) & 0xFF) as u8,
            ((self.wy >> 2) & 0xFF) as u8,
        ]
    }

    /// Decodes EDID bytes `25..35` back into 10-bit chromaticity coordinates.
    #[must_use]
    pub const fn decode(raw: &[u8; 10]) -> Self {
        let rg_low = raw[0] as u16;
        let bw_low = raw[1] as u16;
        Self {
            rx: ((raw[2] as u16) << 2) | ((rg_low >> 6) & 0x03),
            ry: ((raw[3] as u16) << 2) | ((rg_low >> 4) & 0x03),
            gx: ((raw[4] as u16) << 2) | ((rg_low >> 2) & 0x03),
            gy: ((raw[5] as u16) << 2) | (rg_low & 0x03),
            bx: ((raw[6] as u16) << 2) | ((bw_low >> 6) & 0x03),
            by: ((raw[7] as u16) << 2) | ((bw_low >> 4) & 0x03),
            wx: ((raw[8] as u16) << 2) | ((bw_low >> 2) & 0x03),
            wy: ((raw[9] as u16) << 2) | (bw_low & 0x03),
        }
    }
}

/// Color space preset for synthesized virtual monitor EDIDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorProfile {
    /// Standard Dynamic Range (BT.709 primaries, sRGB transfer bit set).
    #[default]
    SdrBt709,
    /// HDR / Wide Color Gamut with ITU-R BT.2020 primaries.
    HdrBt2020,
    /// HDR / Wide Color Gamut with Display P3 / DCI-P3 primaries.
    HdrDciP3,
}

impl ColorProfile {
    /// Returns the 10-bit CIE 1931 chromaticity coordinates for this profile.
    #[must_use]
    pub const fn chromaticity(self) -> Chromaticity10Bit {
        match self {
            Self::SdrBt709 => Chromaticity10Bit::BT709,
            Self::HdrBt2020 => Chromaticity10Bit::BT2020,
            Self::HdrDciP3 => Chromaticity10Bit::DCI_P3,
        }
    }

    /// Returns `true` if the EDID sRGB standard color space feature bit should be set.
    #[must_use]
    pub const fn sets_srgb_bit(self) -> bool {
        matches!(self, Self::SdrBt709)
    }

    /// Returns `true` if this profile targets HDR / wide color gamut output.
    #[must_use]
    pub const fn is_hdr(self) -> bool {
        matches!(self, Self::HdrBt2020 | Self::HdrDciP3)
    }
}

/// Consistent physical monitor dimensions in both Block 0 cm and DTD mm units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalDimensions {
    /// Horizontal screen size in centimeters (EDID Block 0 byte 21).
    pub width_cm: u8,
    /// Vertical screen size in centimeters (EDID Block 0 byte 22).
    pub height_cm: u8,
    /// Horizontal image size in millimeters (EDID DTD byte 66 + upper nibble of 68).
    pub width_mm: u16,
    /// Vertical image size in millimeters (EDID DTD byte 67 + lower nibble of 68).
    pub height_mm: u16,
}

/// Computes realistic physical dimensions (~96-110 PPI by default) consistently in cm and mm.
///
/// Ensures Block 0 cm bytes (`21..=22`) and DTD mm bytes (`66..=68`) agree within 5 mm
/// so Windows defaults to ~100% DPI scaling instead of 250-300%.
pub fn compute_physical_dimensions(
    width: u16,
    height: u16,
    target_ppi: u16,
) -> Result<PhysicalDimensions, EdidError> {
    if width == 0 || height == 0 {
        return Err(EdidError::InvalidDimensions { width, height });
    }
    if target_ppi == 0 {
        return Err(EdidError::InvalidTargetPpi(target_ppi));
    }

    let ppi = u32::from(target_ppi);
    // WHY: 1 inch = 25.4 mm, so mm = round(pixels * 254 / (ppi * 10)).
    // Clamping to 10..=2550 mm keeps DTD mm aligned with Block 0's 1..=255 cm range.
    let width_mm = ((u32::from(width) * 254 + ppi * 5) / (ppi * 10)).clamp(10, 2550) as u16;
    let height_mm = ((u32::from(height) * 254 + ppi * 5) / (ppi * 10)).clamp(10, 2550) as u16;
    let width_cm = ((u32::from(width_mm) + 5) / 10).clamp(1, 255) as u8;
    let height_cm = ((u32::from(height_mm) + 5) / 10).clamp(1, 255) as u8;

    Ok(PhysicalDimensions {
        width_cm,
        height_cm,
        width_mm,
        height_mm,
    })
}

/// Deterministic EDID identity fields derived from `monitor_guid` and `client_name`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorIdentity {
    /// 32-bit binary serial number written to Block 0 bytes `12..16`.
    pub binary_serial: u32,
    /// 13-byte ASCII serial payload written to descriptor `0xFF`.
    pub ascii_serial: [u8; 13],
    /// 13-byte ASCII product name payload written to descriptor `0xFC`.
    pub product_name: [u8; 13],
}

fn fnv1a_64(seed: u64, monitor_guid: &[u8; 16], client_name: &[u8]) -> u64 {
    let mut hash = 0xCBF2_9CE4_8422_2325u64 ^ seed;
    let prime = 0x0000_0100_0000_01B3u64;
    for &b in monitor_guid {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(prime);
    }
    for b in (client_name.len() as u32).to_le_bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(prime);
    }
    for &b in client_name {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(prime);
    }
    hash
}

const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

fn encode_edid_text_13(raw: &[u8]) -> [u8; 13] {
    let mut out = [0x20u8; 13];
    let len = raw.len().min(13);
    out[..len].copy_from_slice(&raw[..len]);
    if len < 13 {
        out[len] = 0x0A;
    }
    out
}

/// Derives deterministic binary serial, 13-char ASCII serial, and 13-char product name.
#[must_use]
pub fn derive_monitor_identity(monitor_guid: &[u8; 16], client_name: &str) -> MonitorIdentity {
    let h1 = fnv1a_64(0x9E37_79B9_7F4A_7C15, monitor_guid, client_name.as_bytes());
    let h2 = fnv1a_64(0x517C_C1B7_2722_0A95, monitor_guid, client_name.as_bytes());

    let folded = ((h1 >> 32) ^ h1) as u32;
    let binary_serial = if folded == 0 { 1 } else { folded };

    let mut ascii_serial = [b'Z'; 13];
    for (idx, slot) in ascii_serial[1..].iter_mut().enumerate() {
        let nibble = ((h2 >> (4 * (11 - idx))) & 0x0F) as usize;
        *slot = HEX_UPPER[nibble];
    }

    let mut sanitized = [0u8; 13];
    let mut sanitized_len = 0usize;
    let trimmed = client_name.trim();
    for ch in trimmed.chars() {
        if sanitized_len >= 13 {
            break;
        }
        let byte = if ch.is_ascii() {
            let b = ch as u8;
            if (0x20..=0x7E).contains(&b) {
                b
            } else {
                b'_'
            }
        } else {
            b'_'
        };
        sanitized[sanitized_len] = byte;
        sanitized_len += 1;
    }

    let product_name = if sanitized_len > 0 {
        encode_edid_text_13(&sanitized[..sanitized_len])
    } else {
        let mut fallback = *b"ZVDD-00000000";
        for (idx, slot) in fallback[5..].iter_mut().enumerate() {
            let nibble = ((h1 >> (4 * (7 - idx))) & 0x0F) as usize;
            *slot = HEX_UPPER[nibble];
        }
        fallback
    };

    MonitorIdentity {
        binary_serial,
        ascii_serial,
        product_name,
    }
}

/// Computes the 128-byte EDID block checksum byte given the first 127 bytes.
#[must_use]
pub fn compute_block_checksum(first_127: &[u8; 127]) -> u8 {
    let sum = first_127.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    0u8.wrapping_sub(sum)
}

/// Verifies a 128-byte EDID block checksum, returning `Err((expected, actual))` on mismatch.
pub fn verify_block_checksum(block: &[u8; 128]) -> Result<(), (u8, u8)> {
    let mut prefix = [0u8; 127];
    prefix.copy_from_slice(&block[..127]);
    let expected = compute_block_checksum(&prefix);
    let actual = block[127];
    if expected == actual {
        Ok(())
    } else {
        Err((expected, actual))
    }
}

fn encode_dtd(
    preferred_width: u16,
    preferred_height: u16,
    refresh_millihz: u32,
    dims: PhysicalDimensions,
) -> [u8; 18] {
    let mut dtd_w = preferred_width;
    let mut dtd_h = preferred_height;
    // WHY: EDID 1.4 DTD active fields are 12-bit (max 4095); scale 8K+ modes proportionally
    // for the Block 0 DTD while preserving full physical mm dimensions for Windows DPI sizing.
    while dtd_w > 4095 || dtd_h > 4095 {
        dtd_w = (dtd_w / 2).max(1);
        dtd_h = (dtd_h / 2).max(1);
    }

    let h_blank: u16 = 80;
    let h_sync_offset: u16 = 8;
    let h_sync_width: u16 = 32;

    let hz_millihz = u64::from(refresh_millihz.max(1000));
    let denom = 1_000_000_000u64.saturating_sub(460 * hz_millihz).max(1);
    let numer = 460u64 * u64::from(dtd_h) * hz_millihz;
    let v_blank_calc = numer.div_ceil(denom);
    let v_blank = (v_blank_calc.clamp(15, 4095)) as u16;
    let v_sync_width: u16 = 8;
    let v_sync_offset = v_blank.saturating_sub(14).clamp(1, 63);

    let total_w = u64::from(dtd_w) + u64::from(h_blank);
    let total_h = u64::from(dtd_h) + u64::from(v_blank);
    let pclk_10khz = ((total_w * total_h * hz_millihz) / 10_000_000).clamp(1, 65_535) as u16;
    let pclk_bytes = pclk_10khz.to_le_bytes();

    [
        pclk_bytes[0],
        pclk_bytes[1],
        (dtd_w & 0xFF) as u8,
        (h_blank & 0xFF) as u8,
        ((((dtd_w >> 8) & 0x0F) << 4) | ((h_blank >> 8) & 0x0F)) as u8,
        (dtd_h & 0xFF) as u8,
        (v_blank & 0xFF) as u8,
        ((((dtd_h >> 8) & 0x0F) << 4) | ((v_blank >> 8) & 0x0F)) as u8,
        (h_sync_offset & 0xFF) as u8,
        (h_sync_width & 0xFF) as u8,
        (((v_sync_offset & 0x0F) << 4) | (v_sync_width & 0x0F)) as u8,
        ((((h_sync_offset >> 8) & 0x03) << 6)
            | (((h_sync_width >> 8) & 0x03) << 4)
            | (((v_sync_offset >> 4) & 0x03) << 2)
            | ((v_sync_width >> 4) & 0x03)) as u8,
        (dims.width_mm & 0xFF) as u8,
        (dims.height_mm & 0xFF) as u8,
        ((((dims.width_mm >> 8) & 0x0F) << 4) | ((dims.height_mm >> 8) & 0x0F)) as u8,
        0x00,
        0x00,
        0x1E,
    ]
}

fn encode_display_descriptor(tag: u8, payload_13: &[u8; 13]) -> [u8; 18] {
    let mut out = [0u8; 18];
    out[3] = tag;
    out[5..18].copy_from_slice(payload_13);
    out
}

fn encode_range_limits_descriptor() -> [u8; 18] {
    [
        0x00, 0x00, 0x00, 0xFD, 0x00, 24, 240, 15, 255, 255, 0x00, 0x0A, 0x20, 0x20, 0x20, 0x20,
        0x20, 0x20,
    ]
}

/// Parameters required to synthesize EDID 1.4 Block 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block0Params<'a> {
    /// Unique per-monitor GUID (`MonitorContainerId`).
    pub monitor_guid: [u8; 16],
    /// Client display/device name used to derive product name and serial.
    pub client_name: &'a str,
    /// Preferred active horizontal resolution in pixels.
    pub preferred_width: u16,
    /// Preferred active vertical resolution in pixels.
    pub preferred_height: u16,
    /// Preferred refresh rate in millihertz (for example `60_000` or `59_940`).
    pub preferred_refresh_millihz: u32,
    /// Target pixel density in PPI (defaults to `100` when `None`).
    pub target_ppi: Option<u16>,
    /// Color profile controlling CIE 1931 chromaticity and sRGB feature flag.
    pub color_profile: ColorProfile,
    /// Optional explicit digital bit depth (`8`, `10`, or `12` bpc) overriding `color_profile` default.
    pub bit_depth_bpc: Option<u8>,
    /// Number of 128-byte extension blocks following Block 0.
    pub extension_count: u8,
}

/// Synthesizes a spec-compliant 128-byte EDID 1.4 Block 0 with valid checksum.
pub fn synthesize_block0(params: &Block0Params<'_>) -> Result<[u8; 128], EdidError> {
    if params.preferred_refresh_millihz == 0 {
        return Err(EdidError::InvalidRefreshRate(
            params.preferred_refresh_millihz,
        ));
    }

    let ppi = params.target_ppi.unwrap_or(DEFAULT_TARGET_PPI);
    let dims = compute_physical_dimensions(params.preferred_width, params.preferred_height, ppi)?;
    let identity = derive_monitor_identity(&params.monitor_guid, params.client_name);
    let chroma = params.color_profile.chromaticity();

    let mut block = [0u8; 128];
    block[0..8].copy_from_slice(&EDID_HEADER);

    // WHY: Manufacturer ID "ZVD" packed as 5-bit ASCII offsets from '@': Z=26, V=22, D=4.
    let mfg: u16 = (26 << 10) | (22 << 5) | 4;
    block[8..10].copy_from_slice(&mfg.to_be_bytes());
    block[10..12].copy_from_slice(&0x5644u16.to_le_bytes());
    block[12..16].copy_from_slice(&identity.binary_serial.to_le_bytes());

    block[16] = 1;
    block[17] = 36;
    block[18] = 1;
    block[19] = 4;

    // WHY: Digital DisplayPort input (0x05) with explicit 8/10/12-bpc bit depth (default 10 for HDR, 8 for SDR).
    let default_bpc = if params.color_profile.is_hdr() { 10 } else { 8 };
    let bit_depth_bits = match params.bit_depth_bpc.unwrap_or(default_bpc) {
        8 => 0x20,
        10 => 0x30,
        12 => 0x40,
        other => return Err(EdidError::InvalidBitDepth(other)),
    };
    block[20] = 0x80 | bit_depth_bits | 0x05;

    block[21] = dims.width_cm;
    block[22] = dims.height_cm;
    block[23] = 120;

    // WHY: Bit 1 (0x02) marks Preferred Timing Mode in EDID 1.4; bit 2 (0x04) is sRGB
    // and is only set when chromaticity coordinates match BT.709 / sRGB.
    let mut feature_support = 0x18 | 0x02;
    if params.color_profile.sets_srgb_bit() {
        feature_support |= 0x04;
    }
    block[24] = feature_support;

    block[25..35].copy_from_slice(&chroma.encode());

    block[35] = 0x21;
    block[36] = 0x08;
    block[37] = 0x00;

    for chunk in block[38..54].as_chunks_mut::<2>().0 {
        chunk[0] = 0x01;
        chunk[1] = 0x01;
    }

    let dtd = encode_dtd(
        params.preferred_width,
        params.preferred_height,
        params.preferred_refresh_millihz,
        dims,
    );
    block[54..72].copy_from_slice(&dtd);
    block[72..90].copy_from_slice(&encode_range_limits_descriptor());
    block[90..108].copy_from_slice(&encode_display_descriptor(0xFC, &identity.product_name));
    block[108..126].copy_from_slice(&encode_display_descriptor(0xFF, &identity.ascii_serial));

    block[126] = params.extension_count;

    let mut prefix = [0u8; 127];
    prefix.copy_from_slice(&block[..127]);
    block[127] = compute_block_checksum(&prefix);

    Ok(block)
}
