use std::collections::BTreeSet;

use proptest::prelude::*;
use zerovdd_modes::{
    compute_cvt_rb2_monitor_signal, compute_cvt_rb2_target_signal, plan_modes, round_to_even_pixel,
    BitDepthMask, ColorCapability, ModeError, ModePlanRequest, RationalHz,
};

#[test]
fn rational_hz_reduces_ntsc_and_integer_rates_and_rejects_zero() {
    let r59 = RationalHz::from_millihz(59_940).expect("59.94 Hz valid");
    assert_eq!((r59.num(), r59.den()), (60_000, 1001));

    let r119 = RationalHz::from_millihz(119_880).expect("119.88 Hz valid");
    assert_eq!((r119.num(), r119.den()), (120_000, 1001));
    assert_eq!(r59.double().unwrap(), r119);

    let r239 = RationalHz::from_millihz(239_760).expect("239.76 Hz valid");
    assert_eq!((r239.num(), r239.den()), (240_000, 1001));
    assert_eq!(r119.double().unwrap(), r239);

    let r60 = RationalHz::from_millihz(60_000).expect("60 Hz valid");
    assert_eq!((r60.num(), r60.den()), (60, 1));
    assert_eq!(RationalHz::from_hz(60).unwrap(), r60);

    let r75_5 = RationalHz::from_millihz(75_500).expect("75.5 Hz valid");
    assert_eq!((r75_5.num(), r75_5.den()), (151, 2));

    assert_eq!(RationalHz::from_millihz(0), Err(ModeError::ZeroRefreshRate));
    assert_eq!(RationalHz::from_hz(0), Err(ModeError::ZeroRefreshRate));
    assert_eq!(RationalHz::new(0, 1), Err(ModeError::ZeroRefreshRate));
    assert_eq!(RationalHz::new(60, 0), Err(ModeError::ZeroRefreshRate));
}

#[test]
fn cvt_8k_240hz_and_4k_59_94hz_do_not_overflow_and_satisfy_pixel_rate_bound() {
    let hz_240 = RationalHz::from_hz(240).unwrap();
    let mon_8k = compute_cvt_rb2_monitor_signal(7680, 4320, hz_240).unwrap();
    let tgt_8k = compute_cvt_rb2_target_signal(7680, 4320, hz_240).unwrap();

    assert!(mon_8k.pixel_rate > u64::from(u32::MAX));
    assert!(mon_8k.pixel_rate >= 7680u64 * 4320u64 * 240u64);
    assert_eq!(mon_8k.vsync_freq_divider, 0);
    assert_eq!(tgt_8k.vsync_freq_divider, 1);

    let mut normalized = tgt_8k;
    normalized.vsync_freq_divider = 0;
    assert_eq!(mon_8k, normalized);

    let hz_59_94 = RationalHz::from_millihz(59_940).unwrap();
    let mon_4k = compute_cvt_rb2_monitor_signal(3840, 2160, hz_59_94).unwrap();
    let tgt_4k = compute_cvt_rb2_target_signal(3840, 2160, hz_59_94).unwrap();

    let min_pixel_rate_scaled = 3840u128 * 2160u128 * u128::from(hz_59_94.num());
    assert!(u128::from(mon_4k.pixel_rate) * u128::from(hz_59_94.den()) >= min_pixel_rate_scaled);
    assert_eq!(mon_4k.vsync_freq_divider, 0);
    assert_eq!(tgt_4k.vsync_freq_divider, 1);

    let mut normalized_4k = tgt_4k;
    normalized_4k.vsync_freq_divider = 0;
    assert_eq!(mon_4k, normalized_4k);
}

#[test]
fn cvt_rejects_zero_dimensions_and_excessive_refresh_rates() {
    let hz_60 = RationalHz::from_hz(60).unwrap();
    assert_eq!(
        compute_cvt_rb2_monitor_signal(0, 1080, hz_60),
        Err(ModeError::InvalidDimensions {
            width: 0,
            height: 1080,
        })
    );
    assert_eq!(
        compute_cvt_rb2_target_signal(1920, 0, hz_60),
        Err(ModeError::InvalidDimensions {
            width: 1920,
            height: 0,
        })
    );

    let impossible = RationalHz::from_hz(2500).unwrap();
    assert_eq!(
        compute_cvt_rb2_monitor_signal(1920, 1080, impossible),
        Err(ModeError::RefreshRateTooHigh)
    );
}

