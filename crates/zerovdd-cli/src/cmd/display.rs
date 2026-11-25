//! Virtual display management subcommands (`zerovdd display`).

use anyhow::{bail, Result};
use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use uuid::Uuid;
use zerovdd_client::gpu::GpuSelector;
use zerovdd_client::monitor_guid_for_name;
use zerovdd_proto::ipc::{
    BitDepth, ColorSpace, DisplayAddRequest, DisplayOrientation, DisplayRemoveRequest,
    DisplaySetModeRequest, IpcEnvelope, IpcRequest, IpcResponse, SwitchGpuRequest, TopologyMode,
};
use zerovdd_proto::types::{BitDepthPreference, ColorSpacePreference, HdrMasteringMetadata};
use zerovdd_proto::validation::{
    validate_color_and_bit_depth, validate_dimensions, validate_hdr_metadata,
    validate_refresh_rate, validate_target_ppi, MAX_LEASE_TTL_SECS,
};

/// Arguments for `zerovdd display`.
#[derive(Debug, Args)]
pub struct DisplayArgs {
    /// Display management action to execute.
    #[command(subcommand)]
    pub action: DisplayCommand,
}

/// Supported virtual display subcommands.
#[derive(Debug, Subcommand)]
pub enum DisplayCommand {
    /// Add a virtual display at `<WxH@Hz>` (delegates to zerovdd-svc if running, or requires `--hold` / `--lease` standalone).
    Add(DisplayAddArgs),
    /// Remove an active virtual display by `--guid` or `--target-id`.
    Remove(DisplayRemoveArgs),
    /// List active virtual displays exposed by the driver.
    List(DisplayListArgs),
    /// Update resolution, refresh rate, or HDR state on an active virtual display in place.
    SetMode(DisplaySetModeArgs),
    /// Switch the render GPU adapter used by the virtual display driver.
    SwitchGpu(SwitchGpuArgs),
    /// Depart active virtual displays and restore physical display topology.
    Restore(DisplayRestoreArgs),
}

/// CLI color gamut selector (`--color-space`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorSpaceArg {
    /// Automatic (`bt709` for SDR, `bt2020` for HDR10).
    #[value(name = "auto")]
    Auto,
    /// ITU-R BT.709 / sRGB primaries (SDR).
    #[value(name = "bt709")]
    Bt709,
    /// ITU-R BT.2020 wide color gamut primaries (HDR10).
    #[value(name = "bt2020")]
    Bt2020,
    /// Display P3 / DCI-P3 primaries (HDR10).
    #[value(name = "dci-p3", alias = "dci_p3")]
    DciP3,
}

impl From<ColorSpaceArg> for ColorSpace {
    fn from(value: ColorSpaceArg) -> Self {
        match value {
            ColorSpaceArg::Auto => Self::Auto,
            ColorSpaceArg::Bt709 => Self::Bt709,
            ColorSpaceArg::Bt2020 => Self::Bt2020,
            ColorSpaceArg::DciP3 => Self::DciP3,
        }
    }
}

impl From<ColorSpaceArg> for ColorSpacePreference {
    fn from(value: ColorSpaceArg) -> Self {
        ColorSpace::from(value).into()
    }
}

/// CLI color bit-depth selector (`--bpc`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BitDepthArg {
    /// Automatic defaults (`8|10` bpc for SDR, `10|12` bpc for HDR10).
    #[value(name = "auto")]
    Auto,
    /// Restrict output modes to 8 bpc (SDR).
    #[value(name = "8", alias = "bpc8")]
    Bpc8,
    /// Restrict output modes to 10 bpc.
    #[value(name = "10", alias = "bpc10")]
    Bpc10,
    /// Restrict output modes to 12 bpc.
    #[value(name = "12", alias = "bpc12")]
    Bpc12,
}

impl From<BitDepthArg> for BitDepth {
    fn from(value: BitDepthArg) -> Self {
        match value {
            BitDepthArg::Auto => Self::Auto,
            BitDepthArg::Bpc8 => Self::Bpc8,
            BitDepthArg::Bpc10 => Self::Bpc10,
            BitDepthArg::Bpc12 => Self::Bpc12,
        }
    }
}

impl From<BitDepthArg> for BitDepthPreference {
    fn from(value: BitDepthArg) -> Self {
        BitDepth::from(value).into()
    }
}

/// Arguments for `zerovdd display add`.
#[derive(Debug, Args)]
pub struct DisplayAddArgs {
    /// Target mode formatted as `<Width>x<Height>@<Hz>` (for example `3840x2160@59.94` or `2560x1600@90`).
    #[arg(value_name = "WxH@Hz")]
    pub mode: String,
    /// Enable HDR10 (SMPTE ST 2084 PQ / BT.2020) profile and state.
    #[arg(long)]
    pub hdr: bool,
    /// Explicit color space gamut (`auto`, `bt709`, `bt2020`, `dci-p3`).
    #[arg(long, value_enum, value_name = "SPACE")]
    pub color_space: Option<ColorSpaceArg>,
    /// Explicit bit-depth cap per color component (`auto`, `8`, `10`, `12`).
    #[arg(long, value_enum, value_name = "BPC")]
    pub bpc: Option<BitDepthArg>,
    /// Explicit target pixel density (`48..=600` PPI) for synthesized EDID physical sizing.
    #[arg(long, value_name = "PPI")]
    pub ppi: Option<u16>,
    /// Override peak HDR10 mastering luminance in nits (`100..=10000`).
    #[arg(long, value_name = "NITS")]
    pub max_nits: Option<u16>,
    /// Override HDR10 maximum frame-average light level in nits.
    #[arg(long, value_name = "NITS")]
    pub max_fall: Option<u16>,
    /// Override minimum mastering luminance in `0.0001`-nit units (e.g. `500` = `0.05` nits).
    #[arg(long, value_name = "VAL")]
    pub min_nits_10k: Option<u32>,
    /// Optional path to a raw custom EDID binary file (`128..=512` bytes, multiple of 128).
    #[arg(long, value_name = "PATH")]
    pub edid: Option<std::path::PathBuf>,
    /// Logical display/client name used for EDID label and deterministic GUID derivation.
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
    /// Explicit monitor container UUID (overrides GUID derived from `--name`).
    #[arg(long, value_name = "UUID")]
    pub guid: Option<Uuid>,
    /// Render GPU selector (`auto`, `luid:<low>:<high>`, `pci:<bus>:<dev>.<fn>`, or friendly name).
    #[arg(long, value_name = "SELECTOR")]
    pub gpu: Option<String>,
    /// Hold the Control Device handle open until Ctrl+C / termination (standalone mode).
    #[arg(long, conflicts_with = "lease")]
    pub hold: bool,
    /// Create a leased display that persists for `<SECS>` seconds after exit (standalone mode or service TTL).
    #[arg(long, value_name = "SECS", conflicts_with = "hold")]
    pub lease: Option<u32>,
    /// Disable physical monitors and set the virtual display as sole primary at `(0, 0)`.
    #[arg(long, conflicts_with = "extend")]
    pub isolated: bool,
    /// Extend the active desktop alongside physical monitors.
    #[arg(long, conflicts_with = "isolated")]
    pub extend: bool,
    /// Clockwise display orientation (`0`, `90`, `180`, `270`).
    #[arg(long, value_name = "DEG")]
    pub orientation: Option<u16>,
    /// Explicit desktop origin `X,Y` when extending (supports negative coordinates).
    #[arg(long, value_name = "X,Y", allow_hyphen_values = true)]
    pub position: Option<String>,
    /// Set the virtual display as primary `(0, 0)` even when extending.
    #[arg(long)]
    pub primary: bool,
    /// Output result as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zerovdd display remove`.
#[derive(Debug, Args)]
pub struct DisplayRemoveArgs {
    /// Monitor container UUID of the virtual display to remove.
    #[arg(
        long,
        value_name = "UUID",
        conflicts_with = "target_id",
        required_unless_present = "target_id"
    )]
    pub guid: Option<Uuid>,
    /// OS VidPN target identifier of the virtual display to remove.
    #[arg(
        long,
        value_name = "ID",
        conflicts_with = "guid",
        required_unless_present = "guid"
    )]
    pub target_id: Option<u32>,
    /// Output result as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zerovdd display list`.
