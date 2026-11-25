//! Unified 64-bit overflow-proof VESA CVT-RBv2 timing calculator.

use crate::rational::{gcd_u128, RationalHz};
use crate::ModeError;

const CVT_RB2_H_BLANK: u64 = 80;
const CVT_RB2_MIN_V_BLANK_US: u128 = 460;
const CVT_RB2_MIN_VBI_LINES: u128 = 15;

/// Target IddCx callback role controlling `vSyncFreqDivider`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoSignalRole {
    /// Monitor description / preferred range (`ParseMonitorDescription2`, `vSyncFreqDivider = 0`).
    MonitorDescription,
    /// Target mode list (`QueryTargetModes2`, `vSyncFreqDivider = 1`).
    TargetMode,
}

impl VideoSignalRole {
    /// Returns the Windows `DISPLAYCONFIG_VIDEO_SIGNAL_INFO::vSyncFreqDivider` value (`0` or `1`).
    #[must_use]
    pub const fn vsync_freq_divider(self) -> u32 {
        match self {
            Self::MonitorDescription => 0,
            Self::TargetMode => 1,
        }
    }
}

/// Complete timing parameters matching Windows `DISPLAYCONFIG_VIDEO_SIGNAL_INFO`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoSignalInfo {
    /// Pixel clock rate in Hz (`u64` to prevent 32-bit overflow up to 8K@240Hz and above).
    pub pixel_rate: u64,
    /// Active horizontal resolution in pixels.
    pub active_width: u32,
    /// Active vertical resolution in lines.
    pub active_height: u32,
    /// Total horizontal pixels including CVT-RBv2 blanking (`active_width + 80`).
    pub total_width: u32,
    /// Total vertical lines including VBI.
    pub total_height: u32,
    /// Horizontal sync frequency numerator (Hz).
    pub hsync_freq_num: u32,
    /// Horizontal sync frequency denominator.
    pub hsync_freq_den: u32,
    /// Vertical sync frequency numerator (Hz).
    pub vsync_freq_num: u32,
    /// Vertical sync frequency denominator.
    pub vsync_freq_den: u32,
    /// `vSyncFreqDivider` (`0` for monitor description, `1` for target mode).
    pub vsync_freq_divider: u32,
}

impl VideoSignalInfo {
    /// Returns an identical `VideoSignalInfo` with the given `VideoSignalRole` divider.
    #[must_use]
    pub const fn with_role(mut self, role: VideoSignalRole) -> Self {
        self.vsync_freq_divider = role.vsync_freq_divider();
        self
    }
}

/// Computes unified VESA CVT-RBv2 timings using 64/128-bit checked arithmetic.
///
/// Both `MonitorDescription` (`vsync_freq_divider = 0`) and `TargetMode`
/// (`vsync_freq_divider = 1`) use this exact function so Windows Monitor/Target mode
/// intersection matches field-for-field.
pub fn compute_cvt_rb2(
    width: u32,
    height: u32,
    refresh: RationalHz,
    role: VideoSignalRole,
) -> Result<VideoSignalInfo, ModeError> {
    if width == 0 || height == 0 {
        return Err(ModeError::InvalidDimensions { width, height });
    }

    let total_width_u64 = u64::from(width)
        .checked_add(CVT_RB2_H_BLANK)
        .ok_or(ModeError::ArithmeticOverflow)?;
    let total_width = u32::try_from(total_width_u64).map_err(|_| ModeError::ArithmeticOverflow)?;

    let num = refresh.num();
    let den = refresh.den();

    // WHY: CVT-RBv2 computes VBI = ceil(460us / h_period_est) where
    // h_period_est = (1_000_000 / refresh_hz - 460) / active_height.
    // Multiplying numerator and denominator by refresh.den() keeps all math exact.
    let frame_us_scaled = 1_000_000u128
        .checked_mul(u128::from(den))
        .ok_or(ModeError::ArithmeticOverflow)?;
    let vbi_us_scaled = CVT_RB2_MIN_V_BLANK_US
        .checked_mul(u128::from(num))
        .ok_or(ModeError::ArithmeticOverflow)?;
    let active_period_us_scaled = frame_us_scaled
        .checked_sub(vbi_us_scaled)
        .filter(|&v| v > 0)
        .ok_or(ModeError::RefreshRateTooHigh)?;

    let vbi_numer = CVT_RB2_MIN_V_BLANK_US
        .checked_mul(u128::from(height))
        .and_then(|v| v.checked_mul(u128::from(num)))
        .ok_or(ModeError::ArithmeticOverflow)?;
    let vbi_lines = vbi_numer
        .div_ceil(active_period_us_scaled)
        .max(CVT_RB2_MIN_VBI_LINES);

    let total_height_u128 = u128::from(height)
        .checked_add(vbi_lines)
        .ok_or(ModeError::ArithmeticOverflow)?;
    let total_height =
        u32::try_from(total_height_u128).map_err(|_| ModeError::ArithmeticOverflow)?;

    let pixel_rate_numer = u128::from(total_width)
        .checked_mul(u128::from(total_height))
        .and_then(|v| v.checked_mul(u128::from(num)))
        .ok_or(ModeError::ArithmeticOverflow)?;
    let pixel_rate_u128 = pixel_rate_numer.div_ceil(u128::from(den));
    let pixel_rate = u64::try_from(pixel_rate_u128).map_err(|_| ModeError::ArithmeticOverflow)?;

    // WHY: Reduce hsync_freq = total_height * (num / den) in u128 via GCD before checked
    // u32 narrowing so high-resolution or fractional rates never overflow intermediate u32s.
    let hsync_num_u128 = u128::from(total_height)
        .checked_mul(u128::from(num))
        .ok_or(ModeError::ArithmeticOverflow)?;
    let hsync_den_u128 = u128::from(den);
    let hsync_gcd = gcd_u128(hsync_num_u128, hsync_den_u128);
    let hsync_freq_num =
        u32::try_from(hsync_num_u128 / hsync_gcd).map_err(|_| ModeError::ArithmeticOverflow)?;
    let hsync_freq_den =
        u32::try_from(hsync_den_u128 / hsync_gcd).map_err(|_| ModeError::ArithmeticOverflow)?;

    let (vsync_freq_num, vsync_freq_den) = refresh.to_u32_pair()?;

    Ok(VideoSignalInfo {
        pixel_rate,
        active_width: width,
        active_height: height,
        total_width,
        total_height,
        hsync_freq_num,
        hsync_freq_den,
        vsync_freq_num,
        vsync_freq_den,
        vsync_freq_divider: role.vsync_freq_divider(),
    })
}

/// Computes CVT-RBv2 timings for `ParseMonitorDescription2` (`vsync_freq_divider = 0`).
pub fn compute_cvt_rb2_monitor_signal(
    width: u32,
    height: u32,
    refresh: RationalHz,
) -> Result<VideoSignalInfo, ModeError> {
    compute_cvt_rb2(width, height, refresh, VideoSignalRole::MonitorDescription)
}

/// Computes CVT-RBv2 timings for `QueryTargetModes2` (`vsync_freq_divider = 1`).
pub fn compute_cvt_rb2_target_signal(
    width: u32,
    height: u32,
    refresh: RationalHz,
) -> Result<VideoSignalInfo, ModeError> {
    compute_cvt_rb2(width, height, refresh, VideoSignalRole::TargetMode)
}
