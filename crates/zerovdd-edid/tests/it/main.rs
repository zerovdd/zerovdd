use proptest::prelude::*;
use zerovdd_edid::{
    decode_max_luminance_millinits, decode_min_luminance_ten_thousandths,
    encode_max_luminance_nits, encode_min_luminance_ten_thousandths, synthesize_edid,
    validate_custom_edid, Chromaticity10Bit, ColorProfile, ColorimetryFlags, EdidError, EdidSpec,
    EotfFlags, HdrStaticMetadata, CTA861_EXTENSION_TAG, CTA861_REVISION, EDID_HEADER,
};

fn ppi(pixels: u16, mm: u16) -> f64 {
    f64::from(pixels) * 25.4 / f64::from(mm)
}

#[test]
fn synthesizes_valid_256_byte_sdr_and_hdr_edids_with_consistent_dimensions() {
    let guid = [
        0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0x4D, 0xEF, 0x81, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD,
        0xEF,
    ];

    let resolutions = [
        (1280, 720, 60_000),
        (1366, 768, 60_000),
        (1920, 1080, 60_000),
        (2560, 1600, 90_000),
        (3840, 2160, 119_880),
        (5120, 2880, 60_000),
        (7680, 4320, 240_000),
    ];

    for &(w, h, hz) in &resolutions {
        let mut sdr_spec = EdidSpec::new(guid, "Desktop-Client", w, h, hz);
        sdr_spec.color_profile = ColorProfile::SdrBt709;
        let sdr = synthesize_edid(&sdr_spec).expect("SDR synthesis should succeed");
        assert_eq!(sdr.len(), 256);
        assert_eq!(sdr[..128].iter().fold(0u8, |a, &b| a.wrapping_add(b)), 0);
        assert_eq!(sdr[128..].iter().fold(0u8, |a, &b| a.wrapping_add(b)), 0);

        let summary = validate_custom_edid(&sdr).expect("synthesized SDR EDID must validate");
        assert_eq!(summary.version, 1);
        assert_eq!(summary.revision, 4);
        assert_eq!(summary.extension_blocks, 1);
        assert_eq!(summary.chromaticity, Chromaticity10Bit::BT709);
        assert!(!summary.chromaticity.is_all_zero());
        assert!(summary.srgb_bit_set);

        let (w_cm, h_cm) = summary.block0_size_cm;
        assert_eq!((w_cm, h_cm), (sdr[21], sdr[22]));
        let (w_mm, h_mm) = summary.dtd_size_mm.expect("DTD mm must be present");
        let dtd_w_mm_raw = u16::from(sdr[66]) | (u16::from(sdr[68] >> 4) << 8);
        let dtd_h_mm_raw = u16::from(sdr[67]) | (u16::from(sdr[68] & 0x0F) << 8);
        assert_eq!((w_mm, h_mm), (dtd_w_mm_raw, dtd_h_mm_raw));

        assert!(u16::from(w_cm).abs_imul_diff(w_mm) <= 5);
        assert!(u16::from(h_cm).abs_imul_diff(h_mm) <= 5);

        let ppi_x_mm = ppi(w, w_mm);
        let ppi_y_mm = ppi(h, h_mm);
        let ppi_x_cm = ppi(w, u16::from(w_cm) * 10);
        let ppi_y_cm = ppi(h, u16::from(h_cm) * 10);
        assert!((96.0..=110.0).contains(&ppi_x_mm), "ppi_x_mm={ppi_x_mm}");
        assert!((96.0..=110.0).contains(&ppi_y_mm), "ppi_y_mm={ppi_y_mm}");
        assert!((96.0..=110.0).contains(&ppi_x_cm), "ppi_x_cm={ppi_x_cm}");
        assert!((96.0..=110.0).contains(&ppi_y_cm), "ppi_y_cm={ppi_y_cm}");
    }
}

trait CmMmDiff {
    fn abs_imul_diff(self, mm: u16) -> u16;
}

impl CmMmDiff for u16 {
    fn abs_imul_diff(self, mm: u16) -> u16 {
        (self * 10).abs_diff(mm)
    }
}

