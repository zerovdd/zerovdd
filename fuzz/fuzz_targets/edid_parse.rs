//! LibFuzzer target for `zerovdd-edid` parser validation and structured EDID synthesis.

#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;
use zerovdd_edid::{
    synthesize_edid, validate_custom_edid, ColorProfile, ColorimetryFlags, EdidSpec, EotfFlags,
    HdrStaticMetadata,
};

#[derive(Debug, Arbitrary)]
struct HdrInput {
    eotf_raw: u8,
    static_metadata_type1: bool,
    max_luminance_nits: Option<u16>,
    max_fall_nits: Option<u16>,
    min_luminance_ten_thousandths: Option<u32>,
}

#[derive(Debug, Arbitrary)]
struct EdidSynthInput {
    monitor_guid: [u8; 16],
    client_name: String,
    preferred_width: u16,
    preferred_height: u16,
    preferred_refresh_millihz: u32,
    target_ppi: Option<u16>,
    color_profile_idx: u8,
    colorimetry_override: Option<u8>,
    hdr_override: Option<Option<HdrInput>>,
}

fuzz_target!(|data: &[u8]| {
    let _ = validate_custom_edid(data);

    let mut unstructured = Unstructured::new(data);
    if let Ok(input) = EdidSynthInput::arbitrary(&mut unstructured) {
        let color_profile = match input.color_profile_idx % 3 {
            0 => ColorProfile::SdrBt709,
            1 => ColorProfile::HdrBt2020,
            _ => ColorProfile::HdrDciP3,
        };
        let hdr_override = input.hdr_override.map(|opt| {
            opt.map(|h| HdrStaticMetadata {
                eotf: EotfFlags::from_raw(h.eotf_raw),
                static_metadata_type1: h.static_metadata_type1,
                max_luminance_nits: h.max_luminance_nits,
                max_fall_nits: h.max_fall_nits,
                min_luminance_ten_thousandths: h.min_luminance_ten_thousandths,
            })
        });
        let spec = EdidSpec {
            monitor_guid: input.monitor_guid,
            client_name: &input.client_name,
            preferred_width: input.preferred_width,
            preferred_height: input.preferred_height,
            preferred_refresh_millihz: input.preferred_refresh_millihz,
            target_ppi: input.target_ppi,
            color_profile,
            colorimetry_override: input.colorimetry_override.map(ColorimetryFlags::from_raw),
            hdr_override,
        };

        if let Ok(edid) = synthesize_edid(&spec) {
            assert!(
                validate_custom_edid(&edid).is_ok(),
                "synthesized EDID must pass validate_custom_edid"
            );
        }
    }
});
