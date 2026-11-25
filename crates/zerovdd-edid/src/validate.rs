//! Bounds-checked EDID 1.4 and CTA-861-G parser and custom EDID validator.

use alloc::string::String;
use alloc::vec::Vec;

use crate::block0::{verify_block_checksum, Chromaticity10Bit, EDID_HEADER};
use crate::cta861::{ColorimetryFlags, CTA861_EXTENSION_TAG};
use crate::hdr::{decode_max_luminance_millinits, decode_min_luminance_ten_thousandths, EotfFlags};
use crate::EdidError;

/// Parsed timing summary from a Block 0 Detailed Timing Descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetailedTimingSummary {
    /// Active horizontal pixels (`12-bit`).
    pub width: u16,
    /// Active vertical lines (`12-bit`).
    pub height: u16,
    /// Pixel clock in Hz (`10 kHz` unit multiplied by `10_000`).
    pub pixel_clock_hz: u32,
}

/// Decoded HDR Static Metadata Data Block summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HdrSummary {
    /// Supported EOTF flags.
    pub eotf: EotfFlags,
    /// Whether Static Metadata Type 1 is supported.
    pub static_metadata_type1: bool,
    /// Raw encoded max luminance byte (`0` = unspecified).
    pub max_luminance_code: u8,
    /// Raw encoded max frame-average luminance byte (`0` = unspecified).
    pub max_fall_code: u8,
    /// Raw encoded min luminance byte (`0` = unspecified).
    pub min_luminance_code: u8,
    /// Decoded max luminance in millinits.
    pub max_luminance_millinits: Option<u32>,
    /// Decoded max frame-average luminance in millinits.
    pub max_fall_millinits: Option<u32>,
    /// Decoded min luminance in `0.0001` nit units.
    pub min_luminance_ten_thousandths: Option<u32>,
}

/// Decoded CTA-861 extension block summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cta861Summary {
    /// CTA-861 revision number (typically `3` for CTA-861-G).
    pub revision: u8,
    /// CEA Video Identification Codes advertised in Video Data Blocks.
    pub video_vics: Vec<u8>,
    /// Whether a Speaker Allocation Data Block was present.
    pub has_speaker_allocation: bool,
    /// Colorimetry Data Block flags, if present.
    pub colorimetry: Option<ColorimetryFlags>,
    /// HDR Static Metadata Data Block summary, if present.
    pub hdr_static_metadata: Option<HdrSummary>,
}

/// Validated summary of an EDID 1.4 binary blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdidSummary {
    /// EDID structure version (`1`).
    pub version: u8,
    /// EDID structure revision (for example `4` for EDID 1.4).
    pub revision: u8,
    /// 3-letter PNP manufacturer identifier.
    pub manufacturer: [char; 3],
    /// 16-bit product code.
    pub product_code: u16,
    /// 32-bit binary serial number (Block 0 bytes `12..16`).
    pub binary_serial: u32,
    /// 13-character ASCII serial string from descriptor `0xFF`, if present.
    pub ascii_serial: Option<String>,
    /// 13-character product name string from descriptor `0xFC`, if present.
    pub product_name: Option<String>,
    /// Physical screen size from Block 0 bytes `21..=22` (`width_cm`, `height_cm`).
    pub block0_size_cm: (u8, u8),
    /// Physical image size from the preferred DTD (`width_mm`, `height_mm`), if present.
    pub dtd_size_mm: Option<(u16, u16)>,
    /// Preferred Detailed Timing Descriptor summary, if present.
    pub preferred_timing: Option<DetailedTimingSummary>,
    /// 10-bit CIE 1931 chromaticity coordinates.
    pub chromaticity: Chromaticity10Bit,
    /// Whether the Block 0 sRGB standard color space bit (`byte 24 & 0x04`) is set.
    pub srgb_bit_set: bool,
    /// Number of extension blocks declared in Block 0 byte `126`.
    pub extension_blocks: u8,
    /// Summary of the first CTA-861 extension block, if present.
    pub cta861: Option<Cta861Summary>,
}

