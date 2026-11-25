use zerovdd_edid::{synthesize_edid, validate_custom_edid, Chromaticity10Bit, EdidSpec};
use zerovdd_engine::{
    dispatch_ioctl, EngineError, FakeIddPlatform, FileKey, MonitorQueryKey, VddEngine, MAX_MONITORS,
};
use zerovdd_modes::BitDepthMask;
use zerovdd_proto::ioctl::{
    IOCTL_ADD_DISPLAY, IOCTL_GET_STATUS, IOCTL_GET_VERSION, IOCTL_LIST_DISPLAYS,
    IOCTL_REMOVE_DISPLAY, IOCTL_RENEW_LEASE, IOCTL_SET_RENDER_ADAPTER, IOCTL_UPDATE_DISPLAY_MODE,
};
use zerovdd_proto::types::{
    AddDisplayRequest, AddDisplayResponse, BitDepthPreference, ColorSpacePreference,
    DriverStatusResponse, HdrMasteringMetadata, HdrProfile, ListDisplaysResponse, OwnershipMode,
    ProtocolVersion, RemoveDisplayRequest, RenewLeaseRequest, SetRenderAdapterRequest,
    UpdateDisplayModeRequest, UpdateDisplayModeResponse,
};
use zerovdd_proto::validation::ProtoError;

#[test]
fn per_handle_cleanup_isolates_clients_and_preserves_leased_monitors() {
    let engine = VddEngine::new();
    let platform = FakeIddPlatform::new();

    let client_a = FileKey(101);
    let client_b = FileKey(202);
    let guid_a = [0xA1; 16];
    let guid_b = [0xB2; 16];
    let guid_leased = [0xC3; 16];

    let req_a = AddDisplayRequest::new(
        guid_a,
        1920,
        1080,
        60,
        1,
        HdrProfile::Sdr,
        OwnershipMode::HandleBound,
    );
    let req_b = AddDisplayRequest::new(
        guid_b,
        2560,
        1440,
        120,
        1,
        HdrProfile::Hdr10,
        OwnershipMode::HandleBound,
    );
    let req_leased = AddDisplayRequest::new(
        guid_leased,
        1280,
        720,
        60,
        1,
        HdrProfile::Sdr,
        OwnershipMode::Leased,
    );

    engine.add_display(client_a, &req_a, &platform).unwrap();
    engine.add_display(client_b, &req_b, &platform).unwrap();
    engine
        .add_display(client_a, &req_leased, &platform)
        .unwrap();
    assert_eq!(engine.active_display_count(), 3);

    // WHY: Simulating Client A crashing/closing handle must remove ONLY Client A's HandleBound monitor.
    let departed = engine.on_file_cleanup(client_a, &platform).unwrap();
    assert_eq!(departed, 1);
    assert_eq!(engine.active_display_count(), 2);

    let list = engine.list_displays(&platform);
    let entries = list.entries().unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|d| d.monitor_guid == guid_b));
    assert!(entries.iter().any(|d| d.monitor_guid == guid_leased));
    assert!(!entries.iter().any(|d| d.monitor_guid == guid_a));
}

#[test]
fn idempotent_reconnect_reuses_slot_and_rejects_handle_conflict() {
    let engine = VddEngine::new();
    let platform = FakeIddPlatform::new();

    let client_a = FileKey(1);
    let client_b = FileKey(2);
    let guid = [0x55; 16];

    let req = AddDisplayRequest::new(
        guid,
        3840,
        2160,
        120_000,
        1001,
        HdrProfile::Hdr10,
        OwnershipMode::HandleBound,
    );

    let first = engine.add_display(client_a, &req, &platform).unwrap();
    assert_eq!(first.created_new, 1);

    let second = engine.add_display(client_a, &req, &platform).unwrap();
    assert_eq!(second.created_new, 0);
    assert_eq!(second.adapter_luid_low, first.adapter_luid_low);
    assert_eq!(second.adapter_luid_high, first.adapter_luid_high);
    assert_eq!(second.target_id, first.target_id);
    assert_eq!(second.connector_index, first.connector_index);
    assert_eq!(engine.active_display_count(), 1);
    assert_eq!(platform.create_monitor_calls(), 1);

    let conflict = engine.add_display(client_b, &req, &platform);
    assert_eq!(conflict, Err(EngineError::MonitorConflict));
}