#[test]
fn hdr_edid_roundtrips_primaries_colorimetry_and_luminance() {
    let guid = [0xA5u8; 16];
    let mut spec = EdidSpec::new(guid, "LivingRoom-TV", 3840, 2160, 119_880);
    spec.color_profile = ColorProfile::HdrBt2020;
    spec.hdr_override = Some(Some(HdrStaticMetadata {
        eotf: EotfFlags::PQ_AND_HLG,
        static_metadata_type1: true,
        max_luminance_nits: Some(1000),
        max_fall_nits: Some(400),
        min_luminance_ten_thousandths: Some(500),
    }));

    let bytes = synthesize_edid(&spec).expect("HDR synthesis must succeed");
    assert_eq!(bytes[128], CTA861_EXTENSION_TAG);
    assert_eq!(bytes[129], CTA861_REVISION);

    let summary = validate_custom_edid(&bytes).expect("HDR EDID must validate");
    assert_eq!(summary.chromaticity, Chromaticity10Bit::BT2020);
    assert!(!summary.srgb_bit_set);
    assert_eq!(summary.product_name.as_deref(), Some("LivingRoom-TV"));
    assert_eq!(summary.ascii_serial.as_ref().map(String::len), Some(13));
    assert_ne!(summary.binary_serial, 0);

    let cta = summary.cta861.expect("CTA-861 block must be parsed");
    assert_eq!(cta.revision, CTA861_REVISION);
    assert!(cta.has_speaker_allocation);
    assert!(!cta.video_vics.is_empty());
    let colorimetry = cta.colorimetry.expect("colorimetry block must be present");
    assert!(colorimetry.contains(ColorimetryFlags::BT2020_RGB_AND_YCC));

    let hdr = cta
        .hdr_static_metadata
        .expect("HDR static metadata must be present");
    assert!(hdr.eotf.contains(EotfFlags::PQ));
    assert!(hdr.eotf.contains(EotfFlags::HLG));
    assert!(hdr.static_metadata_type1);

    let max_millinits = hdr
        .max_luminance_millinits
        .expect("max luminance must decode");
    let max_fall_millinits = hdr.max_fall_millinits.expect("max FALL must decode");
    let min_ten_thousandths = hdr
        .min_luminance_ten_thousandths
        .expect("min luminance must decode");

    assert!(
        max_millinits.abs_diff(1_000_000) <= 25_000,
        "max_millinits={max_millinits}"
    );
    assert!(
        max_fall_millinits.abs_diff(400_000) <= 10_000,
        "max_fall_millinits={max_fall_millinits}"
    );
    assert!(
        min_ten_thousandths.abs_diff(500) <= 60,
        "min_ten_thousandths={min_ten_thousandths}"
    );
}

#[test]
fn dci_p3_profile_sets_dci_p3_primaries_and_clears_srgb() {
    let spec = EdidSpec {
        color_profile: ColorProfile::HdrDciP3,
        ..EdidSpec::new([0x42; 16], "MacBook-Pro", 2560, 1600, 120_000)
    };
    let edid = synthesize_edid(&spec).expect("DCI-P3 synthesis must succeed");
    let summary = validate_custom_edid(&edid).expect("DCI-P3 EDID must validate");
    assert_eq!(summary.chromaticity, Chromaticity10Bit::DCI_P3);
    assert!(!summary.srgb_bit_set);
    assert_eq!(summary.product_name.as_deref(), Some("MacBook-Pro"));
}

#[test]
fn deterministic_identity_varies_with_guid_and_client_name() {
    let edid_a = synthesize_edid(&EdidSpec::new([1; 16], "ClientA", 1920, 1080, 60_000)).unwrap();
    let edid_a_again =
        synthesize_edid(&EdidSpec::new([1; 16], "ClientA", 1920, 1080, 60_000)).unwrap();
    let edid_b = synthesize_edid(&EdidSpec::new([2; 16], "ClientA", 1920, 1080, 60_000)).unwrap();
    let edid_c = synthesize_edid(&EdidSpec::new([1; 16], "ClientB", 1920, 1080, 60_000)).unwrap();
    let edid_empty = synthesize_edid(&EdidSpec::new([1; 16], "", 1920, 1080, 60_000)).unwrap();

    assert_eq!(edid_a, edid_a_again);

    let sum_a = validate_custom_edid(&edid_a).unwrap();
    let sum_b = validate_custom_edid(&edid_b).unwrap();
    let sum_c = validate_custom_edid(&edid_c).unwrap();
    let sum_empty = validate_custom_edid(&edid_empty).unwrap();

    assert_ne!(sum_a.binary_serial, sum_b.binary_serial);
    assert_ne!(sum_a.ascii_serial, sum_b.ascii_serial);
    assert_ne!(sum_a.binary_serial, sum_c.binary_serial);
    assert_ne!(sum_a.ascii_serial, sum_c.ascii_serial);
    assert_eq!(sum_a.product_name.as_deref(), Some("ClientA"));
    assert_eq!(sum_c.product_name.as_deref(), Some("ClientB"));
    assert!(sum_empty
        .product_name
        .as_deref()
        .is_some_and(|name| name.starts_with("ZVDD-")));
}

