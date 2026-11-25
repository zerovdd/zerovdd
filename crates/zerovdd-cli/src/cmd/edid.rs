//! Custom EDID inspection command (`zerovdd edid inspect <PATH>`).

use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde::Serialize;
use zerovdd_edid::{validate_custom_edid, EdidSummary};
use zerovdd_proto::types::MAX_CUSTOM_EDID_LEN;

/// Arguments for `zerovdd edid`.
#[derive(Debug, Args)]
pub struct EdidArgs {
    /// EDID utility action to execute.
    #[command(subcommand)]
    pub action: EdidCommand,
}

/// Supported EDID subcommands.
#[derive(Debug, Subcommand)]
pub enum EdidCommand {
    /// Validate and inspect a raw EDID binary file (`128..=512` bytes).
    Inspect(EdidInspectArgs),
}

/// Arguments for `zerovdd edid inspect`.
#[derive(Debug, Args)]
pub struct EdidInspectArgs {
    /// Path to a raw EDID binary file (128..=512 bytes, multiple of 128).
    #[arg(value_name = "PATH")]
    pub path: PathBuf,
    /// Output inspection report as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Serialize)]
struct PreferredTimingJson {
    width: u16,
    height: u16,
    pixel_clock_hz: u32,
}

#[derive(Debug, Serialize)]
struct HdrMetadataJson {
    eotf_raw: u8,
    sdr_gamma: bool,
    hdr_gamma: bool,
    pq_smpte_st2084: bool,
    hlg: bool,
    static_metadata_type1: bool,
    max_luminance_code: u8,
    max_fall_code: u8,
    min_luminance_code: u8,
    max_luminance_millinits: Option<u32>,
    max_fall_millinits: Option<u32>,
    min_luminance_ten_thousandths: Option<u32>,
}

#[derive(Debug, Serialize)]
struct Cta861Json {
    revision: u8,
    video_vics: Vec<u8>,
    has_speaker_allocation: bool,
    colorimetry_raw: Option<u8>,
    colorimetry_bt2020_rgb: bool,
    colorimetry_bt2020_ycc: bool,
    colorimetry_op_rgb: bool,
    hdr_static_metadata: Option<HdrMetadataJson>,
}

#[derive(Debug, Serialize)]
struct ChromaticityJson {
    rx: u16,
    ry: u16,
    gx: u16,
    gy: u16,
    bx: u16,
    by: u16,
    wx: u16,
    wy: u16,
}

#[derive(Debug, Serialize)]
struct EdidInspectOutput {
    file_path: String,
    byte_length: usize,
    block_count: usize,
    checksum_valid: bool,
    version: u8,
    revision: u8,
    manufacturer: String,
    product_code: String,
    binary_serial: u32,
    ascii_serial: Option<String>,
    product_name: Option<String>,
    block0_size_cm: (u8, u8),
    dtd_size_mm: Option<(u16, u16)>,
    preferred_timing: Option<PreferredTimingJson>,
    chromaticity: ChromaticityJson,
    srgb_bit_set: bool,
    extension_blocks: u8,
    cta861: Option<Cta861Json>,
}

fn build_inspect_output(
    path: &std::path::Path,
    bytes: &[u8],
    summary: &EdidSummary,
) -> EdidInspectOutput {
    let manufacturer: String = summary.manufacturer.iter().collect();
    let cta861 = summary.cta861.as_ref().map(|cta| Cta861Json {
        revision: cta.revision,
        video_vics: cta.video_vics.clone(),
        has_speaker_allocation: cta.has_speaker_allocation,
        colorimetry_raw: cta.colorimetry.map(|c| c.bits()),
        colorimetry_bt2020_rgb: cta.colorimetry.is_some_and(|c| c.bits() & 0x80 != 0),
        colorimetry_bt2020_ycc: cta.colorimetry.is_some_and(|c| c.bits() & 0x40 != 0),
        colorimetry_op_rgb: cta.colorimetry.is_some_and(|c| c.bits() & 0x10 != 0),
        hdr_static_metadata: cta.hdr_static_metadata.map(|hdr| HdrMetadataJson {
            eotf_raw: hdr.eotf.bits(),
            sdr_gamma: (hdr.eotf.bits() & 0x01) != 0,
            hdr_gamma: (hdr.eotf.bits() & 0x02) != 0,
            pq_smpte_st2084: (hdr.eotf.bits() & 0x04) != 0,
            hlg: (hdr.eotf.bits() & 0x08) != 0,
            static_metadata_type1: hdr.static_metadata_type1,
            max_luminance_code: hdr.max_luminance_code,
            max_fall_code: hdr.max_fall_code,
            min_luminance_code: hdr.min_luminance_code,
            max_luminance_millinits: hdr.max_luminance_millinits,
            max_fall_millinits: hdr.max_fall_millinits,
            min_luminance_ten_thousandths: hdr.min_luminance_ten_thousandths,
        }),
    });

    EdidInspectOutput {
        file_path: path.display().to_string(),
        byte_length: bytes.len(),
        block_count: bytes.len() / 128,
        checksum_valid: true,
        version: summary.version,
        revision: summary.revision,
        manufacturer,
        product_code: format!("0x{:04X}", summary.product_code),
        binary_serial: summary.binary_serial,
        ascii_serial: summary.ascii_serial.clone(),
        product_name: summary.product_name.clone(),
        block0_size_cm: summary.block0_size_cm,
        dtd_size_mm: summary.dtd_size_mm,
        preferred_timing: summary.preferred_timing.map(|dtd| PreferredTimingJson {
            width: dtd.width,
            height: dtd.height,
            pixel_clock_hz: dtd.pixel_clock_hz,
        }),
        chromaticity: ChromaticityJson {
            rx: summary.chromaticity.rx,
            ry: summary.chromaticity.ry,
            gx: summary.chromaticity.gx,
            gy: summary.chromaticity.gy,
            bx: summary.chromaticity.bx,
            by: summary.chromaticity.by,
            wx: summary.chromaticity.wx,
            wy: summary.chromaticity.wy,
        },
        srgb_bit_set: summary.srgb_bit_set,
        extension_blocks: summary.extension_blocks,
        cta861,
    }
}

/// Reads and strictly validates a custom EDID binary file (`128..=512` bytes, multiple of 128).
pub fn read_and_validate_edid_file(path: &std::path::Path) -> Result<(Vec<u8>, EdidSummary)> {
    let bytes =
        fs::read(path).with_context(|| format!("failed to read EDID file `{}`", path.display()))?;
    if bytes.len() > MAX_CUSTOM_EDID_LEN {
        bail!(
            "EDID file `{}` is {} bytes; maximum supported length is {MAX_CUSTOM_EDID_LEN} bytes (4 blocks)",
            path.display(),
            bytes.len()
        );
    }
    let summary = validate_custom_edid(&bytes)
        .with_context(|| format!("invalid EDID data in `{}`", path.display()))?;
    Ok((bytes, summary))
}

/// Executes `zerovdd edid`.
pub fn execute(args: EdidArgs) -> Result<()> {
    match args.action {
        EdidCommand::Inspect(inspect_args) => run_inspect(inspect_args),
    }
}

fn run_inspect(args: EdidInspectArgs) -> Result<()> {
    let (bytes, summary) = read_and_validate_edid_file(&args.path)?;
    let report = build_inspect_output(&args.path, &bytes, &summary);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("EDID Inspection: {}", report.file_path);
    println!(
        "  Blocks / Size:   {} block(s) ({} bytes), checksums=OK",
        report.block_count, report.byte_length
    );
    println!("  EDID Version:    {}.{}", report.version, report.revision);
    println!(
        "  Manufacturer:    {} (product {}, binary_serial=0x{:08X})",
        report.manufacturer, report.product_code, report.binary_serial
    );
    if let Some(ref name) = report.product_name {
        println!("  Product Name:    {name}");
    }
    if let Some(ref serial) = report.ascii_serial {
        println!("  ASCII Serial:    {serial}");
    }
    println!(
        "  Physical Size:   {}x{} cm (Block 0), {} (DTD mm)",
        report.block0_size_cm.0,
        report.block0_size_cm.1,
        report
            .dtd_size_mm
            .map_or_else(|| "none".to_string(), |(w, h)| format!("{w}x{h} mm"))
    );
    if let Some(ref dtd) = report.preferred_timing {
        println!(
            "  Preferred Timing: {}x{} (pclk={} Hz)",
            dtd.width, dtd.height, dtd.pixel_clock_hz
        );
    }
    println!(
        "  Chromaticity:    R=({},{}) G=({},{}) B=({},{}) W=({},{}) sRGB={}",
        report.chromaticity.rx,
        report.chromaticity.ry,
        report.chromaticity.gx,
        report.chromaticity.gy,
        report.chromaticity.bx,
        report.chromaticity.by,
        report.chromaticity.wx,
        report.chromaticity.wy,
        report.srgb_bit_set
    );
    if let Some(ref cta) = report.cta861 {
        println!(
            "  CTA-861 Block 1: rev={}, VICs={:?}, speaker_alloc={}, colorimetry={}",
            cta.revision,
            cta.video_vics,
            cta.has_speaker_allocation,
            cta.colorimetry_raw
                .map_or_else(|| "none".to_string(), |v| format!("0x{v:02X}"))
        );
        if let Some(ref hdr) = cta.hdr_static_metadata {
            println!(
                "  HDR10 Metadata:  PQ={} HLG={} type1={} max_nits={:?} mNits max_fall={:?} mNits min={:?} (1e-4 nits)",
                hdr.pq_smpte_st2084,
                hdr.hlg,
                hdr.static_metadata_type1,
                hdr.max_luminance_millinits,
                hdr.max_fall_millinits,
                hdr.min_luminance_ten_thousandths
            );
        }
    }
    Ok(())
}