#[test]
fn custom_edid_validation_accepts_valid_and_rejects_corrupt_checksum() {
    let engine = VddEngine::new();
    let platform = FakeIddPlatform::new();
    let guid = [0x66; 16];

    let edid_spec = EdidSpec::new(guid, "CustomPanel", 1920, 1080, 60_000);
    let mut edid = synthesize_edid(&edid_spec).unwrap();

    let mut req = AddDisplayRequest::new(
        guid,
        1920,
        1080,
        60,
        1,
        HdrProfile::Sdr,
        OwnershipMode::HandleBound,
    );
    req.set_custom_edid(&edid).unwrap();
    let ok = engine.add_display(FileKey(9), &req, &platform).unwrap();
    assert_eq!(ok.created_new, 1);

    edid[127] ^= 0xFF;
    let mut bad_req = AddDisplayRequest::new(
        [0x67; 16],
        1920,
        1080,
        60,
        1,
        HdrProfile::Sdr,
        OwnershipMode::HandleBound,
    );
    bad_req.set_custom_edid(&edid).unwrap();
    let err = engine
        .add_display(FileKey(9), &bad_req, &platform)
        .expect_err("corrupt custom EDID checksum must be rejected");
    assert!(matches!(err, EngineError::Edid(_)));
}

#[test]
fn ioctl_dispatch_roundtrips_and_rejects_invalid_buffers_and_versions() {
    let engine = VddEngine::new();
    let platform = FakeIddPlatform::new();
    let file_key = FileKey(42);

    assert!(engine.on_d0_entry(&platform).unwrap());
    engine.set_hdr_capable(true);

    let mut ver_buf = [0u8; ProtocolVersion::WIRE_SIZE];
    let written = dispatch_ioctl(
        &engine,
        file_key,
        IOCTL_GET_VERSION,
        &[],
        &mut ver_buf,
        &platform,
    )
    .unwrap();
    assert_eq!(written, ProtocolVersion::WIRE_SIZE);
    assert_eq!(
        ProtocolVersion::from_bytes(&ver_buf).unwrap(),
        ProtocolVersion::CURRENT
    );

    let mut short_buf = [0u8; 4];
    assert!(matches!(
        dispatch_ioctl(
            &engine,
            file_key,
            IOCTL_GET_VERSION,
            &[],
            &mut short_buf,
            &platform
        ),
        Err(EngineError::Proto(ProtoError::BufferTooShort { .. }))
    ));

    let guid = [0x77; 16];
    let mut add_req = AddDisplayRequest::new(
        guid,
        2560,
        1440,
        144,
        1,
        HdrProfile::Hdr10,
        OwnershipMode::Leased,
    );
    add_req.set_client_name("LivingRoomTV").unwrap();
    add_req.set_serial("TV001").unwrap();

    let mut add_out = [0u8; AddDisplayResponse::WIRE_SIZE];
    let add_written = dispatch_ioctl(
        &engine,
        file_key,
        IOCTL_ADD_DISPLAY,
        &add_req.to_bytes(),
        &mut add_out,
        &platform,
    )
    .unwrap();
    assert_eq!(add_written, AddDisplayResponse::WIRE_SIZE);
    let add_resp = AddDisplayResponse::from_bytes(&add_out).unwrap();
    assert_eq!(add_resp.created_new, 1);

    let mut list_out = [0u8; ListDisplaysResponse::WIRE_SIZE];
    let list_written = dispatch_ioctl(
        &engine,
        file_key,
        IOCTL_LIST_DISPLAYS,
        &[],
        &mut list_out,
        &platform,
    )
    .unwrap();
    assert_eq!(list_written, ListDisplaysResponse::WIRE_SIZE);
    let list_resp = ListDisplaysResponse::from_bytes(&list_out).unwrap();
    assert_eq!(list_resp.count, 1);
    assert_eq!(
        list_resp.entries().unwrap()[0].client_name_str().unwrap(),
        "LivingRoomTV"
    );

    let renew_req = RenewLeaseRequest::new(guid, 120);
    assert_eq!(
        dispatch_ioctl(
            &engine,
            file_key,
            IOCTL_RENEW_LEASE,
            &renew_req.to_bytes(),
            &mut [],
            &platform
        )
        .unwrap(),
        0
    );

    let gpu_req = SetRenderAdapterRequest::new(0xABCD, 1);
    assert_eq!(
        dispatch_ioctl(
            &engine,
            file_key,
            IOCTL_SET_RENDER_ADAPTER,
            &gpu_req.to_bytes(),
            &mut [],
            &platform
        )
        .unwrap(),
        0
    );
    assert_eq!(platform.current_render_adapter().low, 0xABCD);

    let mut status_out = [0u8; DriverStatusResponse::WIRE_SIZE];
    assert_eq!(
        dispatch_ioctl(
            &engine,
            file_key,
            IOCTL_GET_STATUS,
            &[],
            &mut status_out,
            &platform
        )
        .unwrap(),
        DriverStatusResponse::WIRE_SIZE
    );
    let status = DriverStatusResponse::from_bytes(&status_out).unwrap();
    assert_eq!(status.active_display_count, 1);
    assert_eq!(status.max_displays, MAX_MONITORS as u32);
    assert_eq!(status.adapter_ready, 1);
    assert_eq!(status.hdr_capable, 1);

    let remove_req = RemoveDisplayRequest::by_target_id(add_resp.target_id);
    assert_eq!(
        dispatch_ioctl(
            &engine,
            file_key,
            IOCTL_REMOVE_DISPLAY,
            &remove_req.to_bytes(),
            &mut [],
            &platform
        )
        .unwrap(),
        0
    );
    assert_eq!(engine.active_display_count(), 0);

    // WHY: Reject unsupported major version and unknown IOCTL codes cleanly.
    let mut bad_ver_req = add_req;
    bad_ver_req.protocol_version.major = 99;
    let err_major = dispatch_ioctl(
        &engine,
        file_key,
        IOCTL_ADD_DISPLAY,
        &bad_ver_req.to_bytes(),
        &mut add_out,
        &platform,
    );
    assert!(matches!(
        err_major,
        Err(EngineError::Proto(ProtoError::UnsupportedMajorVersion {
            expected: 1,
            actual: 99
        }))
    ));

    assert_eq!(
        dispatch_ioctl(&engine, file_key, 0xFFFF_FFFF, &[], &mut [], &platform),
        Err(EngineError::UnsupportedIoctl(0xFFFF_FFFF))
    );
}

