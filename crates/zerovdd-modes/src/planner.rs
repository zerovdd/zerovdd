//! Unified display mode list planner with even-rounded scaling and deduplication.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use crate::cvt::{compute_cvt_rb2_monitor_signal, compute_cvt_rb2_target_signal, VideoSignalInfo};
use crate::rational::RationalHz;
use crate::ModeError;

/// Default resolution scaling percentages offered alongside the preferred client mode.
pub const DEFAULT_SCALE_PERCENTS: [u32; 5] = [50, 75, 100, 125, 150];

/// Standard descending refresh-rate ladder in millihertz exposed at the preferred resolution
/// (capped at or below the requested preferred refresh rate).
pub const DEFAULT_REFRESH_LADDER_MILLIHZ: [u32; 10] = [
    240_000, 144_000, 120_000, 119_880, 90_000, 60_000, 59_940, 48_000, 24_000, 23_976,
];

/// Bitmask of supported color depths per color component (`bpc`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BitDepthMask(u32);

impl BitDepthMask {
    /// 8 bits per channel (`0x01`).
    pub const BITS_8: Self = Self(0x01);
    /// 10 bits per channel (`0x02`).
    pub const BITS_10: Self = Self(0x02);
    /// 12 bits per channel (`0x04`).
    pub const BITS_12: Self = Self(0x04);
    /// Default SDR bit depths (`8-bit | 10-bit`).
    pub const SDR_DEFAULT: Self = Self(0x01 | 0x02);
    /// Default HDR bit depths (`10-bit | 12-bit`).
    pub const HDR_DEFAULT: Self = Self(0x02 | 0x04);

    /// Returns the raw bitmask value.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Returns `true` if all bits in `other` are set in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

/// Target dynamic range capability controlling planned mode bit-depth masks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorCapability {
    /// Standard Dynamic Range (`8-bit | 10-bit`).
    #[default]
    Sdr,
    /// High Dynamic Range (`10-bit | 12-bit`).
    Hdr,
}

impl ColorCapability {
    /// Returns the default `BitDepthMask` for this dynamic range capability.
    #[must_use]
    pub const fn bit_depths(self) -> BitDepthMask {
        match self {
            Self::Sdr => BitDepthMask::SDR_DEFAULT,
            Self::Hdr => BitDepthMask::HDR_DEFAULT,
        }
    }
}

/// Scales a pixel dimension by `percent` and rounds odd results up to an even integer.
///
/// For example, `1366 * 50% = 683` rounds up to `684`, avoiding odd horizontal widths.
pub fn round_to_even_pixel(dimension: u32, percent: u32) -> Result<u32, ModeError> {
    if dimension == 0 {
        return Err(ModeError::InvalidDimensions {
            width: 0,
            height: 0,
        });
    }
    let scaled = (u64::from(dimension) * u64::from(percent) + 50) / 100;
    let even = if scaled < 2 {
        2
    } else if scaled % 2 != 0 {
        scaled.checked_add(1).ok_or(ModeError::ArithmeticOverflow)?
    } else {
        scaled
    };
    u32::try_from(even).map_err(|_| ModeError::ArithmeticOverflow)
}

/// Input specification for planning a monitor's exposed mode list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModePlanRequest {
    /// Preferred client horizontal width in pixels (always placed at index 0).
    pub preferred_width: u32,
    /// Preferred client vertical height in lines (always placed at index 0).
    pub preferred_height: u32,
    /// Preferred client refresh rate (always placed at index 0).
    pub preferred_refresh: RationalHz,
    /// Color dynamic range capability controlling bit-depth masks.
    pub color_capability: ColorCapability,
    /// Optional explicit bit-depth mask overriding `color_capability.bit_depths()`.
    pub bit_depths_override: Option<BitDepthMask>,
    /// Whether to generate `50/75/100/125/150%` even-rounded resolution variants.
    pub include_scaled_variants: bool,
    /// Whether to include an exact `2x` refresh rate variant alongside `1x`.
    pub include_double_refresh: bool,
    /// Whether to expose standard lower/cinema refresh rates at the preferred resolution.
    pub include_refresh_ladder: bool,
    /// Additional explicit refresh rates to expose alongside `1x` (and optional `2x`).
    pub extra_refreshes: Vec<RationalHz>,
}

impl ModePlanRequest {
    /// Creates a `ModePlanRequest` with scaling, `2x` refresh, and refresh ladder enabled.
    #[must_use]
    pub fn new(
        preferred_width: u32,
        preferred_height: u32,
        preferred_refresh: RationalHz,
        color_capability: ColorCapability,
    ) -> Self {
        Self {
            preferred_width,
            preferred_height,
            preferred_refresh,
            color_capability,
            bit_depths_override: None,
            include_scaled_variants: true,
            include_double_refresh: true,
            include_refresh_ladder: true,
            extra_refreshes: Vec::new(),
        }
    }
}