#[derive(Debug, Args)]
pub struct DisplayListArgs {
    /// Output active displays as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zerovdd display set-mode`.
#[derive(Debug, Args)]
pub struct DisplaySetModeArgs {
    /// Target mode formatted as `<Width>x<Height>@<Hz>` (for example `3024x1890@60` or `2560x1440@23.976`).
    #[arg(value_name = "WxH@Hz")]
    pub mode: String,
    /// Logical display/client name matching the active virtual display.
    #[arg(long, value_name = "NAME", conflicts_with_all = ["guid", "target_id"])]
    pub name: Option<String>,
    /// Monitor container UUID of the active virtual display.
    #[arg(long, value_name = "UUID", conflicts_with_all = ["name", "target_id"])]
    pub guid: Option<Uuid>,
    /// OS VidPN target identifier of the active virtual display.
    #[arg(long, value_name = "ID", conflicts_with_all = ["name", "guid"])]
    pub target_id: Option<u32>,
    /// Enable HDR10 on the virtual display.
    #[arg(long, conflicts_with = "sdr")]
    pub hdr: bool,
    /// Switch the virtual display to SDR (disables HDR).
    #[arg(long, conflicts_with = "hdr")]
    pub sdr: bool,
    /// Explicit color space gamut (`auto`, `bt709`, `bt2020`, `dci-p3`).
    #[arg(long, value_enum, value_name = "SPACE")]
    pub color_space: Option<ColorSpaceArg>,
    /// Explicit bit-depth cap per color component (`auto`, `8`, `10`, `12`).
    #[arg(long, value_enum, value_name = "BPC")]
    pub bpc: Option<BitDepthArg>,
    /// Override peak HDR10 mastering luminance in nits (`100..=10000`).
    #[arg(long, value_name = "NITS")]
    pub max_nits: Option<u16>,
    /// Override HDR10 maximum frame-average light level in nits.
    #[arg(long, value_name = "NITS")]
    pub max_fall: Option<u16>,
    /// Override minimum mastering luminance in `0.0001`-nit units (e.g. `500` = `0.05` nits).
    #[arg(long, value_name = "VAL")]
    pub min_nits_10k: Option<u32>,
    /// Output result as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zerovdd display switch-gpu`.
#[derive(Debug, Args)]
pub struct SwitchGpuArgs {
    /// Render GPU selector (`auto`, `luid:<low>:<high>`, `pci:<bus>:<dev>.<fn>`, or friendly name).
    #[arg(value_name = "SELECTOR")]
    pub selector: String,
    /// Output result as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zerovdd display restore`.
#[derive(Debug, Args)]
pub struct DisplayRestoreArgs {
    /// Output restoration summary as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Parsed `<WxH@Hz>` display mode specification with exact rational refresh rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ModeSpec {
    /// Active horizontal resolution in pixels.
    pub width: u32,
    /// Active vertical resolution in pixels.
    pub height: u32,
    /// Exact rational refresh numerator.
    pub refresh_num: u32,
    /// Exact rational refresh denominator.
    pub refresh_den: u32,
    /// Rounded refresh rate in millihertz.
    pub refresh_mhz: u32,
}

const fn gcd_u32(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

/// Parses `<Width>x<Height>@<Hz>` supporting integer and fractional refresh rates (e.g. `59.94`, `119.88`).
pub fn parse_mode_spec(input: &str) -> Result<ModeSpec> {
    let trimmed = input.trim();
    let Some((dims_str, hz_raw)) = trimmed.split_once('@') else {
        bail!("invalid mode `{input}`: expected <Width>x<Height>@<Hz> (e.g. 3840x2160@59.94)");
    };
    if hz_raw.contains('@') {
        bail!("invalid mode `{input}`: multiple `@` separators");
    }

    let Some((w_str, h_str)) = dims_str.split_once(['x', 'X']) else {
        bail!("invalid mode `{input}`: expected <Width>x<Height> before `@`");
    };
    if h_str.contains(['x', 'X']) {
        bail!("invalid mode `{input}`: multiple `x` separators");
    }

    let width: u32 = w_str
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid width `{w_str}` in `{input}`"))?;
    let height: u32 = h_str
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid height `{h_str}` in `{input}`"))?;
    validate_dimensions(width, height)?;

    let mut hz_str = hz_raw.trim();
    if hz_str.len() >= 2 && hz_str.as_bytes()[hz_str.len() - 2..].eq_ignore_ascii_case(b"hz") {
        hz_str = hz_str[..hz_str.len() - 2].trim();
    }
    if hz_str.is_empty() {
        bail!("missing refresh rate in `{input}`");
    }

    let refresh_mhz = parse_hz_to_millihz(hz_str, input)?;
    let (refresh_num, refresh_den) = millihz_to_rational(refresh_mhz);
    validate_refresh_rate(refresh_num, refresh_den)?;

    Ok(ModeSpec {
        width,
        height,
        refresh_num,
        refresh_den,
        refresh_mhz,
    })
}

pub(crate) fn parse_hz_to_millihz(hz_str: &str, original: &str) -> Result<u32> {
    let (int_part, frac_part) = match hz_str.split_once('.') {
        Some((i, f)) => {
            if f.contains('.') || i.is_empty() || f.is_empty() {
                bail!("invalid refresh rate `{hz_str}` in `{original}`");
            }
            (i, Some(f))
        }
        None => (hz_str, None),
    };

    if !int_part.bytes().all(|b| b.is_ascii_digit()) {
        bail!("invalid refresh rate `{hz_str}` in `{original}`");
    }
    let whole: u32 = int_part
        .parse()
        .map_err(|_| anyhow::anyhow!("refresh rate `{hz_str}` overflows u32"))?;

    let frac_mhz: u32 = match frac_part {
        None => 0,
        Some(f) => {
            if !f.bytes().all(|b| b.is_ascii_digit()) {
                bail!("invalid fractional refresh rate `{hz_str}` in `{original}`");
            }
            if f.len() > 3 {
                bail!("refresh rate `{hz_str}` supports at most 3 fractional digits");
            }
            let raw: u32 = f
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid fractional refresh rate `{hz_str}`"))?;
            match f.len() {
                1 => raw * 100,
                2 => raw * 10,
                3 => raw,
                _ => unreachable!(),
            }
        }
    };

    whole
        .checked_mul(1000)
        .and_then(|v| v.checked_add(frac_mhz))
        .ok_or_else(|| anyhow::anyhow!("refresh rate `{hz_str}` overflows millihertz"))
}

fn millihz_to_rational(millihz: u32) -> (u32, u32) {
    match millihz {
        23_976 => (24_000, 1001),
        29_970 => (30_000, 1001),
        47_952 => (48_000, 1001),
        59_940 => (60_000, 1001),
        119_880 => (120_000, 1001),
        239_760 => (240_000, 1001),
        other => {
            let g = gcd_u32(other, 1000);
            (other / g, 1000 / g)
        }
    }
}

/// Parses a `--gpu` selector string (`auto`, `luid:<low>:<high>`, `pci:<bus>[:<dev>.<fn>]`, or friendly name).
pub fn parse_gpu_selector(raw: &str) -> Result<GpuSelector> {
    Ok(zerovdd_client::gpu::parse_gpu_selector(raw)?)
}

/// Validates `--lease` bounds regardless of backend.
pub fn validate_lease_ttl(lease: Option<u32>) -> Result<()> {
    if let Some(secs) = lease {
        if secs == 0 || secs > MAX_LEASE_TTL_SECS {
            bail!("--lease must be between 1 and {MAX_LEASE_TTL_SECS} seconds (got {secs})");
        }
    }
    Ok(())
}

/// Validates standalone lifetime requirements (`--hold` or `--lease <secs>`) when `zerovdd-svc` is offline.
pub fn validate_add_lifetime(
    service_reachable: bool,
    hold: bool,
    lease: Option<u32>,
) -> Result<()> {
    validate_lease_ttl(lease)?;
    if !service_reachable && !hold && lease.is_none() {
        bail!(
            "zerovdd-svc is not reachable; standalone `display add` requires either `--hold` \
             (keeps the virtual display alive until Ctrl+C) or `--lease <secs>`"
        );
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct DisplayAddOutput {
    backend: &'static str,
    client_id: String,
    monitor_guid: String,
    adapter_luid_low: u32,
    adapter_luid_high: i32,
    target_id: u32,
    connector_index: u32,
    width: u32,
    height: u32,
    refresh_num: u32,
    refresh_den: u32,
    hdr: bool,
    ownership: &'static str,
    lease_secs: Option<u32>,
}

/// Executes `zerovdd display`.
pub fn execute(args: DisplayArgs) -> Result<()> {
    match args.action {
        DisplayCommand::Add(add_args) => run_add(add_args),
        DisplayCommand::Remove(rem_args) => run_remove(rem_args),
        DisplayCommand::List(list_args) => run_list(list_args),
        DisplayCommand::SetMode(mode_args) => run_set_mode(mode_args),
        DisplayCommand::SwitchGpu(gpu_args) => run_switch_gpu(gpu_args),
        DisplayCommand::Restore(restore_args) => run_restore(restore_args),
    }
}

fn parse_position_xy(raw: &str) -> Result<(i32, i32)> {
    let trimmed = raw.trim();
    let Some((x_str, y_str)) = trimmed.split_once(',') else {
        bail!("invalid --position `{raw}`: expected <X>,<Y> (e.g. -1080,0)");
    };
    if y_str.contains(',') {
        bail!("invalid --position `{raw}`: multiple commas");
    }
    let x: i32 = x_str
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid X coordinate `{x_str}` in `--position {raw}`"))?;
    let y: i32 = y_str
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid Y coordinate `{y_str}` in `--position {raw}`"))?;
    Ok((x, y))
}

fn parse_orientation_deg(deg: u16) -> Result<DisplayOrientation> {
    DisplayOrientation::from_degrees(deg)
        .ok_or_else(|| anyhow::anyhow!("invalid --orientation {deg}: expected 0, 90, 180, or 270"))
}

fn resolve_display_identity(name: Option<&str>, guid: Option<Uuid>) -> (String, [u8; 16], Uuid) {
    match (name, guid) {
        (Some(n), Some(g)) => (n.to_string(), *g.as_bytes(), g),
        (Some(n), None) => {
            let bytes = monitor_guid_for_name(n);
            (n.to_string(), bytes, Uuid::from_bytes(bytes))
        }
        (None, Some(g)) => (g.hyphenated().to_string(), *g.as_bytes(), g),
        (None, None) => {
            let default_name = "zerovdd-cli";
            let bytes = monitor_guid_for_name(default_name);
            (default_name.to_string(), bytes, Uuid::from_bytes(bytes))
        }
    }
}

fn resolve_explicit_hdr_metadata(
    hdr_enabled: bool,
    max_nits: Option<u16>,
    max_fall: Option<u16>,
    min_nits_10k: Option<u32>,
) -> Result<HdrMasteringMetadata> {
    if !hdr_enabled && (max_nits.is_some() || max_fall.is_some() || min_nits_10k.is_some()) {
        bail!("--max-nits, --max-fall, and --min-nits-10k require HDR10 (`--hdr`)");
    }
    if !hdr_enabled {
        return Ok(HdrMasteringMetadata::default());
    }
    let defaults = HdrMasteringMetadata::DEFAULT_HDR10;
    let eff_max = max_nits.unwrap_or(defaults.max_luminance_nits);
    let eff_fall = max_fall.unwrap_or(defaults.max_fall_nits.min(eff_max));
    let eff_min = min_nits_10k.unwrap_or(defaults.min_luminance_ten_thousandths);
    let meta = HdrMasteringMetadata {
        max_luminance_nits: eff_max,
        max_fall_nits: eff_fall,
        min_luminance_ten_thousandths: eff_min,
    };
    validate_hdr_metadata(meta)?;
    Ok(meta)
}

fn run_add(args: DisplayAddArgs) -> Result<()> {
    use zerovdd_proto::types::HdrProfile;

    let mode = parse_mode_spec(&args.mode)?;
    validate_lease_ttl(args.lease)?;
    if let Some(ppi) = args.ppi {
        if ppi == 0 {
            bail!("--ppi must be between 48 and 600 (got 0)");
        }
        validate_target_ppi(ppi)?;
    }
    let hdr_profile = if args.hdr {
        HdrProfile::Hdr10
    } else {
        HdrProfile::Sdr
    };
    let color_pref: ColorSpacePreference = args.color_space.map(Into::into).unwrap_or_default();
    let bit_pref: BitDepthPreference = args.bpc.map(Into::into).unwrap_or_default();
    validate_color_and_bit_depth(hdr_profile, color_pref, bit_pref)?;
    let _ =
        resolve_explicit_hdr_metadata(args.hdr, args.max_nits, args.max_fall, args.min_nits_10k)?;
    let custom_edid = match args.edid {
        Some(ref path) => Some(crate::cmd::edid::read_and_validate_edid_file(path)?.0),
        None => None,
    };

    let (client_id, monitor_guid, parsed_guid) =
        resolve_display_identity(args.name.as_deref(), args.guid);
    let gpu_selector = args.gpu.as_deref().map(parse_gpu_selector).transpose()?;
    let orientation = args.orientation.map(parse_orientation_deg).transpose()?;
    let position = args
        .position
        .as_deref()
        .map(parse_position_xy)
        .transpose()?;

    if !args.hold {
        let topology_mode = if args.isolated {
            Some(TopologyMode::Isolate)
        } else if args.extend {
            Some(TopologyMode::Extend)
        } else {
            None
        };
        let req = IpcRequest::DisplayAdd(DisplayAddRequest {
            client_id: client_id.clone(),
            width: mode.width,
            height: mode.height,
            refresh_mhz: mode.refresh_mhz,
            hdr: args.hdr,
            lease_ttl_secs: args.lease,
            topology_mode,
            target_ppi: args.ppi,
            color_space: args.color_space.map(Into::into),
            bit_depth: args.bpc.map(Into::into),
            max_luminance_nits: args.max_nits,
            max_fall_nits: args.max_fall,
            min_luminance_ten_thousandths: args.min_nits_10k,
            custom_edid: custom_edid.clone(),
            guid: args.guid.map(|g| g.hyphenated().to_string()),
            gpu: args.gpu.clone(),
            orientation,
            position,
            make_primary: args.primary.then_some(true),
        });
        if let Some(resp) = try_send_ipc_request(&req)? {
            return handle_service_add_response(resp, &args, &mode, &client_id, parsed_guid);
        }

        validate_add_lifetime(false, args.hold, args.lease)?;
    }

    run_standalone_add(
        &args,
        &mode,
        &client_id,
        monitor_guid,
        parsed_guid,
        gpu_selector.as_ref(),
        custom_edid.as_deref(),
        orientation,
        position,
    )
}

fn handle_service_add_response(
    resp: IpcResponse,
    args: &DisplayAddArgs,
    mode: &ModeSpec,
    client_id: &str,
    parsed_guid: Uuid,
) -> Result<()> {
    match resp {
        IpcResponse::DisplayAdded {
            adapter_luid_low,
            adapter_luid_high,
            target_id,
            connector_index,
        } => {
            let out = DisplayAddOutput {
                backend: "zerovdd-svc",
                client_id: client_id.to_string(),
                monitor_guid: parsed_guid.hyphenated().to_string(),
                adapter_luid_low,
                adapter_luid_high,
                target_id,
                connector_index,
                width: mode.width,
                height: mode.height,
                refresh_num: mode.refresh_num,
                refresh_den: mode.refresh_den,
                hdr: args.hdr,
                ownership: if args.lease.is_some() {
                    "leased"
                } else {
                    "service"
                },
                lease_secs: args.lease,
            };
            print_add_output(&out, args.json)
        }
        IpcResponse::Error(err) => {
            bail!(
                "zerovdd-svc rejected DisplayAdd [{}]: {}",
                err.code,
                err.message
            )
        }
        other => bail!("unexpected IPC response from zerovdd-svc: {other:?}"),
    }
}

fn print_add_output(out: &DisplayAddOutput, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(out)?);
    } else {
        println!(
            "Virtual display active ({}) via {}:",
            out.ownership, out.backend
        );
        println!("  Client ID:     {}", out.client_id);
        println!("  Monitor GUID:  {}", out.monitor_guid);
        println!(
            "  Adapter LUID:  {:#x}:{:#x}",
            out.adapter_luid_low, out.adapter_luid_high
        );
        println!("  Target ID:     {}", out.target_id);
        println!("  Connector:     {}", out.connector_index);
        println!(
            "  Mode:          {}x{}@{}/{} Hz (HDR={})",
            out.width, out.height, out.refresh_num, out.refresh_den, out.hdr
        );
        if let Some(secs) = out.lease_secs {
            println!("  Lease TTL:     {secs}s");
        }
    }
    Ok(())
}

#[cfg(windows)]
#[allow(
    clippy::too_many_arguments,
    reason = "standalone display creation forwards resolved identity, mode, EDID, GPU, and layout options"
)]
fn run_standalone_add(
    args: &DisplayAddArgs,
    mode: &ModeSpec,
    client_id: &str,
    monitor_guid: [u8; 16],
    parsed_guid: Uuid,
    gpu_selector: Option<&GpuSelector>,
    custom_edid: Option<&[u8]>,
    orientation: Option<DisplayOrientation>,
    position: Option<(i32, i32)>,
) -> Result<()> {
    use zerovdd_client::ccd::{
        apply_streaming_session, restore_streaming_session, SessionApplyOptions, WindowsCcdBackend,
    };
    use zerovdd_client::device::ControlDevice;
    use zerovdd_client::gpu::{enumerate_dxgi_adapters, select_gpu};
    use zerovdd_client::topology::{AdapterLuid, DisplayRotation, StreamingTopologyRequest};
    use zerovdd_config::SunshineTopologyMode;
    use zerovdd_proto::types::{
        AddDisplayRequest, HdrProfile, OwnershipMode, RemoveDisplayRequest, SetRenderAdapterRequest,
    };

    let dev = ControlDevice::open()?;

    if let Some(selector) = gpu_selector {
        let adapters = enumerate_dxgi_adapters()?;
        let chosen = select_gpu(&adapters, selector)?;
        let gpu_req =
            SetRenderAdapterRequest::new(chosen.adapter_luid_low, chosen.adapter_luid_high);
        dev.set_render_adapter(&gpu_req)?;
    }

    let ownership = if args.lease.is_some() {
        OwnershipMode::Leased
    } else {
        OwnershipMode::HandleBound
    };
    let hdr_profile = if args.hdr {
        HdrProfile::Hdr10
    } else {
        HdrProfile::Sdr
    };

    let mut req = AddDisplayRequest::new(
        monitor_guid,
        mode.width,
        mode.height,
        mode.refresh_num,
        mode.refresh_den,
        hdr_profile,
        ownership,
    );
    if let Some(secs) = args.lease {
        req.lease_ttl_secs = secs;
    }
    req.target_ppi = args.ppi.unwrap_or(0);
    req.color_space = args
        .color_space
        .map(ColorSpacePreference::from)
        .unwrap_or_default()
        .as_u8();
    req.bit_depth = args
        .bpc
        .map(BitDepthPreference::from)
        .unwrap_or_default()
        .as_u8();
    req.hdr_metadata =
        resolve_explicit_hdr_metadata(args.hdr, args.max_nits, args.max_fall, args.min_nits_10k)?;
    if let Some(edid_bytes) = custom_edid {
        req.set_custom_edid(edid_bytes)?;
    }
    req.set_client_name(client_id)?;

    struct TopologyRestoreGuard {
        snapshot_path: Option<std::path::PathBuf>,
        _temp_dir: Option<tempfile::TempDir>,
        virtual_target: (AdapterLuid, u32),
    }

    impl Drop for TopologyRestoreGuard {
        fn drop(&mut self) {
            if let Some(ref snap_path) = self.snapshot_path {
                let ccd = WindowsCcdBackend::new();
                let _ = restore_streaming_session(&ccd, snap_path, Some(self.virtual_target));
            }
        }
    }

    let resp = dev.add_display(&req)?;
    let virtual_luid = AdapterLuid::new(resp.adapter_luid_low, resp.adapter_luid_high);

    let mut restore_guard = TopologyRestoreGuard {
        snapshot_path: None,
        _temp_dir: None,
        virtual_target: (virtual_luid, resp.target_id),
    };

    let apply_layout =
        args.isolated || args.extend || orientation.is_some() || position.is_some() || args.primary;
    if apply_layout {
        let ccd = WindowsCcdBackend::new();
        let temp_dir = tempfile::Builder::new()
            .prefix("zerovdd-cli-topo-")
            .tempdir()?;
        let snap_path = temp_dir.path().join("topology-backup.json");
        let rotation = match orientation.unwrap_or_default() {
            DisplayOrientation::Identity => DisplayRotation::Identity,
            DisplayOrientation::Rotate90 => DisplayRotation::Rotate90,
            DisplayOrientation::Rotate180 => DisplayRotation::Rotate180,
            DisplayOrientation::Rotate270 => DisplayRotation::Rotate270,
        };
        let topo_req = StreamingTopologyRequest {
            mode: if args.isolated {
                SunshineTopologyMode::Isolate
            } else {
                SunshineTopologyMode::Extend
            },
            width: mode.width,
            height: mode.height,
            refresh_num: mode.refresh_num,
            refresh_den: mode.refresh_den,
            hdr_enabled: args.hdr,
            extend_origin: position,
            make_virtual_primary: args.isolated || args.primary,
            rotation,
        };
        let opts = SessionApplyOptions {
            snapshot_path: if args.hold {
                Some(snap_path.as_path())
            } else {
                None
            },
            created_at_unix_ms: unix_time_ms(),
            ..Default::default()
        };
        if let Err(err) =
            apply_streaming_session(&ccd, virtual_luid, resp.target_id, &topo_req, &opts)
        {
            // WHY: Roll back the newly added display so failed CCD application never leaves an orphan.
            let _ = dev.remove_display(&RemoveDisplayRequest::by_guid(monitor_guid));
            return Err(err.into());
        }
        if args.hold {
            restore_guard.snapshot_path = Some(snap_path);
            restore_guard._temp_dir = Some(temp_dir);
        }
    }

    let out = DisplayAddOutput {
        backend: "direct-ioctl",
        client_id: client_id.to_string(),
        monitor_guid: parsed_guid.hyphenated().to_string(),
        adapter_luid_low: resp.adapter_luid_low,
        adapter_luid_high: resp.adapter_luid_high,
        target_id: resp.target_id,
        connector_index: resp.connector_index,
        width: mode.width,
        height: mode.height,
        refresh_num: mode.refresh_num,
        refresh_den: mode.refresh_den,
        hdr: args.hdr,
        ownership: if args.lease.is_some() {
            "leased"
        } else {
            "handle-bound"
        },
        lease_secs: args.lease,
    };
    print_add_output(&out, args.json)?;

    if args.hold {
        if !args.json {
            println!("Holding virtual display open. Press Ctrl+C to depart and exit...");
        }
        wait_for_ctrl_c()?;
    }

    drop(restore_guard);
    drop(dev);

    Ok(())
}

#[cfg(not(windows))]
fn run_standalone_add(
    _args: &DisplayAddArgs,
    _mode: &ModeSpec,
    _client_id: &str,
    _monitor_guid: [u8; 16],
    _parsed_guid: Uuid,
    _gpu_selector: Option<&GpuSelector>,
    _custom_edid: Option<&[u8]>,
    _orientation: Option<DisplayOrientation>,
    _position: Option<(i32, i32)>,
) -> Result<()> {
    bail!("zerovdd display add is only supported on Windows")
}

#[cfg(windows)]
fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(windows)]
fn wait_for_ctrl_c() -> Result<()> {
    use core::sync::atomic::{AtomicIsize, Ordering};
    use windows::core::BOOL;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
    };
    use windows::Win32::System::Threading::{
        CreateEventW, SetEvent, WaitForSingleObject, INFINITE,
    };