#[test]
fn oversized_ioctl_output_buffers_write_exact_wire_size_without_clobbering_tail() {
    let engine = VddEngine::new();
    let platform = FakeIddPlatform::new();
    let file_key = FileKey(77);

    let mut ver_buf = [0xAAu8; ProtocolVersion::WIRE_SIZE + 32];
    let ver_written = dispatch_ioctl(
        &engine,
        file_key,
        IOCTL_GET_VERSION,
        &[],
        &mut ver_buf,
        &platform,
    )
    .unwrap();
    assert_eq!(ver_written, ProtocolVersion::WIRE_SIZE);
    assert_eq!(
        ProtocolVersion::from_bytes(&ver_buf[..ProtocolVersion::WIRE_SIZE]).unwrap(),
        ProtocolVersion::CURRENT
    );
    assert!(ver_buf[ProtocolVersion::WIRE_SIZE..]
        .iter()
        .all(|&b| b == 0xAA));

    let add_req = AddDisplayRequest::new(
        [0x88; 16],
        1920,
        1080,
        60,
        1,
        HdrProfile::Sdr,
        OwnershipMode::HandleBound,
    );
    let mut add_buf = [0xBBu8; AddDisplayResponse::WIRE_SIZE + 24];
    let add_written = dispatch_ioctl(
        &engine,
        file_key,
        IOCTL_ADD_DISPLAY,
        &add_req.to_bytes(),
        &mut add_buf,
        &platform,
    )
    .unwrap();
    assert_eq!(add_written, AddDisplayResponse::WIRE_SIZE);
    assert_eq!(
        AddDisplayResponse::from_bytes(&add_buf[..AddDisplayResponse::WIRE_SIZE])
            .unwrap()
            .created_new,
        1
    );
    assert!(add_buf[AddDisplayResponse::WIRE_SIZE..]
        .iter()
        .all(|&b| b == 0xBB));

    let mut list_buf = vec![0xCCu8; ListDisplaysResponse::WIRE_SIZE + 64];
    let list_written = dispatch_ioctl(
        &engine,
        file_key,
        IOCTL_LIST_DISPLAYS,
        &[],
        &mut list_buf,
        &platform,
    )
    .unwrap();
    assert_eq!(list_written, ListDisplaysResponse::WIRE_SIZE);
    assert_eq!(
        ListDisplaysResponse::from_bytes(&list_buf[..ListDisplaysResponse::WIRE_SIZE])
            .unwrap()
            .count,
        1
    );
    assert!(list_buf[ListDisplaysResponse::WIRE_SIZE..]
        .iter()
        .all(|&b| b == 0xCC));

    let mut status_buf = [0xDDu8; DriverStatusResponse::WIRE_SIZE + 32];
    let status_written = dispatch_ioctl(
        &engine,
        file_key,
        IOCTL_GET_STATUS,
        &[],
        &mut status_buf,
        &platform,
    )
    .unwrap();
    assert_eq!(status_written, DriverStatusResponse::WIRE_SIZE);
    assert_eq!(
        DriverStatusResponse::from_bytes(&status_buf[..DriverStatusResponse::WIRE_SIZE])
            .unwrap()
            .active_display_count,
        1
    );
    assert!(status_buf[DriverStatusResponse::WIRE_SIZE..]
        .iter()
        .all(|&b| b == 0xDD));
}