/// A single planned display mode with precomputed monitor and target signal timings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannedMode {
    /// Active horizontal pixels.
    pub width: u32,
    /// Active vertical lines.
    pub height: u32,
    /// Exact rational refresh rate.
    pub refresh: RationalHz,
    /// Supported component bit-depth flags.
    pub bit_depths: BitDepthMask,
    /// `true` strictly for the preferred mode at index 0.
    pub is_preferred: bool,
    /// CVT-RBv2 signal for `ParseMonitorDescription2` (`vsync_freq_divider = 0`).
    pub monitor_signal: VideoSignalInfo,
    /// CVT-RBv2 signal for `QueryTargetModes2` (`vsync_freq_divider = 1`).
    pub target_signal: VideoSignalInfo,
}

fn build_planned_mode(
    width: u32,
    height: u32,
    refresh: RationalHz,
    bit_depths: BitDepthMask,
    is_preferred: bool,
) -> Result<PlannedMode, ModeError> {
    let monitor_signal = compute_cvt_rb2_monitor_signal(width, height, refresh)?;
    let target_signal = compute_cvt_rb2_target_signal(width, height, refresh)?;
    Ok(PlannedMode {
        width,
        height,
        refresh,
        bit_depths,
        is_preferred,
        monitor_signal,
        target_signal,
    })
}

/// Plans a deduplicated mode list with the preferred client mode guaranteed at index 0.
///
/// Deduplicates strictly by `(width, height, RationalHz)` so distinct refresh rates for the
/// same resolution are preserved and index 0 is never displaced.
pub fn plan_modes(req: &ModePlanRequest) -> Result<Vec<PlannedMode>, ModeError> {
    if req.preferred_width == 0 || req.preferred_height == 0 {
        return Err(ModeError::InvalidDimensions {
            width: req.preferred_width,
            height: req.preferred_height,
        });
    }

    let bit_depths = req
        .bit_depths_override
        .unwrap_or_else(|| req.color_capability.bit_depths());
    let preferred_mode = build_planned_mode(
        req.preferred_width,
        req.preferred_height,
        req.preferred_refresh,
        bit_depths,
        true,
    )?;

    let mut modes = Vec::new();
    let mut seen: BTreeSet<(u32, u32, RationalHz)> = BTreeSet::new();

    seen.insert((
        req.preferred_width,
        req.preferred_height,
        req.preferred_refresh,
    ));
    modes.push(preferred_mode);

    let mut base_refreshes = Vec::new();
    base_refreshes.push((req.preferred_refresh, true));
    if req.include_double_refresh {
        if let Ok(doubled) = req.preferred_refresh.double() {
            base_refreshes.push((doubled, false));
        }
    }
    for &extra in &req.extra_refreshes {
        base_refreshes.push((extra, true));
    }

    let mut preferred_refreshes = base_refreshes.clone();
    if req.include_refresh_ladder {
        for &millihz in &DEFAULT_REFRESH_LADDER_MILLIHZ {
            if let Ok(candidate) = RationalHz::from_millihz(millihz) {
                let capped_at_preferred = u128::from(candidate.num())
                    * u128::from(req.preferred_refresh.den())
                    <= u128::from(req.preferred_refresh.num()) * u128::from(candidate.den());
                if capped_at_preferred {
                    preferred_refreshes.push((candidate, false));
                }
            }
        }
    }

    let mut resolutions = Vec::new();
    resolutions.push((req.preferred_width, req.preferred_height));
    if req.include_scaled_variants {
        for pct in DEFAULT_SCALE_PERCENTS {
            if pct == 100 {
                continue;
            }
            let sw = round_to_even_pixel(req.preferred_width, pct)?;
            let sh = round_to_even_pixel(req.preferred_height, pct)?;
            if !resolutions.contains(&(sw, sh)) {
                resolutions.push((sw, sh));
            }
        }
    }

    for (w, h) in resolutions {
        let refresh_list = if (w, h) == (req.preferred_width, req.preferred_height) {
            &preferred_refreshes
        } else {
            &base_refreshes
        };
        for &(r, required) in refresh_list {
            if !seen.insert((w, h, r)) {
                continue;
            }
            match build_planned_mode(w, h, r, bit_depths, false) {
                Ok(mode) => modes.push(mode),
                Err(err) if required => return Err(err),
                Err(_) => {
                    // WHY: Optional derived 2x or ladder refresh rates above the CVT-RBv2
                    // ceiling are skipped rather than failing an otherwise valid mode plan request.
                }
            }
        }
    }

    Ok(modes)
}