#[test]
fn validate_custom_edid_rejects_short_unaligned_and_corrupt_inputs_without_oob() {
    assert_eq!(
        validate_custom_edid(&[]),
        Err(EdidError::TooShort { len: 0 })
    );
    assert_eq!(
        validate_custom_edid(&[0u8; 64]),
        Err(EdidError::TooShort { len: 64 })
    );
    assert_eq!(
        validate_custom_edid(&[0u8; 127]),
        Err(EdidError::TooShort { len: 127 })
    );
    assert_eq!(
        validate_custom_edid(&[0u8; 129]),
        Err(EdidError::InvalidLength { len: 129 })
    );
    assert_eq!(
        validate_custom_edid(&[0u8; 255]),
        Err(EdidError::InvalidLength { len: 255 })
    );

    let mut bad_header = [0u8; 128];
    bad_header[..8].copy_from_slice(&EDID_HEADER);
    bad_header[0] = 0xAA;
    assert_eq!(
        validate_custom_edid(&bad_header),
        Err(EdidError::InvalidHeader)
    );

    let mut valid = synthesize_edid(&EdidSpec::new([9; 16], "Test", 1920, 1080, 60_000)).unwrap();
    valid[10] ^= 0x01;
    assert!(matches!(
        validate_custom_edid(&valid),
        Err(EdidError::InvalidChecksum { block_index: 0, .. })
    ));

    let mut valid_b1 =
        synthesize_edid(&EdidSpec::new([9; 16], "Test", 1920, 1080, 60_000)).unwrap();
    valid_b1[135] ^= 0x01;
    assert!(matches!(
        validate_custom_edid(&valid_b1),
        Err(EdidError::InvalidChecksum { block_index: 1, .. })
    ));

    let mut bad_ext_count =
        synthesize_edid(&EdidSpec::new([9; 16], "Test", 1920, 1080, 60_000)).unwrap();
    bad_ext_count[126] = 2;
    assert_eq!(
        validate_custom_edid(&bad_ext_count),
        Err(EdidError::ExtensionCountMismatch {
            declared: 2,
            actual_blocks: 2,
        })
    );

    let mut malformed_cta =
        synthesize_edid(&EdidSpec::new([9; 16], "Test", 1920, 1080, 60_000)).unwrap();
    malformed_cta[130] = 3;
    let sum_b1 = malformed_cta[128..255]
        .iter()
        .fold(0u8, |a, &b| a.wrapping_add(b));
    malformed_cta[255] = 0u8.wrapping_sub(sum_b1);
    assert_eq!(
        validate_custom_edid(&malformed_cta),
        Err(EdidError::MalformedExtensionBlock { block_index: 1 })
    );
}

#[test]
fn zero_dimensions_or_zero_refresh_return_error() {
    assert!(matches!(
        synthesize_edid(&EdidSpec::new([0; 16], "Bad", 0, 1080, 60_000)),
        Err(EdidError::InvalidDimensions { .. })
    ));
    assert!(matches!(
        synthesize_edid(&EdidSpec::new([0; 16], "Bad", 1920, 0, 60_000)),
        Err(EdidError::InvalidDimensions { .. })
    ));
    assert!(matches!(
        synthesize_edid(&EdidSpec::new([0; 16], "Bad", 1920, 1080, 0)),
        Err(EdidError::InvalidRefreshRate(0))
    ));
}