#[test]
fn update_display_mode_uses_in_place_for_compatible_and_rearrives_for_hdr_or_new_resolution() {
    use zerovdd_engine::MonitorQueryKey;

    let engine = VddEngine::new();
    let platform = FakeIddPlatform::new();
    let file_key = FileKey(11);
    let guid = [0x42; 16];

    let add_req = AddDisplayRequest::new(
        guid,
        3024,
        1890,
        120,
        1,
        HdrProfile::Sdr,
        OwnershipMode::HandleBound,
    );
    let added = engine.add_display(file_key, &add_req, &platform).unwrap();
    let departs_before = platform.depart_monitor_calls();

    // WHY: Same resolution and refresh within the arrival ladder stays in-place without departing.
    let in_place_req = UpdateDisplayModeRequest::by_guid(guid, 3024, 1890, 60, 1, HdrProfile::Sdr);
    let mut out_buf = [0u8; UpdateDisplayModeResponse::WIRE_SIZE];
    let written = dispatch_ioctl(
        &engine,
        file_key,
        IOCTL_UPDATE_DISPLAY_MODE,
        &in_place_req.to_bytes(),
        &mut out_buf,
        &platform,
    )
    .unwrap();
    assert_eq!(written, UpdateDisplayModeResponse::WIRE_SIZE);
    let resp = UpdateDisplayModeResponse::from_bytes(&out_buf).unwrap();
    assert_eq!(resp.target_id, added.target_id);
    assert!(resp.mode_count >= 4);
    assert_eq!(platform.update_monitor_modes_calls(), 1);
    assert_eq!(platform.depart_monitor_calls(), departs_before);

    platform.set_fail_update_monitor_modes(true);
    let failed_in_place =
        UpdateDisplayModeRequest::by_target_id(added.target_id, 3024, 1890, 90, 1, HdrProfile::Sdr);
    let err = engine
        .update_display_mode(file_key, &failed_in_place, &platform)
        .unwrap_err();
    assert!(matches!(err, EngineError::Platform(_)));
    let list = engine.list_displays(&platform);
    let rolled_back = &list.entries().unwrap()[0];
    assert_eq!((rolled_back.refresh_num, rolled_back.refresh_den), (60, 1));
    platform.set_fail_update_monitor_modes(false);

    // WHY: Out-of-EDID resolution or SDR-to-HDR10 upgrade requires transparent re-arrival on the same slot.
    let rearrive_req =
        UpdateDisplayModeRequest::by_guid(guid, 2560, 1440, 60_000, 1001, HdrProfile::Hdr10);
    let resp2 = engine
        .update_display_mode(file_key, &rearrive_req, &platform)
        .unwrap();
    assert!(resp2.mode_count >= 4);
    assert_eq!(platform.depart_monitor_calls(), departs_before + 1);
    assert_eq!(platform.update_monitor_modes_calls(), 2);

    let list = engine.list_displays(&platform);
    let info = &list.entries().unwrap()[0];
    assert_eq!((info.width, info.height), (2560, 1440));
    assert_eq!((info.refresh_num, info.refresh_den), (60_000, 1001));
    assert_eq!(info.hdr_profile, HdrProfile::Hdr10.as_u8());

    let query_modes = engine
        .on_query_target_modes(MonitorQueryKey::MonitorGuid(guid))
        .unwrap();
    assert_eq!(query_modes.planned_modes[0].width, 2560);
    assert_eq!(query_modes.planned_modes[0].height, 1440);
    assert_eq!(query_modes.planned_modes[0].refresh.num(), 60_000);
    assert_eq!(query_modes.planned_modes[0].refresh.den(), 1001);
    assert!(
        query_modes.spec.edid.len() == 256,
        "HDR10 re-arrival should synthesize CTA-861 extension block"
    );

    let parsed = engine
        .on_parse_monitor_description(MonitorQueryKey::MonitorGuid(guid))
        .unwrap();
    assert_eq!(parsed.preferred_mode.width, 2560);
    assert_eq!(parsed.preferred_mode.height, 1440);

    let missing =
        UpdateDisplayModeRequest::by_target_id(999_999, 1920, 1080, 60, 1, HdrProfile::Sdr);
    assert!(matches!(
        engine.update_display_mode(file_key, &missing, &platform),
        Err(EngineError::MonitorNotFound)
    ));
}

