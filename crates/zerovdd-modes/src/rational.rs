//! Exact 64-bit rational refresh rate representation with GCD reduction.

use core::cmp::Ordering;
use core::fmt;

use crate::ModeError;

/// Computes the greatest common divisor of two `u64` values using Euclid's algorithm.
#[must_use]
pub const fn gcd_u64(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

/// Computes the greatest common divisor of two `u128` values using Euclid's algorithm.
#[must_use]
pub const fn gcd_u128(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

/// Exact reduced positive rational refresh rate in Hz (`num / den`).
///
/// Invariants guaranteed by constructors: `num > 0`, `den > 0`, and `gcd(num, den) == 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RationalHz {
    num: u64,
    den: u64,
}

impl RationalHz {
    /// Constructs a reduced `RationalHz` from `num / den`.
    ///
    /// Returns `Err(ModeError::ZeroRefreshRate)` if `num == 0` or `den == 0`.
    pub const fn new(num: u64, den: u64) -> Result<Self, ModeError> {
        if num == 0 || den == 0 {
            return Err(ModeError::ZeroRefreshRate);
        }
        let g = gcd_u64(num, den);
        Ok(Self {
            num: num / g,
            den: den / g,
        })
    }

    /// Constructs an integer Hz refresh rate (`hz / 1`).
    ///
    /// Returns `Err(ModeError::ZeroRefreshRate)` if `hz == 0`.
    pub const fn from_hz(hz: u32) -> Result<Self, ModeError> {
        Self::new(hz as u64, 1)
    }

    /// Constructs a rational refresh rate from millihertz (for example `59_940` -> `60000/1001`).
    ///
    /// Recognizes standard NTSC `1000/1001` rates (`23.976`, `29.97`, `47.952`, `59.94`,
    /// `119.88`, `239.76` Hz) and reduces all other rates via exact GCD over `1000`.
    /// Returns `Err(ModeError::ZeroRefreshRate)` when `millihz == 0`.
    pub const fn from_millihz(millihz: u32) -> Result<Self, ModeError> {
        if millihz == 0 {
            return Err(ModeError::ZeroRefreshRate);
        }
        match millihz {
            23_976 => Self::new(24_000, 1001),
            29_970 => Self::new(30_000, 1001),
            47_952 => Self::new(48_000, 1001),
            59_940 => Self::new(60_000, 1001),
            119_880 => Self::new(120_000, 1001),
            239_760 => Self::new(240_000, 1001),
            _ => Self::new(millihz as u64, 1000),
        }
    }

    /// Constructs an exact NTSC `base_hz * 1000 / 1001` rational refresh rate.
    ///
    /// Returns `Err(ModeError::ZeroRefreshRate)` when `base_hz == 0`.
    pub const fn from_ntsc_base_hz(base_hz: u32) -> Result<Self, ModeError> {
        if base_hz == 0 {
            return Err(ModeError::ZeroRefreshRate);
        }
        Self::new((base_hz as u64) * 1000, 1001)
    }

    /// Returns the reduced numerator.
    #[must_use]
    pub const fn num(self) -> u64 {
        self.num
    }

    /// Returns the reduced denominator (always non-zero).
    #[must_use]
    pub const fn den(self) -> u64 {
        self.den
    }

    /// Returns the exact doubled refresh rate (`2 * num / den`, GCD-reduced).
    pub const fn double(self) -> Result<Self, ModeError> {
        match self.num.checked_mul(2) {
            Some(doubled) => Self::new(doubled, self.den),
            None => Err(ModeError::ArithmeticOverflow),
        }
    }

    /// Returns the rounded frequency in millihertz using `u128` intermediate math.
    #[must_use]
    pub const fn as_millihz_rounded(self) -> u64 {
        let numer = (self.num as u128) * 1000 + (self.den as u128) / 2;
        (numer / (self.den as u128)) as u64
    }

    /// Narrows `(num, den)` to `u32` for `DISPLAYCONFIG_RATIONAL` with overflow checking.
    pub fn to_u32_pair(self) -> Result<(u32, u32), ModeError> {
        let num = u32::try_from(self.num).map_err(|_| ModeError::ArithmeticOverflow)?;
        let den = u32::try_from(self.den).map_err(|_| ModeError::ArithmeticOverflow)?;
        Ok((num, den))
    }
}

impl Ord for RationalHz {
    fn cmp(&self, other: &Self) -> Ordering {
        let lhs = u128::from(self.num) * u128::from(other.den);
        let rhs = u128::from(other.num) * u128::from(self.den);
        lhs.cmp(&rhs)
    }
}

impl PartialOrd for RationalHz {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for RationalHz {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{} Hz", self.num, self.den)
    }
}