    static STOP_EVENT_HANDLE: AtomicIsize = AtomicIsize::new(0);

    // SAFETY: Matches Windows `PHANDLER_ROUTINE` (`extern "system" fn(u32) -> BOOL`) ABI;
    // only reads an atomic handle and signals the event via async-safe `SetEvent`.
    unsafe extern "system" fn ctrl_handler(ctrl_type: u32) -> BOOL {
        if matches!(
            ctrl_type,
            CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT
        ) {
            let raw = STOP_EVENT_HANDLE.load(Ordering::Acquire);
            if raw != 0 {
                // SAFETY: `raw` is a valid manual-reset event handle created below and still open.
                unsafe {
                    let _ = SetEvent(HANDLE(raw as *mut core::ffi::c_void));
                }
            }
            return BOOL(1);
        }
        BOOL(0)
    }

    struct CtrlHandlerGuard {
        event: HANDLE,
        handler_installed: bool,
    }

    impl Drop for CtrlHandlerGuard {
        fn drop(&mut self) {
            if self.handler_installed {
                // SAFETY: Deregisters the previously registered console control handler.
                unsafe {
                    let _ = SetConsoleCtrlHandler(Some(ctrl_handler), false);
                }
            }
            STOP_EVENT_HANDLE.store(0, Ordering::Release);
            if !self.event.is_invalid() {
                // SAFETY: `self.event` was created by `CreateEventW` and is closed once on drop.
                unsafe {
                    let _ = CloseHandle(self.event);
                }
            }
        }
    }