#[test]
fn update_display_mode_defers_concurrent_departure_until_update_finishes() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread;
    use zerovdd_engine::{
        AdapterLuid, ConnectorSlot, IddPlatform, MonitorArrivalResult, MonitorHandle, PlatformError,
    };
    use zerovdd_modes::PlannedMode;

    struct BlockingUpdatePlatform {
        inner: FakeIddPlatform,
        updating: AtomicBool,
        entered_tx: mpsc::SyncSender<()>,
        release_rx: Mutex<mpsc::Receiver<()>>,
    }

    impl IddPlatform for BlockingUpdatePlatform {
        fn init_adapter(&self) -> Result<(), PlatformError> {
            self.inner.init_adapter()
        }

        fn create_monitor(
            &self,
            slot: ConnectorSlot,
            monitor_guid: [u8; 16],
            edid: &[u8],
        ) -> Result<MonitorHandle, PlatformError> {
            self.inner.create_monitor(slot, monitor_guid, edid)
        }

        fn arrive_monitor(
            &self,
            handle: MonitorHandle,
            slot: ConnectorSlot,
            monitor_guid: [u8; 16],
        ) -> Result<MonitorArrivalResult, PlatformError> {
            self.inner.arrive_monitor(handle, slot, monitor_guid)
        }

        fn depart_monitor(&self, handle: MonitorHandle) -> Result<(), PlatformError> {
            assert!(
                !self.updating.load(Ordering::Acquire),
                "depart_monitor must not run while update_monitor_modes is in flight"
            );
            self.inner.depart_monitor(handle)
        }

        fn update_monitor_modes(
            &self,
            handle: MonitorHandle,
            planned_modes: &[PlannedMode],
        ) -> Result<(), PlatformError> {
            self.updating.store(true, Ordering::Release);
            let _ = self.entered_tx.send(());
            self.release_rx
                .lock()
                .expect("release lock")
                .recv()
                .expect("release signal");
            self.updating.store(false, Ordering::Release);
            self.inner.update_monitor_modes(handle, planned_modes)
        }

        fn set_render_adapter(&self, luid: AdapterLuid) -> Result<(), PlatformError> {
            self.inner.set_render_adapter(luid)
        }

        fn now_monotonic_ms(&self) -> u64 {
            self.inner.now_monotonic_ms()
        }
    }

    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let inner = FakeIddPlatform::new();
    inner.set_now_ms(1_000);

    let platform = Arc::new(BlockingUpdatePlatform {
        inner,
        updating: AtomicBool::new(false),
        entered_tx,
        release_rx: Mutex::new(release_rx),
    });

    let engine = Arc::new(VddEngine::new());
    let file_key = FileKey(77);
    let guid = [0x77; 16];
    let mut add_req = AddDisplayRequest::new(
        guid,
        1920,
        1080,
        120,
        1,
        HdrProfile::Sdr,
        OwnershipMode::Leased,
    );
    add_req.lease_ttl_secs = 5;
    engine
        .add_display(file_key, &add_req, platform.as_ref())
        .unwrap();

    let engine_bg = Arc::clone(&engine);
    let platform_bg = Arc::clone(&platform);
    let handle = thread::spawn(move || {
        let req = UpdateDisplayModeRequest::by_guid(guid, 1920, 1080, 60, 1, HdrProfile::Sdr);
        engine_bg.update_display_mode(file_key, &req, platform_bg.as_ref())
    });

    entered_rx.recv().unwrap();

    platform.inner.set_now_ms(10_000);
    let immediate_reap = engine.reap_expired_leases(platform.as_ref()).unwrap();
    assert_eq!(immediate_reap, 0);
    assert_eq!(platform.inner.depart_monitor_calls(), 0);
    assert_eq!(engine.list_displays(platform.as_ref()).total_active, 1);

    release_tx.send(()).unwrap();
    let update_err = handle.join().unwrap().unwrap_err();
    assert!(matches!(update_err, EngineError::MonitorNotFound));
    assert_eq!(platform.inner.depart_monitor_calls(), 1);
    assert_eq!(engine.list_displays(platform.as_ref()).total_active, 0);

    // WHY: Concurrent remove_display during Updating waits and surfaces depart_monitor errors to the remover.
    let (entered_tx2, entered_rx2) = mpsc::sync_channel(1);
    let (release_tx2, release_rx2) = mpsc::sync_channel(1);
    let platform2 = Arc::new(BlockingUpdatePlatform {
        inner: FakeIddPlatform::new(),
        updating: AtomicBool::new(false),
        entered_tx: entered_tx2,
        release_rx: Mutex::new(release_rx2),
    });
    let engine2 = Arc::new(VddEngine::new());
    let guid2 = [0x88; 16];
    let add_req2 = AddDisplayRequest::new(
        guid2,
        1920,
        1080,
        120,
        1,
        HdrProfile::Sdr,
        OwnershipMode::HandleBound,
    );
    engine2
        .add_display(file_key, &add_req2, platform2.as_ref())
        .unwrap();

    let eng_upd = Arc::clone(&engine2);
    let plat_upd = Arc::clone(&platform2);
    let upd_handle = thread::spawn(move || {
        let req = UpdateDisplayModeRequest::by_guid(guid2, 1920, 1080, 60, 1, HdrProfile::Sdr);
        eng_upd.update_display_mode(file_key, &req, plat_upd.as_ref())
    });

    entered_rx2.recv().unwrap();
    platform2.inner.set_fail_depart_monitor(true);

    let barrier = Arc::new(std::sync::Barrier::new(2));
    let bar_rem = Arc::clone(&barrier);
    let eng_rem = Arc::clone(&engine2);
    let plat_rem = Arc::clone(&platform2);
    let rem_handle = thread::spawn(move || {
        bar_rem.wait();
        eng_rem.remove_display(
            file_key,
            &RemoveDisplayRequest::by_guid(guid2),
            plat_rem.as_ref(),
        )
    });

    barrier.wait();
    thread::yield_now();
    release_tx2.send(()).unwrap();

    assert!(upd_handle.join().unwrap().is_ok());
    let rem_err = rem_handle.join().unwrap().unwrap_err();
    assert!(matches!(rem_err, EngineError::Platform(_)));
}

