//! LibFuzzer target for `zerovdd-modes` rational refresh rates, CVT-RBv2 math, and mode planning.

#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;
use zerovdd_modes::{
    compute_cvt_rb2, plan_modes, round_to_even_pixel, ColorCapability, ModePlanRequest, RationalHz,
    VideoSignalRole,
};

#[derive(Debug, Arbitrary)]
struct ModesFuzzInput {
    width: u32,
    height: u32,
    millihz: u32,
    raw_num: u64,
    raw_den: u64,
    ntsc_base_hz: u32,
    scale_percent: u32,
    is_hdr: bool,
    include_scaled_variants: bool,
    include_double_refresh: bool,
    extra_millihz: [u32; 4],
}

fuzz_target!(|data: &[u8]| {
    let mut unstructured = Unstructured::new(data);
    let Ok(input) = ModesFuzzInput::arbitrary(&mut unstructured) else {
        return;
    };

    let _ = RationalHz::new(input.raw_num, input.raw_den);
    let _ = RationalHz::from_hz(input.millihz);
    let _ = RationalHz::from_ntsc_base_hz(input.ntsc_base_hz);
    let _ = round_to_even_pixel(input.width, input.scale_percent);
    let _ = round_to_even_pixel(input.height, input.scale_percent);

    let Ok(refresh) = RationalHz::from_millihz(input.millihz) else {
        return;
    };

    let _ = compute_cvt_rb2(
        input.width,
        input.height,
        refresh,
        VideoSignalRole::MonitorDescription,
    );
    let _ = compute_cvt_rb2(
        input.width,
        input.height,
        refresh,
        VideoSignalRole::TargetMode,
    );

    let mut extra_refreshes = Vec::new();
    for hz in input.extra_millihz {
        if let Ok(r) = RationalHz::from_millihz(hz) {
            extra_refreshes.push(r);
        }
    }

    let req = ModePlanRequest {
        preferred_width: input.width,
        preferred_height: input.height,
        preferred_refresh: refresh,
        color_capability: if input.is_hdr {
            ColorCapability::Hdr
        } else {
            ColorCapability::Sdr
        },
        include_scaled_variants: input.include_scaled_variants,
        include_double_refresh: input.include_double_refresh,
        extra_refreshes,
    };

    if let Ok(modes) = plan_modes(&req) {
        if let Some(first) = modes.first() {
            assert!(first.is_preferred);
        }
    }
});