    // SAFETY: Creates an unnamed manual-reset Win32 event for Ctrl+C wakeup.
    let event = unsafe { CreateEventW(None, true, false, None) }?;
    let mut guard = CtrlHandlerGuard {
        event,
        handler_installed: false,
    };
    STOP_EVENT_HANDLE.store(guard.event.0 as isize, Ordering::Release);

    // SAFETY: `ctrl_handler` matches the `PHANDLER_ROUTINE` ABI and remains valid until deregistered.
    unsafe { SetConsoleCtrlHandler(Some(ctrl_handler), true) }?;
    guard.handler_installed = true;

    // SAFETY: `guard.event` is a valid Win32 event handle waited on until console termination signal.
    let _ = unsafe { WaitForSingleObject(guard.event, INFINITE) };
    Ok(())
}

fn run_remove(args: DisplayRemoveArgs) -> Result<()> {
    if let Some(target_id) = args.target_id {
        if target_id == 0 {
            bail!("--target-id must be non-zero");
        }
    }

    let req = IpcRequest::DisplayRemove(DisplayRemoveRequest {
        client_id: args.guid.map(|g| g.hyphenated().to_string()),
        target_id: args.target_id,
    });
    if let Some(resp) = try_send_ipc_request(&req)? {
        match resp {
            IpcResponse::DisplayRemoved { removed } => {
                if removed || args.guid.is_none() {
                    if args.json {
                        println!(
                            "{}",
                            serde_json::json!({ "backend": "zerovdd-svc", "removed": removed })
                        );
                    } else {
                        println!("Display remove via zerovdd-svc: removed={removed}");
                    }
                    return Ok(());
                }
            }
            IpcResponse::Error(err) => {
                bail!(
                    "zerovdd-svc rejected DisplayRemove [{}]: {}",
                    err.code,
                    err.message
                );
            }
            other => bail!("unexpected IPC response from zerovdd-svc: {other:?}"),
        }
    }

    #[cfg(windows)]
    {
        use zerovdd_client::device::ControlDevice;
        use zerovdd_proto::types::RemoveDisplayRequest;

        let dev = ControlDevice::open()?;
        let req = match (args.guid, args.target_id) {
            (Some(guid), None) => RemoveDisplayRequest::by_guid(*guid.as_bytes()),
            (None, Some(target_id)) => RemoveDisplayRequest::by_target_id(target_id),
            _ => bail!("specify exactly one of --guid or --target-id"),
        };
        dev.remove_display(&req)?;
        if args.json {
            println!(
                "{}",
                serde_json::json!({ "backend": "direct-ioctl", "removed": true })
            );
        } else {
            println!("Virtual display removed.");
        }
        Ok(())
    }

    #[cfg(not(windows))]
    {
        let _ = args.json;
        bail!("zerovdd display remove is only supported on Windows")
    }
}