#[test]
fn color_space_bit_depth_ppi_and_hdr_nits_drive_edid_modes_and_rearrive() {
    let engine = VddEngine::new();
    let platform = FakeIddPlatform::new();
    let file_key = FileKey(42);
    let guid = [0x91; 16];

    let mut req = AddDisplayRequest::new(
        guid,
        3840,
        2160,
        120,
        1,
        HdrProfile::Hdr10,
        OwnershipMode::HandleBound,
    );
    req.target_ppi = 140;
    req.color_space = ColorSpacePreference::DciP3.as_u8();
    req.bit_depth = BitDepthPreference::Bpc10.as_u8();
    req.hdr_metadata = HdrMasteringMetadata {
        max_luminance_nits: 1500,
        max_fall_nits: 500,
        min_luminance_ten_thousandths: 250,
    };
    req.set_client_name("ReferenceP3").unwrap();

    engine.add_display(file_key, &req, &platform).unwrap();
    let parsed = engine
        .on_parse_monitor_description(MonitorQueryKey::MonitorGuid(guid))
        .unwrap();
    assert_eq!(parsed.edid[20] & 0x70, 0x30);
    let summary = validate_custom_edid(&parsed.edid).unwrap();
    assert_eq!(summary.chromaticity, Chromaticity10Bit::DCI_P3);
    let (w_mm, _) = summary.dtd_size_mm.unwrap();
    let ppi_x = (3840.0 * 25.4) / f64::from(w_mm);
    assert!((137.0..=143.0).contains(&ppi_x), "ppi_x={ppi_x}");
    let cta = summary.cta861.unwrap();
    let hdr_meta = cta.hdr_static_metadata.unwrap();
    assert!(hdr_meta.max_luminance_millinits.unwrap() > 1_400_000);
    assert!(parsed
        .spec
        .planned_modes
        .iter()
        .all(|m| m.bit_depths == BitDepthMask::BITS_10));

    let departs_before = platform.depart_monitor_calls();
    let mut upd = UpdateDisplayModeRequest::by_guid(guid, 3840, 2160, 120, 1, HdrProfile::Hdr10);
    upd.color_space = ColorSpacePreference::Bt2020.as_u8();
    upd.bit_depth = BitDepthPreference::Bpc12.as_u8();
    engine
        .update_display_mode(file_key, &upd, &platform)
        .unwrap();
    assert_eq!(platform.depart_monitor_calls(), departs_before + 1);
    let updated = engine
        .on_query_target_modes(MonitorQueryKey::MonitorGuid(guid))
        .unwrap();
    assert_eq!(updated.spec.edid[20] & 0x70, 0x40);
    assert!(updated
        .planned_modes
        .iter()
        .all(|m| m.bit_depths == BitDepthMask::BITS_12));
    assert_eq!(
        validate_custom_edid(&updated.spec.edid)
            .unwrap()
            .chromaticity,
        Chromaticity10Bit::BT2020
    );
}