fn decode_manufacturer(raw: [u8; 2]) -> [char; 3] {
    let packed = u16::from_be_bytes(raw);
    let letter = |shift: u16| -> char {
        let code = ((packed >> shift) & 0x1F) as u8;
        if (1..=26).contains(&code) {
            char::from(b'@' + code)
        } else {
            '?'
        }
    };
    [letter(10), letter(5), letter(0)]
}

fn decode_descriptor_text(payload: &[u8]) -> String {
    let end = payload
        .iter()
        .position(|&b| b == 0x0A)
        .unwrap_or(payload.len());
    let mut out = String::new();
    for &b in &payload[..end] {
        if (0x20..=0x7E).contains(&b) {
            out.push(char::from(b));
        } else {
            out.push('?');
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

fn parse_cta861_block(block: &[u8; 128], block_index: usize) -> Result<Cta861Summary, EdidError> {
    let revision = block[1];
    let dtd_offset = usize::from(block[2]);
    if dtd_offset > 127 || (dtd_offset != 0 && dtd_offset < 4) {
        return Err(EdidError::MalformedExtensionBlock { block_index });
    }

    let mut video_vics = Vec::new();
    let mut has_speaker_allocation = false;
    let mut colorimetry = None;
    let mut hdr_static_metadata = None;

    if dtd_offset > 4 {
        let mut cursor = 4usize;
        while cursor < dtd_offset {
            let header = block[cursor];
            let tag = (header >> 5) & 0x07;
            let len = usize::from(header & 0x1F);
            cursor += 1;
            if cursor + len > dtd_offset {
                return Err(EdidError::MalformedExtensionBlock { block_index });
            }
            let payload = &block[cursor..cursor + len];
            match tag {
                2 => {
                    for &vic_byte in payload {
                        video_vics.push(vic_byte & 0x7F);
                    }
                }
                4 => {
                    if len >= 3 {
                        has_speaker_allocation = true;
                    }
                }
                7 => {
                    if let Some(&ext_tag) = payload.first() {
                        match ext_tag {
                            0x05 if len >= 3 => {
                                colorimetry = Some(ColorimetryFlags::from_raw(payload[1]));
                            }
                            0x06 if len >= 3 => {
                                let eotf = EotfFlags::from_raw(payload[1]);
                                let static_metadata_type1 = (payload[2] & 0x01) != 0;
                                let max_luminance_code = payload.get(3).copied().unwrap_or(0);
                                let max_fall_code = payload.get(4).copied().unwrap_or(0);
                                let min_luminance_code = payload.get(5).copied().unwrap_or(0);

                                let max_luminance_millinits =
                                    decode_max_luminance_millinits(max_luminance_code);
                                let max_fall_millinits =
                                    decode_max_luminance_millinits(max_fall_code);
                                let ref_max = max_luminance_millinits.unwrap_or(1_000_000);
                                let min_luminance_ten_thousandths =
                                    decode_min_luminance_ten_thousandths(
                                        min_luminance_code,
                                        ref_max,
                                    );

                                hdr_static_metadata = Some(HdrSummary {
                                    eotf,
                                    static_metadata_type1,
                                    max_luminance_code,
                                    max_fall_code,
                                    min_luminance_code,
                                    max_luminance_millinits,
                                    max_fall_millinits,
                                    min_luminance_ten_thousandths,
                                });
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            cursor += len;
        }
    }

    Ok(Cta861Summary {
        revision,
        video_vics,
        has_speaker_allocation,
        colorimetry,
        hdr_static_metadata,
    })
}

/// Validates a raw EDID byte slice safely with zero heap out-of-bounds access.
///
/// Rejects inputs shorter than 128 bytes, non-multiples of 128 bytes, invalid 8-byte
/// EDID headers, mismatched extension block counts, unsupported EDID versions, and bad
/// block checksums.
pub fn validate_custom_edid(bytes: &[u8]) -> Result<EdidSummary, EdidError> {
    if bytes.len() < 128 {
        return Err(EdidError::TooShort { len: bytes.len() });
    }
    if !bytes.len().is_multiple_of(128) {
        return Err(EdidError::InvalidLength { len: bytes.len() });
    }
    if bytes[..8] != EDID_HEADER {
        return Err(EdidError::InvalidHeader);
    }

    let version = bytes[18];
    let revision = bytes[19];
    if version != 1 {
        return Err(EdidError::UnsupportedVersion { version, revision });
    }

    let declared_extensions = bytes[126];
    let actual_blocks = bytes.len() / 128;
    let expected_blocks = 1usize + usize::from(declared_extensions);
    if expected_blocks != actual_blocks {
        return Err(EdidError::ExtensionCountMismatch {
            declared: declared_extensions,
            actual_blocks,
        });
    }

    let (blocks, _) = bytes.as_chunks::<128>();
    for (block_index, block) in blocks.iter().enumerate() {
        if let Err((expected, actual)) = verify_block_checksum(block) {
            return Err(EdidError::InvalidChecksum {
                block_index,
                expected,
                actual,
            });
        }
    }

    let manufacturer = decode_manufacturer([bytes[8], bytes[9]]);
    let product_code = u16::from_le_bytes([bytes[10], bytes[11]]);
    let binary_serial = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    let block0_size_cm = (bytes[21], bytes[22]);
    let srgb_bit_set = (bytes[24] & 0x04) != 0;

    let mut chroma_raw = [0u8; 10];
    chroma_raw.copy_from_slice(&bytes[25..35]);
    let chromaticity = Chromaticity10Bit::decode(&chroma_raw);

    let mut preferred_timing = None;
    let mut dtd_size_mm = None;
    let mut product_name = None;
    let mut ascii_serial = None;

    for desc in bytes[54..126].as_chunks::<18>().0 {
        let pclk_10khz = u16::from_le_bytes([desc[0], desc[1]]);
        if pclk_10khz != 0 {
            if preferred_timing.is_none() {
                let h_active = u16::from(desc[2]) | (u16::from(desc[4] >> 4) << 8);
                let v_active = u16::from(desc[5]) | (u16::from(desc[7] >> 4) << 8);
                let h_mm = u16::from(desc[12]) | (u16::from(desc[14] >> 4) << 8);
                let v_mm = u16::from(desc[13]) | (u16::from(desc[14] & 0x0F) << 8);
                preferred_timing = Some(DetailedTimingSummary {
                    width: h_active,
                    height: v_active,
                    pixel_clock_hz: u32::from(pclk_10khz) * 10_000,
                });
                dtd_size_mm = Some((h_mm, v_mm));
            }
        } else if desc[0] == 0 && desc[1] == 0 && desc[2] == 0 {
            match desc[3] {
                0xFC if product_name.is_none() => {
                    product_name = Some(decode_descriptor_text(&desc[5..18]));
                }
                0xFF if ascii_serial.is_none() => {
                    ascii_serial = Some(decode_descriptor_text(&desc[5..18]));
                }
                _ => {}
            }
        }
    }

    let mut cta861 = None;
    for (idx, block) in blocks.iter().enumerate().skip(1) {
        if block[0] == CTA861_EXTENSION_TAG {
            cta861 = Some(parse_cta861_block(block, idx)?);
            break;
        }
    }

    Ok(EdidSummary {
        version,
        revision,
        manufacturer,
        product_code,
        binary_serial,
        ascii_serial,
        product_name,
        block0_size_cm,
        dtd_size_mm,
        preferred_timing,
        chromaticity,
        srgb_bit_set,
        extension_blocks: declared_extensions,
        cta861,
    })
}