#[test]
fn planner_1366x768_rounds_to_even_dimensions_preserves_index0_and_deduplicates() {
    assert_eq!(round_to_even_pixel(1366, 50).unwrap(), 684);
    assert_eq!(round_to_even_pixel(768, 50).unwrap(), 384);

    let hz_60 = RationalHz::from_hz(60).unwrap();
    let hz_120 = RationalHz::from_hz(120).unwrap();
    let hz_144 = RationalHz::from_hz(144).unwrap();

    let mut req = ModePlanRequest::new(1366, 768, hz_60, ColorCapability::Sdr);
    req.extra_refreshes = vec![hz_60, hz_120, hz_144];

    let modes = plan_modes(&req).expect("mode plan must succeed");
    assert!(!modes.is_empty());

    assert_eq!(
        (modes[0].width, modes[0].height, modes[0].refresh),
        (1366, 768, hz_60)
    );
    assert!(modes[0].is_preferred);
    assert!(modes.iter().skip(1).all(|m| !m.is_preferred));

    let mut seen = BTreeSet::new();
    for mode in &modes {
        assert_eq!(mode.width % 2, 0, "odd width {} in plan", mode.width);
        assert_eq!(mode.height % 2, 0, "odd height {} in plan", mode.height);
        assert!(seen.insert((mode.width, mode.height, mode.refresh)));
        assert_eq!(mode.bit_depths, BitDepthMask::SDR_DEFAULT);
        assert_eq!(mode.monitor_signal.vsync_freq_divider, 0);
        assert_eq!(mode.target_signal.vsync_freq_divider, 1);

        let mut normalized = mode.target_signal;
        normalized.vsync_freq_divider = 0;
        assert_eq!(mode.monitor_signal, normalized);
    }

    assert!(seen.contains(&(684, 384, hz_60)));
    assert!(seen.contains(&(1366, 768, hz_120)));
    assert!(seen.contains(&(1366, 768, hz_144)));
}

#[test]
fn planner_hdr_bit_depths_and_ntsc_double_refresh() {
    let hz_59_94 = RationalHz::from_millihz(59_940).unwrap();
    let hz_119_88 = RationalHz::from_millihz(119_880).unwrap();

    let req = ModePlanRequest::new(3840, 2160, hz_59_94, ColorCapability::Hdr);
    let modes = plan_modes(&req).expect("HDR mode plan must succeed");

    assert_eq!(modes[0].refresh, hz_59_94);
    assert_eq!(modes[0].bit_depths, BitDepthMask::HDR_DEFAULT);
    assert!(modes.iter().any(|m| {
        m.width == 3840 && m.height == 2160 && m.refresh == hz_119_88 && !m.is_preferred
    }));
}

#[test]
fn planner_refresh_ladder_exposes_standard_and_cinema_rates_capped_at_preferred() {
    let hz_120 = RationalHz::from_hz(120).unwrap();
    let req = ModePlanRequest::new(3024, 1890, hz_120, ColorCapability::Hdr);
    let modes = plan_modes(&req).expect("120Hz HDR plan must succeed");

    assert_eq!(
        (modes[0].width, modes[0].height, modes[0].refresh),
        (3024, 1890, hz_120)
    );
    assert!(modes[0].is_preferred);

    let native_rates: BTreeSet<RationalHz> = modes
        .iter()
        .filter(|m| m.width == 3024 && m.height == 1890)
        .map(|m| m.refresh)
        .collect();
    for expected_mhz in [
        240_000, 120_000, 119_880, 90_000, 60_000, 59_940, 48_000, 24_000, 23_976,
    ] {
        let r = RationalHz::from_millihz(expected_mhz).unwrap();
        assert!(
            native_rates.contains(&r),
            "expected native resolution to expose {} millihz",
            expected_mhz
        );
    }
    assert!(
        !native_rates.contains(&RationalHz::from_hz(144).unwrap()),
        "144Hz exceeds preferred 120Hz and must not be added by ladder"
    );

    let hz_23_976 = RationalHz::from_millihz(23_976).unwrap();
    let hz_48 = RationalHz::from_hz(48).unwrap();
    assert!(
        modes
            .iter()
            .filter(|m| (m.width, m.height) != (3024, 1890))
            .all(|m| m.refresh != hz_23_976 && m.refresh != hz_48),
        "scaled resolutions must not multiply the full refresh ladder"
    );

    let mut no_ladder = ModePlanRequest::new(3024, 1890, hz_120, ColorCapability::Sdr);
    no_ladder.include_refresh_ladder = false;
    let no_ladder_modes = plan_modes(&no_ladder).expect("no-ladder plan must succeed");
    assert!(no_ladder_modes.iter().all(|m| m.refresh != hz_23_976));

    let hz_59_94 = RationalHz::from_millihz(59_940).unwrap();
    let req_59 = ModePlanRequest::new(3840, 2160, hz_59_94, ColorCapability::Sdr);
    let modes_59 = plan_modes(&req_59).expect("59.94Hz plan must succeed");
    let native_59: BTreeSet<RationalHz> = modes_59
        .iter()
        .filter(|m| m.width == 3840 && m.height == 2160)
        .map(|m| m.refresh)
        .collect();
    assert!(native_59.contains(&hz_48));
    assert!(native_59.contains(&RationalHz::from_hz(24).unwrap()));
    assert!(native_59.contains(&hz_23_976));
    assert!(!native_59.contains(&RationalHz::from_hz(60).unwrap()));
}