#[derive(Serialize)]
struct DisplayEntryJson {
    monitor_guid: String,
    client_name: String,
    adapter_luid_low: u32,
    adapter_luid_high: i32,
    target_id: u32,
    connector_index: u32,
    width: u32,
    height: u32,
    refresh_num: u32,
    refresh_den: u32,
    hdr_profile: u8,
    ownership_mode: u8,
    lease_remaining_secs: u32,
    active: bool,
}

fn print_display_list(items: &[DisplayEntryJson], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(items)?);
    } else if items.is_empty() {
        println!("No active virtual displays.");
    } else {
        for item in items {
            println!(
                "- {} ({}) target={} connector={} {}x{}@{}/{} Hz luid={:#x}:{:#x} lease={}s",
                item.monitor_guid,
                item.client_name,
                item.target_id,
                item.connector_index,
                item.width,
                item.height,
                item.refresh_num,
                item.refresh_den,
                item.adapter_luid_low,
                item.adapter_luid_high,
                item.lease_remaining_secs
            );
        }
    }
    Ok(())
}

fn run_list(args: DisplayListArgs) -> Result<()> {
    if let Some(resp) = try_send_ipc_request(&IpcRequest::DisplayList)? {
        match resp {
            IpcResponse::DisplayList { displays: entries } => {
                let items: Vec<DisplayEntryJson> = entries
                    .into_iter()
                    .map(|d| DisplayEntryJson {
                        monitor_guid: d.monitor_guid,
                        client_name: d.client_name,
                        adapter_luid_low: d.adapter_luid_low,
                        adapter_luid_high: d.adapter_luid_high,
                        target_id: d.target_id,
                        connector_index: d.connector_index,
                        width: d.width,
                        height: d.height,
                        refresh_num: d.refresh_num,
                        refresh_den: d.refresh_den,
                        hdr_profile: u8::from(d.hdr_active),
                        ownership_mode: d.ownership_mode,
                        lease_remaining_secs: d.lease_remaining_secs,
                        active: true,
                    })
                    .collect();
                return print_display_list(&items, args.json);
            }
            IpcResponse::Error(err) => {
                bail!(
                    "zerovdd-svc rejected DisplayList [{}]: {}",
                    err.code,
                    err.message
                );
            }
            other => bail!("unexpected IPC response from zerovdd-svc: {other:?}"),
        }
    }

    #[cfg(windows)]
    {
        use zerovdd_client::device::ControlDevice;

        let dev = ControlDevice::open()?;
        let list = dev.list_displays()?;
        let entries = list.entries()?;
        let items: Vec<DisplayEntryJson> = entries
            .iter()
            .map(|d| DisplayEntryJson {
                monitor_guid: Uuid::from_bytes(d.monitor_guid).hyphenated().to_string(),
                client_name: d.client_name_str().unwrap_or("<invalid>").to_string(),
                adapter_luid_low: d.adapter_luid_low,
                adapter_luid_high: d.adapter_luid_high,
                target_id: d.target_id,
                connector_index: d.connector_index,
                width: d.width,
                height: d.height,
                refresh_num: d.refresh_num,
                refresh_den: d.refresh_den,
                hdr_profile: d.hdr_profile,
                ownership_mode: d.ownership_mode,
                lease_remaining_secs: d.lease_remaining_secs,
                active: d.active != 0,
            })
            .collect();
        print_display_list(&items, args.json)
    }

    #[cfg(not(windows))]
    {
        let _ = args.json;
        bail!("zerovdd display list is only supported on Windows")
    }
}

