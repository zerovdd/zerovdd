//! `#![no_std]` 64-bit VESA CVT-RBv2 timing math, rational refresh rates, and mode planner.

#![no_std]

extern crate alloc;

use thiserror::Error;

/// Unified 64-bit VESA CVT-RBv2 `VideoSignalInfo` calculator.
pub mod cvt;
/// Mode list planner with even-rounded scaling, refresh variants, and deduplication.
pub mod planner;
/// Exact 64-bit rational refresh rate representation with GCD reduction.
pub mod rational;

pub use cvt::{
    compute_cvt_rb2, compute_cvt_rb2_monitor_signal, compute_cvt_rb2_target_signal,
    VideoSignalInfo, VideoSignalRole,
};
pub use planner::{
    plan_modes, round_to_even_pixel, BitDepthMask, ColorCapability, ModePlanRequest, PlannedMode,
    DEFAULT_REFRESH_LADDER_MILLIHZ, DEFAULT_SCALE_PERCENTS,
};
pub use rational::{gcd_u128, gcd_u64, RationalHz};

/// Errors returned by rational refresh construction, CVT-RBv2 timing math, or mode planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ModeError {
    /// Refresh rate numerator or denominator is zero.
    #[error("refresh rate must be strictly positive (zero is invalid)")]
    ZeroRefreshRate,
    /// Active horizontal or vertical resolution is zero.
    #[error("display dimensions {width}x{height} must be non-zero")]
    InvalidDimensions {
        /// Requested width in pixels.
        width: u32,
        /// Requested height in lines.
        height: u32,
    },
    /// Refresh rate exceeds the CVT-RBv2 460 us minimum vertical blanking budget.
    #[error("refresh rate exceeds CVT-RBv2 minimum vertical blanking budget")]
    RefreshRateTooHigh,
    /// Intermediate timing math or checked narrowing overflowed target integer bounds.
    #[error("arithmetic overflow while computing display timings")]
    ArithmeticOverflow,
}