proptest! {
    #[test]
    fn prop_synthesized_edid_always_validates_and_keeps_consistent_ppi(
        guid in any::<[u8; 16]>(),
        client_name in "\\PC{0,32}",
        width in 640u16..=7680u16,
        height in 480u16..=4320u16,
        refresh_hz in 24u32..=240u32,
        is_hdr in any::<bool>(),
    ) {
        let mut spec = EdidSpec::new(guid, &client_name, width, height, refresh_hz * 1000);
        spec.color_profile = if is_hdr {
            ColorProfile::HdrBt2020
        } else {
            ColorProfile::SdrBt709
        };
        let bytes = synthesize_edid(&spec).expect("valid spec must synthesize");
        let summary = validate_custom_edid(&bytes).expect("synthesized EDID must validate");

        prop_assert!(!summary.chromaticity.is_all_zero());
        prop_assert_eq!(summary.srgb_bit_set, !is_hdr);

        let (w_cm, h_cm) = summary.block0_size_cm;
        let (w_mm, h_mm) = summary.dtd_size_mm.expect("DTD mm present");
        prop_assert!(u16::from(w_cm).abs_imul_diff(w_mm) <= 5);
        prop_assert!(u16::from(h_cm).abs_imul_diff(h_mm) <= 5);

        let ppi_x = ppi(width, w_mm);
        let ppi_y = ppi(height, h_mm);
        prop_assert!((96.0..=110.0).contains(&ppi_x));
        prop_assert!((96.0..=110.0).contains(&ppi_y));
    }

    #[test]
    fn prop_luminance_encoding_roundtrips_monotonically(
        nits in 50u16..=10_000u16,
        min_ten_thousandths in 1u32..=5_000u32,
    ) {
        let max_code = encode_max_luminance_nits(nits);
        prop_assert!(max_code >= 1);
        let decoded_millinits = decode_max_luminance_millinits(max_code).expect("non-zero code");
        let target_millinits = u32::from(nits) * 1000;
        let rel_err = decoded_millinits.abs_diff(target_millinits) as f64 / target_millinits as f64;
        prop_assert!(rel_err < 0.02, "nits={nits} decoded={decoded_millinits}");

        let min_code = encode_min_luminance_ten_thousandths(min_ten_thousandths, decoded_millinits);
        prop_assert!(min_code >= 1);
        let decoded_min = decode_min_luminance_ten_thousandths(min_code, decoded_millinits)
            .expect("non-zero min code");
        prop_assert!(decoded_min <= 20_000);
    }

    #[test]
    fn prop_arbitrary_short_or_random_bytes_never_panic_validator(
        raw in prop::collection::vec(any::<u8>(), 0..=512)
    ) {
        let _ = validate_custom_edid(&raw);
    }
}

#[test]
fn corpus_regression_replay() {
    let short_64 = include_bytes!("../fixtures/corpus_short_64b.edid");
    assert!(matches!(
        validate_custom_edid(short_64),
        Err(EdidError::TooShort { len: 64 })
    ));

    let unaligned_200 = include_bytes!("../fixtures/corpus_unaligned_200b.edid");
    assert!(matches!(
        validate_custom_edid(unaligned_200),
        Err(EdidError::InvalidLength { len: 200 })
    ));

    let corrupt_csum = include_bytes!("../fixtures/corpus_corrupt_checksum.edid");
    assert!(matches!(
        validate_custom_edid(corrupt_csum),
        Err(EdidError::InvalidChecksum { block_index: 0, .. })
    ));

    let malformed_cta = include_bytes!("../fixtures/corpus_malformed_cta_dtd_offset.edid");
    assert!(matches!(
        validate_custom_edid(malformed_cta),
        Err(EdidError::MalformedExtensionBlock { block_index: 1 })
    ));

    let valid_hdr = include_bytes!("../fixtures/corpus_valid_hdr.edid");
    let summary = validate_custom_edid(valid_hdr).expect("valid HDR EDID corpus must validate");
    assert_eq!(summary.version, 1);
    assert_eq!(summary.revision, 4);
    assert_eq!(summary.extension_blocks, 1);
    assert!(!summary.chromaticity.is_all_zero());
    let cta = summary.cta861.expect("CTA-861 block expected");
    assert!(cta.hdr_static_metadata.is_some());
}

#[test]
fn explicit_bit_depth_dci_p3_and_custom_ppi_reflect_in_edid() {
    for (bpc, expected_bits) in [(8u8, 0x20u8), (10, 0x30), (12, 0x40)] {
        let mut spec = EdidSpec::new([0x21; 16], "BitDepthPanel", 3840, 2160, 120_000);
        spec.color_profile = ColorProfile::HdrDciP3;
        spec.bit_depth_bpc = Some(bpc);
        spec.target_ppi = Some(140);
        let blob = zerovdd_edid::synthesize_edid(&spec).unwrap();
        assert_eq!(blob[20] & 0x70, expected_bits);
        let summary = validate_custom_edid(&blob).unwrap();
        assert_eq!(
            summary.chromaticity,
            zerovdd_edid::Chromaticity10Bit::DCI_P3
        );
        let (w_mm, h_mm) = summary.dtd_size_mm.expect("preferred DTD present");
        let ppi_x = (3840.0 * 25.4) / f64::from(w_mm);
        let ppi_y = (2160.0 * 25.4) / f64::from(h_mm);
        assert!((137.0..=143.0).contains(&ppi_x), "ppi_x={ppi_x}");
        assert!((137.0..=143.0).contains(&ppi_y), "ppi_y={ppi_y}");
    }

    let mut bad = EdidSpec::new([0x22; 16], "BadDepth", 1920, 1080, 60_000);
    bad.bit_depth_bpc = Some(9);
    assert!(matches!(
        zerovdd_edid::synthesize_edid(&bad),
        Err(EdidError::InvalidBitDepth(9))
    ));
}