fn run_switch_gpu(args: SwitchGpuArgs) -> Result<()> {
    let _ = parse_gpu_selector(&args.selector)?;
    let ipc_req = IpcRequest::SwitchGpu(SwitchGpuRequest {
        selector: args.selector.clone(),
    });
    if let Some(resp) = try_send_ipc_request(&ipc_req)? {
        match resp {
            IpcResponse::GpuSwitched {
                adapter_luid_low,
                adapter_luid_high,
                friendly_name,
            } => {
                if args.json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "backend": "zerovdd-svc",
                            "name": friendly_name,
                            "adapter_luid_low": adapter_luid_low,
                            "adapter_luid_high": adapter_luid_high,
                        }))?
                    );
                } else {
                    println!(
                        "Switched driver render GPU to `{friendly_name}` (LUID {adapter_luid_low:#x}:{adapter_luid_high:#x}) via zerovdd-svc."
                    );
                }
                return Ok(());
            }
            IpcResponse::Error(err) => {
                bail!(
                    "zerovdd-svc rejected SwitchGpu [{}]: {}",
                    err.code,
                    err.message
                );
            }
            other => bail!("unexpected IPC response from zerovdd-svc: {other:?}"),
        }
    }

    #[cfg(windows)]
    {
        use zerovdd_client::device::ControlDevice;
        use zerovdd_client::gpu::{enumerate_dxgi_adapters, select_gpu};
        use zerovdd_proto::types::SetRenderAdapterRequest;

        let selector = parse_gpu_selector(&args.selector)?;
        let adapters = enumerate_dxgi_adapters()?;
        let chosen = select_gpu(&adapters, &selector)?;

        let dev = ControlDevice::open()?;
        let req = SetRenderAdapterRequest::new(chosen.adapter_luid_low, chosen.adapter_luid_high);
        dev.set_render_adapter(&req)?;

        if args.json {
            println!("{}", serde_json::to_string_pretty(chosen)?);
        } else {
            println!(
                "Switched driver render GPU to `{}` (LUID {:#x}:{:#x}).",
                chosen.name, chosen.adapter_luid_low, chosen.adapter_luid_high
            );
        }
        Ok(())
    }

    #[cfg(not(windows))]
    {
        bail!("zerovdd display switch-gpu is only supported on Windows")
    }
}

fn run_restore(args: DisplayRestoreArgs) -> Result<()> {
    if let Some(resp) = try_send_ipc_request(&IpcRequest::RestoreTopology)? {
        match resp {
            IpcResponse::TopologyRestored {
                restored_from_backup,
                departed_displays,
            } => {
                if args.json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "backend": "zerovdd-svc",
                            "restored_from_backup": restored_from_backup,
                            "departed_displays": departed_displays,
                        }))?
                    );
                } else {
                    println!(
                        "Restored physical display topology via zerovdd-svc (from_backup={restored_from_backup}, departed_displays={departed_displays})."
                    );
                }
                return Ok(());
            }
            IpcResponse::Error(err) => {
                bail!(
                    "zerovdd-svc rejected RestoreTopology [{}]: {}",
                    err.code,
                    err.message
                );
            }
            other => bail!("unexpected IPC response from zerovdd-svc: {other:?}"),
        }
    }

    #[cfg(windows)]
    {
        use zerovdd_client::ccd::{restore_streaming_session_with_fallback, WindowsCcdBackend};
        use zerovdd_client::device::ControlDevice;
        use zerovdd_client::topology::default_snapshot_path;
        use zerovdd_proto::types::RemoveDisplayRequest;

        let mut departed_displays = 0u32;
        if let Ok(dev) = ControlDevice::open() {
            if let Ok(list) = dev.list_displays() {
                if let Ok(entries) = list.entries() {
                    for info in entries {
                        if dev
                            .remove_display(&RemoveDisplayRequest::by_guid(info.monitor_guid))
                            .is_ok()
                        {
                            departed_displays = departed_displays.saturating_add(1);
                        }
                    }
                }
            }
        }
        let report = restore_streaming_session_with_fallback(
            &WindowsCcdBackend::new(),
            &default_snapshot_path(),
            None,
            true,
        )?;
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "backend": "direct-ccd",
                    "restored_from_backup": report.snapshot_found,
                    "departed_displays": departed_displays,
                }))?
            );
        } else {
            println!(
                "Restored physical display topology directly (from_backup={}, departed_displays={departed_displays}).",
                report.snapshot_found
            );
        }
        Ok(())
    }

    #[cfg(not(windows))]
    {
        let _ = args.json;
        bail!("zerovdd display restore is only supported on Windows")
    }
}

#[derive(Debug, Serialize)]
struct DisplaySetModeOutput {
    backend: &'static str,
    adapter_luid_low: u32,
    adapter_luid_high: i32,
    target_id: u32,
    connector_index: u32,
    width: u32,
    height: u32,
    refresh_num: u32,
    refresh_den: u32,
    refresh_mhz: u32,
    hdr: bool,
    mode_count: u32,
}

fn resolve_hdr_override(hdr: bool, sdr: bool) -> Result<Option<bool>> {
    match (hdr, sdr) {
        (true, true) => bail!("--hdr and --sdr cannot be used together"),
        (true, false) => Ok(Some(true)),
        (false, true) => Ok(Some(false)),
        (false, false) => Ok(None),
    }
}