proptest! {
    #[test]
    fn prop_cvt_and_planner_invariants_hold(
        width in 320u32..=7680u32,
        height in 240u32..=4320u32,
        millihz in prop_oneof![
            Just(23_976u32),
            Just(29_970u32),
            Just(59_940u32),
            Just(119_880u32),
            Just(239_760u32),
            (24u32..=240u32).prop_map(|hz| hz * 1000),
        ],
        is_hdr in any::<bool>(),
    ) {
        let refresh = RationalHz::from_millihz(millihz).unwrap();
        let cap = if is_hdr { ColorCapability::Hdr } else { ColorCapability::Sdr };
        let req = ModePlanRequest::new(width, height, refresh, cap);
        let modes = plan_modes(&req).expect("valid request must plan");

        prop_assert_eq!(modes[0].width, width);
        prop_assert_eq!(modes[0].height, height);
        prop_assert_eq!(modes[0].refresh, refresh);
        prop_assert!(modes[0].is_preferred);

        let mut seen = BTreeSet::new();
        for (idx, mode) in modes.iter().enumerate() {
            prop_assert!(seen.insert((mode.width, mode.height, mode.refresh)));
            if idx > 0 {
                prop_assert!(!mode.is_preferred);
            }
            if (mode.width, mode.height) != (width, height) {
                prop_assert_eq!(mode.width % 2, 0);
                prop_assert_eq!(mode.height % 2, 0);
            }
            if width.is_multiple_of(2) && height.is_multiple_of(2) {
                prop_assert_eq!(mode.width % 2, 0);
                prop_assert_eq!(mode.height % 2, 0);
            }
            let min_rate = u128::from(mode.width)
                * u128::from(mode.height)
                * u128::from(mode.refresh.num());
            prop_assert!(
                u128::from(mode.monitor_signal.pixel_rate) * u128::from(mode.refresh.den())
                    >= min_rate
            );
            let mut normalized = mode.target_signal;
            normalized.vsync_freq_divider = 0;
            prop_assert_eq!(mode.monitor_signal, normalized);
        }
    }
}

#[test]
fn corpus_regression_replay() {
    let corpus = include_str!("../fixtures/corpus_modes_extreme.txt");
    let mut executed = 0usize;

    for (line_idx, raw_line) in corpus.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split(',').collect();
        assert_eq!(parts.len(), 5, "malformed corpus line {}", line_idx + 1);

        let width: u32 = parts[0].parse().expect("valid width");
        let height: u32 = parts[1].parse().expect("valid height");
        let millihz: u32 = parts[2].parse().expect("valid millihz");
        let is_hdr = parts[3] == "1";
        let expect_ok = parts[4] == "ok";

        let _ = round_to_even_pixel(width, 50);
        let _ = round_to_even_pixel(height, 150);

        let result = RationalHz::from_millihz(millihz).and_then(|refresh| {
            let cap = if is_hdr {
                ColorCapability::Hdr
            } else {
                ColorCapability::Sdr
            };
            let req = ModePlanRequest::new(width, height, refresh, cap);
            plan_modes(&req)
        });

        if expect_ok {
            let modes = result.unwrap_or_else(|e| {
                panic!("line {} expected Ok, got Err({e:?})", line_idx + 1);
            });
            assert!(!modes.is_empty());
            assert!(modes[0].is_preferred);
        } else {
            assert!(
                result.is_err(),
                "line {} expected Err, got Ok",
                line_idx + 1
            );
        }
        executed += 1;
    }

    assert!(executed >= 8, "expected at least 8 corpus cases");
}

#[test]
fn bit_depths_override_controls_planned_mode_masks() {
    let refresh = RationalHz::from_hz(120).unwrap();
    for mask in [
        BitDepthMask::BITS_8,
        BitDepthMask::BITS_10,
        BitDepthMask::BITS_12,
    ] {
        let mut req = ModePlanRequest::new(2560, 1440, refresh, ColorCapability::Hdr);
        req.bit_depths_override = Some(mask);
        let modes = plan_modes(&req).unwrap();
        assert!(!modes.is_empty());
        assert!(modes.iter().all(|m| m.bit_depths == mask));
    }
}