fn run_set_mode(args: DisplaySetModeArgs) -> Result<()> {
    if let Some(target_id) = args.target_id {
        if target_id == 0 {
            bail!("--target-id must be non-zero");
        }
    }
    let mode = parse_mode_spec(&args.mode)?;
    let hdr_override = resolve_hdr_override(args.hdr, args.sdr)?;
    if let Some(hdr_val) = hdr_override {
        let profile = if hdr_val {
            zerovdd_proto::types::HdrProfile::Hdr10
        } else {
            zerovdd_proto::types::HdrProfile::Sdr
        };
        validate_color_and_bit_depth(
            profile,
            args.color_space.map(Into::into).unwrap_or_default(),
            args.bpc.map(Into::into).unwrap_or_default(),
        )?;
        let _ = resolve_explicit_hdr_metadata(
            hdr_val,
            args.max_nits,
            args.max_fall,
            args.min_nits_10k,
        )?;
    } else if args.max_nits.is_some() || args.max_fall.is_some() || args.min_nits_10k.is_some() {
        let meta = HdrMasteringMetadata {
            max_luminance_nits: args.max_nits.unwrap_or(1000),
            max_fall_nits: args.max_fall.unwrap_or(400),
            min_luminance_ten_thousandths: args.min_nits_10k.unwrap_or(500),
        };
        validate_hdr_metadata(meta)?;
    }
    let client_id = args
        .guid
        .map(|g| g.hyphenated().to_string())
        .or_else(|| args.name.clone());

    let ipc_req = IpcRequest::DisplaySetMode(DisplaySetModeRequest {
        client_id,
        target_id: args.target_id,
        width: mode.width,
        height: mode.height,
        refresh_mhz: mode.refresh_mhz,
        hdr: hdr_override,
        color_space: args.color_space.map(Into::into),
        bit_depth: args.bpc.map(Into::into),
        max_luminance_nits: args.max_nits,
        max_fall_nits: args.max_fall,
        min_luminance_ten_thousandths: args.min_nits_10k,
    });

    if let Some(resp) = try_send_ipc_request(&ipc_req)? {
        match resp {
            IpcResponse::DisplayModeUpdated {
                adapter_luid_low,
                adapter_luid_high,
                target_id,
                connector_index,
                width,
                height,
                refresh_mhz,
                hdr_active,
                mode_count,
            } => {
                let (refresh_num, refresh_den) = millihz_to_rational(refresh_mhz);
                let out = DisplaySetModeOutput {
                    backend: "zerovdd-svc",
                    adapter_luid_low,
                    adapter_luid_high,
                    target_id,
                    connector_index,
                    width,
                    height,
                    refresh_num,
                    refresh_den,
                    refresh_mhz,
                    hdr: hdr_active,
                    mode_count,
                };
                print_set_mode_output(&out, args.json)?;
                return Ok(());
            }
            IpcResponse::Error(err) => {
                bail!(
                    "zerovdd-svc rejected DisplaySetMode [{}]: {}",
                    err.code,
                    err.message
                );
            }
            other => bail!("unexpected IPC response from zerovdd-svc: {other:?}"),
        }
    }

    run_standalone_set_mode(&args, &mode, hdr_override)
}

fn print_set_mode_output(out: &DisplaySetModeOutput, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(out)?);
    } else {
        let hz_whole = out.refresh_mhz / 1000;
        let hz_frac = out.refresh_mhz % 1000;
        println!(
            "Updated virtual display via {} (target={} connector={}): {}x{}@{}.{:03}Hz HDR={} ({} modes)",
            out.backend,
            out.target_id,
            out.connector_index,
            out.width,
            out.height,
            hz_whole,
            hz_frac,
            out.hdr,
            out.mode_count
        );
    }
    Ok(())
}

#[cfg(windows)]
fn run_standalone_set_mode(
    args: &DisplaySetModeArgs,
    mode: &ModeSpec,
    hdr_override: Option<bool>,
) -> Result<()> {
    use zerovdd_client::ccd::{apply_display_mode_in_place, WindowsCcdBackend};
    use zerovdd_client::device::ControlDevice;
    use zerovdd_client::topology::AdapterLuid;
    use zerovdd_proto::types::{HdrProfile, UpdateDisplayModeRequest};

    let dev = ControlDevice::open()?;
    let list = dev.list_displays()?;
    let entries = list.entries()?;

    let matched = if let Some(target_id) = args.target_id {
        entries
            .iter()
            .find(|d| d.target_id == target_id)
            .ok_or_else(|| {
                anyhow::anyhow!("no active virtual display found for target_id={target_id}")
            })?
    } else if let Some(guid) = args.guid {
        entries
            .iter()
            .find(|d| d.monitor_guid == *guid.as_bytes())
            .ok_or_else(|| anyhow::anyhow!("no active virtual display found for guid={guid}"))?
    } else if let Some(ref name) = args.name {
        let derived = monitor_guid_for_name(name);
        let matches: Vec<_> = entries
            .iter()
            .filter(|d| {
                d.monitor_guid == derived || d.client_name_str().ok() == Some(name.as_str())
            })
            .collect();
        match matches.as_slice() {
            [] => bail!("no active virtual display found for name `{name}`"),
            [only] => *only,
            _ => bail!(
                "multiple active virtual displays match name `{name}`; pass --guid or --target-id"
            ),
        }
    } else if entries.len() == 1 {
        &entries[0]
    } else if entries.is_empty() {
        bail!("no active virtual displays are present");
    } else {
        bail!("multiple virtual displays are active; pass --name, --guid, or --target-id");
    };

    let prev_hdr_profile = HdrProfile::try_from(matched.hdr_profile).unwrap_or(HdrProfile::Sdr);
    let current_hdr = prev_hdr_profile == HdrProfile::Hdr10;
    let hdr_active = hdr_override.unwrap_or(current_hdr);
    let hdr_profile = if hdr_active {
        HdrProfile::Hdr10
    } else {
        HdrProfile::Sdr
    };

    let prev_req = UpdateDisplayModeRequest::by_guid(
        matched.monitor_guid,
        matched.width,
        matched.height,
        matched.refresh_num,
        matched.refresh_den,
        prev_hdr_profile,
    );
    let mut req = UpdateDisplayModeRequest::by_guid(
        matched.monitor_guid,
        mode.width,
        mode.height,
        mode.refresh_num,
        mode.refresh_den,
        hdr_profile,
    );
    req.color_space = args
        .color_space
        .map(ColorSpacePreference::from)
        .unwrap_or_default()
        .as_u8();
    req.bit_depth = args
        .bpc
        .map(BitDepthPreference::from)
        .unwrap_or_default()
        .as_u8();
    if hdr_active
        && (args.max_nits.is_some() || args.max_fall.is_some() || args.min_nits_10k.is_some())
    {
        req.hdr_metadata =
            resolve_explicit_hdr_metadata(true, args.max_nits, args.max_fall, args.min_nits_10k)?;
    } else if !hdr_active
        && (args.max_nits.is_some() || args.max_fall.is_some() || args.min_nits_10k.is_some())
    {
        bail!("--max-nits, --max-fall, and --min-nits-10k require HDR10 (`--hdr`)");
    }
    let resp = dev.update_display_mode(&req)?;
    if let Err(err) = apply_display_mode_in_place(
        &WindowsCcdBackend::new(),
        AdapterLuid::new(resp.adapter_luid_low, resp.adapter_luid_high),
        resp.target_id,
        mode.width,
        mode.height,
        mode.refresh_num,
        mode.refresh_den,
        hdr_override,
    ) {
        let _ = dev.update_display_mode(&prev_req);
        return Err(err.into());
    }

    let out = DisplaySetModeOutput {
        backend: "direct-ioctl",
        adapter_luid_low: resp.adapter_luid_low,
        adapter_luid_high: resp.adapter_luid_high,
        target_id: resp.target_id,
        connector_index: resp.connector_index,
        width: mode.width,
        height: mode.height,
        refresh_num: mode.refresh_num,
        refresh_den: mode.refresh_den,
        refresh_mhz: mode.refresh_mhz,
        hdr: hdr_active,
        mode_count: resp.mode_count,
    };
    print_set_mode_output(&out, args.json)
}

#[cfg(not(windows))]
fn run_standalone_set_mode(
    _args: &DisplaySetModeArgs,
    _mode: &ModeSpec,
    _hdr_override: Option<bool>,
) -> Result<()> {
    bail!("zerovdd display set-mode is only supported on Windows")
}

#[cfg(windows)]
pub(crate) fn try_send_ipc_request(req: &IpcRequest) -> Result<Option<IpcResponse>> {
    if std::env::var_os("ZEROVDD_NO_SERVICE_IPC").is_some() {
        return Ok(None);
    }
    use std::io::{Read, Write};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_PIPE_BUSY, GENERIC_READ,
        GENERIC_WRITE, HANDLE,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FlushFileBuffers, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_NONE,
        OPEN_EXISTING,
    };
    use windows::Win32::System::Pipes::{
        SetNamedPipeHandleState, WaitNamedPipeW, PIPE_READMODE_BYTE,
    };

    use zerovdd_proto::ipc::{read_frame, write_frame, IPC_PIPE_NAME};

    struct PipeStream(HANDLE);

    impl Drop for PipeStream {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: `self.0` was opened via `CreateFileW` and is closed once on drop.
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    impl Read for PipeStream {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let mut bytes_read: u32 = 0;
            // SAFETY: `self.0` is an open named pipe handle and `buf` is valid for `buf.len()` bytes.
            unsafe { ReadFile(self.0, Some(buf), Some(&mut bytes_read), None) }
                .map_err(|err| std::io::Error::from_raw_os_error(err.code().0))?;
            Ok(bytes_read as usize)
        }
    }

    impl Write for PipeStream {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let mut written: u32 = 0;
            // SAFETY: `self.0` is an open named pipe handle and `buf` is valid for `buf.len()` bytes.
            unsafe { WriteFile(self.0, Some(buf), Some(&mut written), None) }
                .map_err(|err| std::io::Error::from_raw_os_error(err.code().0))?;
            Ok(written as usize)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            // SAFETY: `self.0` is an open named pipe handle.
            unsafe { FlushFileBuffers(self.0) }
                .map_err(|err| std::io::Error::from_raw_os_error(err.code().0))
        }
    }

    let wide: Vec<u16> = IPC_PIPE_NAME
        .encode_utf16()
        .chain(core::iter::once(0))
        .collect();

    let mut handle = None;
    for _ in 0..3 {
        // SAFETY: `wide` is NUL-terminated UTF-16 pointing to `IPC_PIPE_NAME`.
        match unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        } {
            Ok(h) => {
                handle = Some(h);
                break;
            }
            Err(err)
                if err.code() == windows::core::HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0)
                    || err.code() == windows::core::HRESULT::from_win32(ERROR_PATH_NOT_FOUND.0) =>
            {
                return Ok(None);
            }
            Err(err) if err.code() == windows::core::HRESULT::from_win32(ERROR_PIPE_BUSY.0) => {
                // SAFETY: `wide` is NUL-terminated UTF-16.
                let ok = unsafe { WaitNamedPipeW(PCWSTR(wide.as_ptr()), 1000) };
                if !ok.as_bool() {
                    return Ok(None);
                }
            }
            Err(err) => return Err(err.into()),
        }
    }
    let Some(handle) = handle else {
        return Ok(None);
    };

    let mode = PIPE_READMODE_BYTE;
    // SAFETY: `handle` is an open named pipe handle and `mode` is a stack local.
    let _ = unsafe { SetNamedPipeHandleState(handle, Some(&mode), None, None) };

    let mut stream = PipeStream(handle);
    let envelope = IpcEnvelope::new(1, req.clone());
    write_frame(&mut stream, &envelope)?;
    let reply: IpcEnvelope<IpcResponse> = read_frame(&mut stream)?;
    reply.validate_version()?;
    Ok(Some(reply.payload))
}

#[cfg(not(windows))]
pub(crate) fn try_send_ipc_request(_req: &IpcRequest) -> Result<Option<IpcResponse>> {
    let _ = core::mem::size_of::<IpcEnvelope<IpcResponse>>();
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_integer_and_fractional_mode_specs() {
        let m1 = parse_mode_spec("3840x2160@59.94").unwrap();
        assert_eq!(m1.width, 3840);
        assert_eq!(m1.height, 2160);
        assert_eq!(m1.refresh_num, 60_000);
        assert_eq!(m1.refresh_den, 1001);
        assert_eq!(m1.refresh_mhz, 59_940);

        let m2 = parse_mode_spec("3840x2160@119.88").unwrap();
        assert_eq!(m2.refresh_num, 120_000);
        assert_eq!(m2.refresh_den, 1001);
        assert_eq!(m2.refresh_mhz, 119_880);

        let m3 = parse_mode_spec("2560x1600@90").unwrap();
        assert_eq!(m3.width, 2560);
        assert_eq!(m3.height, 1600);
        assert_eq!(m3.refresh_num, 90);
        assert_eq!(m3.refresh_den, 1);
        assert_eq!(m3.refresh_mhz, 90_000);

        let m4 = parse_mode_spec("1920X1080@60Hz").unwrap();
        assert_eq!(m4.width, 1920);
        assert_eq!(m4.height, 1080);
        assert_eq!(m4.refresh_num, 60);
        assert_eq!(m4.refresh_den, 1);

        let m5 = parse_mode_spec("1920x1080@23.976").unwrap();
        assert_eq!(m5.refresh_num, 24_000);
        assert_eq!(m5.refresh_den, 1001);
    }

    #[test]
    fn rejects_invalid_mode_specs() {
        for bad in [
            "1920x1080@0",
            "1920x1080@0.00",
            "0x1080@60",
            "100x100@60",
            "1920x1080",
            "1920@60",
            "1920x1080@-60",
            "1920x1080@60.1234",
            "1920x1080@",
            "1920x1080@60é",
            "1920x1080@⚡",
            "1920x1080@éhz",
            "1920x1080@60⚡hz",
        ] {
            assert!(
                parse_mode_spec(bad).is_err(),
                "expected `{bad}` to be rejected"
            );
        }
    }

    #[test]
    fn enforces_standalone_hold_or_lease_requirement() {
        assert!(validate_add_lifetime(false, false, None).is_err());
        assert!(validate_add_lifetime(false, false, Some(0)).is_err());
        assert!(validate_add_lifetime(false, false, Some(MAX_LEASE_TTL_SECS + 1)).is_err());
        assert!(validate_add_lifetime(false, true, None).is_ok());
        assert!(validate_add_lifetime(false, false, Some(60)).is_ok());
        assert!(validate_add_lifetime(true, false, None).is_ok());
    }

    #[test]
    fn parses_layout_orientation_and_position_flags() {
        assert_eq!(parse_position_xy("-1080,0").unwrap(), (-1080, 0));
        assert_eq!(parse_position_xy("2560, -1440").unwrap(), (2560, -1440));
        assert!(parse_position_xy("2560").is_err());
        assert_eq!(
            parse_orientation_deg(0).unwrap(),
            DisplayOrientation::Identity
        );
        assert_eq!(
            parse_orientation_deg(90).unwrap(),
            DisplayOrientation::Rotate90
        );
        assert_eq!(
            parse_orientation_deg(180).unwrap(),
            DisplayOrientation::Rotate180
        );
        assert_eq!(
            parse_orientation_deg(270).unwrap(),
            DisplayOrientation::Rotate270
        );
        assert!(parse_orientation_deg(45).is_err());
    }

    #[test]
    fn parses_gpu_selectors() {
        assert_eq!(parse_gpu_selector("auto").unwrap(), GpuSelector::AutoBest);
        assert_eq!(
            parse_gpu_selector("luid:0x1234:-1").unwrap(),
            GpuSelector::Luid {
                low: 0x1234,
                high: -1
            }
        );
        assert_eq!(
            parse_gpu_selector("pci:3:0.1").unwrap(),
            GpuSelector::PciBusAndName {
                bus: 3,
                device: Some(0),
                function: Some(1),
                name: None
            }
        );
        assert_eq!(
            parse_gpu_selector("NVIDIA GeForce RTX 4090").unwrap(),
            GpuSelector::FriendlyName {
                name: "NVIDIA GeForce RTX 4090".to_string()
            }
        );
        assert!(parse_gpu_selector("   ").is_err());
    }
}
