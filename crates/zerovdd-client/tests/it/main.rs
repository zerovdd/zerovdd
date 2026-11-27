use std::cell::RefCell;
use std::collections::VecDeque;
use std::mem::size_of;

use tempfile::TempDir;
use zerovdd_client::ccd::{
    apply_display_mode_in_place, apply_streaming_session_with_wait, is_forced_availability_target,
    is_live_ccd_display, resolve_virtual_display_with_wait, restore_streaming_session,
    DisplayTopologyBackend, RetryPolicy, SessionApplyOptions,
    DISPLAYCONFIG_TARGET_FORCED_AVAILABILITY_MASK,
};
use zerovdd_client::device::{
    map_win32_hresult, parse_multi_sz_utf16, select_control_device_path,
    validate_protocol_handshake,
};
use zerovdd_client::gpu::{
    interface_path_to_instance_id, parse_gpu_selector, select_gpu, select_gpu_from_config,
    GpuAdapterInfo, GpuSelector,
};
use zerovdd_client::hdr::{
    decode_advanced_color_info_2, decode_advanced_color_info_v1, plan_color_actions,
    AdvancedColorMode, ColorApiAction, DisplayColorState, DisplayConfigGetAdvancedColorInfo2,
    DisplayConfigSetHdrState, DisplayConfigSetWcgState, HdrTargetSetting,
    ADVANCED_COLOR_INFO_2_ACTIVE_BIT, ADVANCED_COLOR_INFO_2_HDR_SUPPORTED_BIT,
    ADVANCED_COLOR_INFO_2_HDR_USER_ENABLED_BIT, ADVANCED_COLOR_INFO_2_SUPPORTED_BIT,
    ADVANCED_COLOR_INFO_2_WCG_SUPPORTED_BIT, ADVANCED_COLOR_INFO_2_WCG_USER_ENABLED_BIT,
    ADVANCED_COLOR_INFO_V1_ENABLED_BIT, ADVANCED_COLOR_INFO_V1_SUPPORTED_BIT, WIN11_24H2_BUILD,
};
use zerovdd_client::topology::{
    load_snapshot, plan_enable_attached_physical, plan_in_place_mode_update, plan_restore_topology,
    plan_streaming_topology, remove_snapshot, save_snapshot_atomic, snapshot_from_state,
    AdapterLuid, CcdTopologyState, DisplayPathRecord, DisplayRotation, StreamingTopologyRequest,
    TopologySnapshot, TOPOLOGY_SNAPSHOT_SCHEMA_VERSION,
};
use zerovdd_client::{monitor_guid_for_name, ClientError};
use zerovdd_config::SunshineTopologyMode;
use zerovdd_proto::ioctl::PROTOCOL_MAJOR;
use zerovdd_proto::types::ProtocolVersion;

const FIXTURE_24H2_DUAL_HDR_ACM: &str = include_str!("../fixtures/win11_24h2_dual_hdr_acm.json");
const FIXTURE_23H2_SINGLE_HDR: &str = include_str!("../fixtures/win11_23h2_single_hdr.json");
const FIXTURE_GPU_MULTI: &str = include_str!("../fixtures/gpu_adapters_multi.json");

struct FakeTopologyBackend {
    query_queue: RefCell<VecDeque<CcdTopologyState>>,
    fallback_state: RefCell<CcdTopologyState>,
    applied_topologies: RefCell<Vec<Vec<DisplayPathRecord>>>,
    applied_hdr_actions: RefCell<Vec<(u32, Vec<ColorApiAction>)>>,
}

impl FakeTopologyBackend {
    fn new(states: Vec<CcdTopologyState>) -> Self {
        let last = states.last().expect("at least one state required").clone();
        Self {
            query_queue: RefCell::new(VecDeque::from(states)),
            fallback_state: RefCell::new(last),
            applied_topologies: RefCell::new(Vec::new()),
            applied_hdr_actions: RefCell::new(Vec::new()),
        }
    }
}

impl DisplayTopologyBackend for FakeTopologyBackend {
    fn query_topology(&self) -> Result<CcdTopologyState, ClientError> {
        let mut queue = self.query_queue.borrow_mut();
        if let Some(next) = queue.pop_front() {
            *self.fallback_state.borrow_mut() = next.clone();
            Ok(next)
        } else {
            Ok(self.fallback_state.borrow().clone())
        }
    }

    fn apply_topology(&self, desired_paths: &[DisplayPathRecord]) -> Result<(), ClientError> {
        self.applied_topologies
            .borrow_mut()
            .push(desired_paths.to_vec());
        let mut state = self.fallback_state.borrow_mut();
        for path in &mut state.paths {
            if let Some(updated) = desired_paths
                .iter()
                .find(|d| d.adapter_luid == path.adapter_luid && d.target_id == path.target_id)
            {
                *path = updated.clone();
            } else {
                path.active = false;
            }
        }
        Ok(())
    }

    fn apply_hdr_setting(&self, setting: &HdrTargetSetting) -> Result<(), ClientError> {
        let state = self.fallback_state.borrow();
        let current = state
            .paths
            .iter()
            .find(|p| p.adapter_luid == setting.adapter_luid && p.target_id == setting.target_id)
            .and_then(|p| p.hdr_state)
            .unwrap_or_default();
        let actions = plan_color_actions(setting.target_id, current, setting, state.os_build)?;
        self.applied_hdr_actions
            .borrow_mut()
            .push((setting.target_id, actions));
        Ok(())
    }
}

#[test]
fn isolate_and_extend_topology_planning_with_24h2_fixture() {
    let state: CcdTopologyState =
        serde_json::from_str(FIXTURE_24H2_DUAL_HDR_ACM).expect("valid 24H2 fixture JSON");
    let virtual_luid = AdapterLuid::new(98305, 0);
    let virtual_target = 1;

    let isolate_req = StreamingTopologyRequest {
        mode: SunshineTopologyMode::Isolate,
        width: 3840,
        height: 2160,
        refresh_num: 120_000,
        refresh_den: 1000,
        hdr_enabled: true,
        extend_origin: None,
        make_virtual_primary: true,
        ..Default::default()
    };
    let isolate_plan = plan_streaming_topology(&state, virtual_luid, virtual_target, &isolate_req)
        .expect("isolate plan succeeds");

    let active_in_isolate: Vec<&DisplayPathRecord> = isolate_plan
        .desired_paths
        .iter()
        .filter(|p| p.active)
        .collect();
    assert_eq!(active_in_isolate.len(), 1);
    assert_eq!(active_in_isolate[0].target_id, virtual_target);
    assert_eq!(active_in_isolate[0].width, 3840);
    assert_eq!(active_in_isolate[0].height, 2160);
    assert_eq!(active_in_isolate[0].origin_x, 0);
    assert_eq!(active_in_isolate[0].origin_y, 0);
    assert_eq!(isolate_plan.hdr_updates.len(), 1);
    assert!(isolate_plan.hdr_updates[0].hdr_enabled);

    let extend_secondary_req = StreamingTopologyRequest {
        mode: SunshineTopologyMode::Extend,
        width: 1920,
        height: 1080,
        refresh_num: 60_000,
        refresh_den: 1000,
        hdr_enabled: false,
        extend_origin: None,
        make_virtual_primary: false,
        ..Default::default()
    };
    let extend_secondary =
        plan_streaming_topology(&state, virtual_luid, virtual_target, &extend_secondary_req)
            .expect("extend secondary succeeds");
    assert_eq!(extend_secondary.desired_paths.len(), 3);
    let virt_sec = extend_secondary
        .desired_paths
        .iter()
        .find(|p| p.target_id == virtual_target)
        .expect("virtual path present");
    assert_eq!((virt_sec.origin_x, virt_sec.origin_y), (6400, 0));

    let extend_primary_req = StreamingTopologyRequest {
        make_virtual_primary: true,
        ..extend_secondary_req
    };
    let extend_primary =
        plan_streaming_topology(&state, virtual_luid, virtual_target, &extend_primary_req)
            .expect("extend primary succeeds");
    let virt_prim = extend_primary
        .desired_paths
        .iter()
        .find(|p| p.target_id == virtual_target)
        .expect("virtual path present");
    let phys_1 = extend_primary
        .desired_paths
        .iter()
        .find(|p| p.target_id == 256)
        .expect("physical 1 present");
    let phys_2 = extend_primary
        .desired_paths
        .iter()
        .find(|p| p.target_id == 257)
        .expect("physical 2 present");
    assert_eq!((virt_prim.origin_x, virt_prim.origin_y), (0, 0));
    assert_eq!((phys_1.origin_x, phys_1.origin_y), (1920, 0));
    assert_eq!((phys_2.origin_x, phys_2.origin_y), (4480, 0));
}

#[test]
fn restore_skips_unplugged_primary_and_reassigns_origin_zero_zero() {
    let initial: CcdTopologyState =
        serde_json::from_str(FIXTURE_24H2_DUAL_HDR_ACM).expect("valid 24H2 fixture JSON");
    let virtual_luid = AdapterLuid::new(98305, 0);
    let virtual_target = 1;
    let snapshot = snapshot_from_state(
        &initial,
        1_700_000_000_000,
        Some((virtual_luid, virtual_target)),
    );
    assert_eq!(snapshot.paths.len(), 2);
    assert_eq!(snapshot.hdr_states.len(), 2);

    let mut current_after_unplug = initial.clone();
    current_after_unplug.paths.retain(|p| p.target_id != 256);

    let restore_plan = plan_restore_topology(
        &snapshot,
        &current_after_unplug,
        Some((virtual_luid, virtual_target)),
    )
    .expect("restore succeeds with surviving secondary monitor");

    assert_eq!(
        restore_plan.skipped_unplugged_targets,
        vec![(AdapterLuid::new(65537, 0), 256)]
    );
    assert_eq!(restore_plan.desired_paths.len(), 1);
    let surviving = &restore_plan.desired_paths[0];
    assert_eq!(surviving.target_id, 257);
    assert_eq!(surviving.origin_x, 0);
    assert_eq!(surviving.origin_y, 0);

    assert_eq!(restore_plan.hdr_updates.len(), 1);
    assert!(!restore_plan.hdr_updates[0].hdr_enabled);
    assert_eq!(restore_plan.hdr_updates[0].wcg_acm_enabled, Some(true));

    let mut no_physical = initial;
    no_physical.paths.retain(|p| p.target_id == virtual_target);
    let err = plan_restore_topology(
        &snapshot,
        &no_physical,
        Some((virtual_luid, virtual_target)),
    )
    .expect_err("expected NoActivePhysicalDisplays when zero physical monitors remain");
    assert!(matches!(err, ClientError::NoActivePhysicalDisplays));
}

#[test]
fn win11_24h2_hdr_vs_acm_decoding_and_23h2_fallback() {
    assert_eq!(size_of::<DisplayConfigGetAdvancedColorInfo2>(), 36);
    assert_eq!(size_of::<DisplayConfigSetHdrState>(), 24);
    assert_eq!(size_of::<DisplayConfigSetWcgState>(), 24);

    let acm_only_bits = ADVANCED_COLOR_INFO_2_SUPPORTED_BIT
        | ADVANCED_COLOR_INFO_2_ACTIVE_BIT
        | ADVANCED_COLOR_INFO_2_WCG_SUPPORTED_BIT
        | ADVANCED_COLOR_INFO_2_WCG_USER_ENABLED_BIT;
    let acm_state = decode_advanced_color_info_2(acm_only_bits, 0, 10, 1);
    assert!(!acm_state.hdr_supported);
    assert!(!acm_state.hdr_enabled);
    assert!(acm_state.wcg_supported);
    assert!(acm_state.wcg_acm_enabled);
    assert_eq!(acm_state.active_color_mode, AdvancedColorMode::Wcg);

    let luid = AdapterLuid::new(65537, 0);
    let err = plan_color_actions(
        257,
        acm_state,
        &HdrTargetSetting {
            adapter_luid: luid,
            target_id: 257,
            hdr_enabled: true,
            wcg_acm_enabled: None,
        },
        WIN11_24H2_BUILD,
    )
    .expect_err("enabling HDR on ACM-only monitor must return HdrUnsupported");
    assert!(matches!(
        err,
        ClientError::HdrUnsupported { target_id: 257, .. }
    ));

    let mut acm_disabled = acm_state;
    acm_disabled.wcg_acm_enabled = false;
    acm_disabled.active_color_mode = AdvancedColorMode::Sdr;
    let restore_acm = plan_color_actions(
        257,
        acm_disabled,
        &HdrTargetSetting {
            adapter_luid: luid,
            target_id: 257,
            hdr_enabled: false,
            wcg_acm_enabled: Some(true),
        },
        WIN11_24H2_BUILD,
    )
    .expect("restoring ACM on 24H2 succeeds");
    assert_eq!(
        restore_acm,
        vec![ColorApiAction::SetWcgState { enable: true }]
    );

    let hdr_plus_acm_bits = ADVANCED_COLOR_INFO_2_SUPPORTED_BIT
        | ADVANCED_COLOR_INFO_2_ACTIVE_BIT
        | ADVANCED_COLOR_INFO_2_HDR_SUPPORTED_BIT
        | ADVANCED_COLOR_INFO_2_WCG_SUPPORTED_BIT
        | ADVANCED_COLOR_INFO_2_WCG_USER_ENABLED_BIT;
    let hdr_capable = decode_advanced_color_info_2(hdr_plus_acm_bits, 0, 10, 1);
    assert!(hdr_capable.hdr_supported);
    assert!(!hdr_capable.hdr_enabled);
    assert!(hdr_capable.wcg_acm_enabled);
    let hdr_active_bits = hdr_plus_acm_bits | ADVANCED_COLOR_INFO_2_HDR_USER_ENABLED_BIT;
    let hdr_active = decode_advanced_color_info_2(hdr_active_bits, 0, 10, 2);
    assert!(hdr_active.hdr_supported);
    assert!(hdr_active.hdr_enabled);
    assert!(hdr_active.wcg_acm_enabled);
    assert_eq!(hdr_active.active_color_mode, AdvancedColorMode::Hdr);

    let toggle_24h2 = plan_color_actions(
        256,
        hdr_capable,
        &HdrTargetSetting {
            adapter_luid: luid,
            target_id: 256,
            hdr_enabled: true,
            wcg_acm_enabled: None,
        },
        WIN11_24H2_BUILD,
    )
    .expect("24H2 toggle HDR succeeds");
    assert_eq!(
        toggle_24h2,
        vec![ColorApiAction::SetHdrState { enable: true }]
    );

    let state_23h2: CcdTopologyState =
        serde_json::from_str(FIXTURE_23H2_SINGLE_HDR).expect("valid 23H2 fixture JSON");
    assert_eq!(state_23h2.os_build, 22631);
    let decoded_v1 = decode_advanced_color_info_v1(
        ADVANCED_COLOR_INFO_V1_SUPPORTED_BIT | ADVANCED_COLOR_INFO_V1_ENABLED_BIT,
        0,
        10,
    );
    assert!(decoded_v1.hdr_supported);
    assert!(decoded_v1.hdr_enabled);
    assert!(!decoded_v1.wcg_acm_enabled);

    let toggle_23h2 = plan_color_actions(
        100,
        DisplayColorState {
            hdr_supported: true,
            hdr_enabled: false,
            ..Default::default()
        },
        &HdrTargetSetting {
            adapter_luid: AdapterLuid::new(71001, 0),
            target_id: 100,
            hdr_enabled: true,
            wcg_acm_enabled: Some(true),
        },
        state_23h2.os_build,
    )
    .expect("23H2 fallback succeeds");
    assert_eq!(
        toggle_23h2,
        vec![ColorApiAction::SetAdvancedColorState { enable: true }]
    );
}

#[test]
fn snapshot_atomic_json_roundtrip_and_session_orchestration() {
    let initial: CcdTopologyState =
        serde_json::from_str(FIXTURE_24H2_DUAL_HDR_ACM).expect("valid 24H2 fixture JSON");
    let virtual_luid = AdapterLuid::new(98305, 0);
    let virtual_target = 1;

    let dir = TempDir::new().expect("create tempdir");
    let backup_path = dir.path().join("nested").join("topology-backup.json");

    let mut before_arrival = initial.clone();
    before_arrival
        .paths
        .retain(|p| p.target_id != virtual_target);
    let backend = FakeTopologyBackend::new(vec![
        before_arrival.clone(),
        before_arrival,
        initial.clone(),
    ]);

    let mut recorded_waits = Vec::new();
    let req = StreamingTopologyRequest {
        mode: SunshineTopologyMode::Isolate,
        width: 2560,
        height: 1440,
        refresh_num: 120_000,
        refresh_den: 1000,
        hdr_enabled: true,
        extend_origin: None,
        make_virtual_primary: true,
        ..Default::default()
    };
    let applied = apply_streaming_session_with_wait(
        &backend,
        virtual_luid,
        virtual_target,
        &req,
        &SessionApplyOptions {
            snapshot_path: Some(&backup_path),
            created_at_unix_ms: 1_700_000_000_123,
            retry_policy: RetryPolicy {
                max_attempts: 5,
                delay_ms: 25,
            },
        },
        |attempt, delay| recorded_waits.push((attempt, delay)),
    )
    .expect("streaming session applies after retry");

    assert_eq!(recorded_waits, vec![(1, 25), (2, 25)]);
    assert!(applied.snapshot_saved);
    assert_eq!(applied.active_path_count, 1);

    let loaded = load_snapshot(&backup_path)
        .expect("load snapshot")
        .expect("snapshot exists");
    assert_eq!(loaded.schema_version, TOPOLOGY_SNAPSHOT_SCHEMA_VERSION);
    assert_eq!(loaded.paths.len(), 2);

    let second_apply = apply_streaming_session_with_wait(
        &backend,
        virtual_luid,
        virtual_target,
        &req,
        &SessionApplyOptions {
            snapshot_path: Some(&backup_path),
            created_at_unix_ms: 1_700_000_009_999,
            retry_policy: RetryPolicy {
                max_attempts: 2,
                delay_ms: 10,
            },
        },
        |_, _| {},
    )
    .expect("second launch succeeds");
    assert!(!second_apply.snapshot_saved);

    let restored =
        restore_streaming_session(&backend, &backup_path, Some((virtual_luid, virtual_target)))
            .expect("restore succeeds");
    assert!(restored.snapshot_found);
    assert_eq!(restored.restored_path_count, 2);
    assert!(restored.skipped_unplugged_targets.is_empty());
    assert!(load_snapshot(&backup_path).expect("check absent").is_none());

    let idempotent_restore =
        restore_streaming_session(&backend, &backup_path, Some((virtual_luid, virtual_target)))
            .expect("idempotent restore succeeds");
    assert!(!idempotent_restore.snapshot_found);

    let bad_snapshot = TopologySnapshot {
        schema_version: 99,
        created_at_unix_ms: 0,
        paths: Vec::new(),
        hdr_states: Vec::new(),
    };
    assert!(matches!(
        save_snapshot_atomic(&backup_path, &bad_snapshot),
        Err(ClientError::SnapshotCorrupt(_))
    ));
    assert!(!remove_snapshot(&backup_path).expect("remove missing returns false"));
}

#[test]
fn retry_exhaustion_returns_virtual_display_not_found() {
    let mut initial: CcdTopologyState =
        serde_json::from_str(FIXTURE_24H2_DUAL_HDR_ACM).expect("valid 24H2 fixture JSON");
    initial.paths.retain(|p| p.target_id != 1);
    let backend = FakeTopologyBackend::new(vec![initial]);

    let mut waits = 0u32;
    let err = resolve_virtual_display_with_wait(
        &backend,
        AdapterLuid::new(98305, 0),
        1,
        RetryPolicy {
            max_attempts: 3,
            delay_ms: 50,
        },
        |_, _| waits += 1,
    )
    .expect_err("expected timeout error");
    assert_eq!(waits, 2);
    assert!(matches!(
        err,
        ClientError::VirtualDisplayNotFound { target_id: 1, .. }
    ));
}

#[test]
fn gpu_selector_matching_filters_warp_and_matches_all_selectors() {
    let adapters: Vec<GpuAdapterInfo> =
        serde_json::from_str(FIXTURE_GPU_MULTI).expect("valid GPU fixture JSON");

    let auto = select_gpu(&adapters, &GpuSelector::AutoBest).expect("AutoBest selects hardware");
    assert_eq!(auto.name, "NVIDIA GeForce RTX 4090");
    assert!(!auto.is_software);

    let by_luid = select_gpu(
        &adapters,
        &GpuSelector::Luid {
            low: 65539,
            high: 0,
        },
    )
    .expect("Luid selects AMD");
    assert_eq!(by_luid.name, "AMD Radeon RX 7900 XTX");

    let by_pci_name = select_gpu(
        &adapters,
        &GpuSelector::PciBusAndName {
            bus: 6,
            device: Some(0),
            function: Some(0),
            name: Some("radeon".to_string()),
        },
    )
    .expect("PciBusAndName selects AMD");
    assert_eq!(by_pci_name.adapter_luid_low, 65539);

    let by_cfg_pci = select_gpu_from_config(
        &adapters,
        &zerovdd_config::GpuSelector::PciBus {
            bus: 0,
            device: 2,
            function: 0,
        },
    )
    .expect("config PciBus selects Intel");
    assert_eq!(by_cfg_pci.name, "Intel(R) UHD Graphics 770");

    let by_name = select_gpu(
        &adapters,
        &GpuSelector::FriendlyName {
            name: "rtx 4090".to_string(),
        },
    )
    .expect("FriendlyName substring selects RTX 4090");
    assert_eq!(by_name.adapter_luid_low, 65538);

    let explicit_warp = select_gpu(
        &adapters,
        &GpuSelector::FriendlyName {
            name: "Microsoft Basic Render Driver".to_string(),
        },
    )
    .expect("explicit FriendlyName can select WARP");
    assert!(explicit_warp.is_software);

    let only_warp: Vec<GpuAdapterInfo> = adapters.into_iter().filter(|a| a.is_software).collect();
    let auto_warp_err =
        select_gpu(&only_warp, &GpuSelector::AutoBest).expect_err("AutoBest must never pick WARP");
    assert!(matches!(auto_warp_err, ClientError::GpuNotFound { .. }));
}

#[test]
fn control_device_path_selection_multi_sz_and_handshake_helpers() {
    let raw = "\\\\?\\ROOT#DISPLAY#0000#{2963ae53-4cf3-4385-929a-a8348595217e}\0\
               \\\\?\\ROOT#DISPLAY#0000#{2963ae53-4cf3-4385-929a-a8348595217e}\\zerovddcontrol\0\0";
    let utf16: Vec<u16> = raw.encode_utf16().collect();
    let parsed = parse_multi_sz_utf16(&utf16);
    assert_eq!(parsed.len(), 2);

    let selected = select_control_device_path(&parsed).expect("selects \\ZeroVddControl suffix");
    assert!(selected.ends_with(r"\zerovddcontrol"));

    let no_ref = vec![r"\\?\ROOT#DISPLAY#0000#{2963ae53-4cf3-4385-929a-a8348595217e}".to_string()];
    assert!(select_control_device_path(&no_ref).is_none());

    assert!(validate_protocol_handshake(ProtocolVersion::CURRENT).is_ok());
    let bad_ver = ProtocolVersion::new(PROTOCOL_MAJOR + 1, 0, 0, 0);
    assert!(matches!(
        validate_protocol_handshake(bad_ver),
        Err(ClientError::ProtocolMismatch { .. })
    ));

    let instance_id = interface_path_to_instance_id(
        r"\\?\PCI#VEN_10DE&DEV_2684&SUBSYS_12345678&REV_A1#4&1a2b3c&0&0008#{5b45201d-f2f2-4f3b-85bb-30ff1f953599}",
    )
    .expect("instance ID parsed");
    assert_eq!(
        instance_id,
        r"PCI\VEN_10DE&DEV_2684&SUBSYS_12345678&REV_A1\4&1a2b3c&0&0008"
    );

    assert_eq!(
        monitor_guid_for_name("living-room-tv"),
        monitor_guid_for_name("living-room-tv")
    );
    assert_ne!(
        monitor_guid_for_name("living-room-tv"),
        monitor_guid_for_name("steam-deck")
    );
}

#[test]
fn map_win32_hresult_distinguishes_monitor_not_found_from_device_missing() {
    assert!(matches!(
        map_win32_hresult(
            "DeviceIoControl(IOCTL_REMOVE_DISPLAY)",
            0x8007_0490,
            r"\\.\dev",
            true
        ),
        ClientError::MonitorNotFound
    ));
    assert!(matches!(
        map_win32_hresult("CreateFileW", 0x8007_0490, r"\\.\dev", false),
        ClientError::Win32 {
            code: 0x8007_0490,
            ..
        }
    ));
    assert!(matches!(
        map_win32_hresult("CreateFileW", 0x8007_0005, r"\\.\dev", false),
        ClientError::AccessDenied
    ));
    assert!(matches!(
        map_win32_hresult("CreateFileW", 0x8007_0002, r"\\.\dev", false),
        ClientError::DeviceNotFound(ref p) if p == r"\\.\dev"
    ));
    assert!(matches!(
        map_win32_hresult("CreateFileW", 0x8007_0003, r"\\.\dev", false),
        ClientError::DeviceNotFound(ref p) if p == r"\\.\dev"
    ));
    assert!(matches!(
        map_win32_hresult(
            "DeviceIoControl(IOCTL_ADD_DISPLAY)",
            0x8007_00B7,
            r"\\.\dev",
            true
        ),
        ClientError::Win32 {
            code: 0x8007_00B7,
            ..
        }
    ));
}

fn make_test_path(
    luid: AdapterLuid,
    target_id: u32,
    source_id: u32,
    active: bool,
    origin_x: i32,
    width: u32,
    height: u32,
) -> DisplayPathRecord {
    DisplayPathRecord {
        adapter_luid: luid,
        target_id,
        source_id,
        gdi_name: format!(r"\\.\DISPLAY{}", target_id + 1),
        monitor_friendly_name: format!("Monitor {target_id}"),
        active,
        target_available: true,
        width,
        height,
        origin_x,
        origin_y: 0,
        refresh_num: 60_000,
        refresh_den: 1000,
        scaling: Default::default(),
        rotation: Default::default(),
        scanline_ordering: Default::default(),
        hdr_state: None,
    }
}

#[test]
fn extend_reassigns_colliding_source_id_on_same_adapter() {
    let shared_luid = AdapterLuid::new(98305, 0);
    let state = CcdTopologyState {
        os_build: WIN11_24H2_BUILD,
        paths: vec![
            make_test_path(shared_luid, 0, 0, true, 0, 1920, 1080),
            make_test_path(shared_luid, 1, 0, false, 0, 1920, 1080),
        ],
    };

    let req = StreamingTopologyRequest {
        mode: SunshineTopologyMode::Extend,
        width: 2560,
        height: 1440,
        refresh_num: 120_000,
        refresh_den: 1000,
        hdr_enabled: false,
        extend_origin: None,
        make_virtual_primary: false,
        ..Default::default()
    };

    let plan = plan_streaming_topology(&state, shared_luid, 1, &req).expect("extend succeeds");
    assert_eq!(plan.desired_paths.len(), 2);
    let existing = plan
        .desired_paths
        .iter()
        .find(|p| p.target_id == 0)
        .expect("existing display kept");
    let virtual_disp = plan
        .desired_paths
        .iter()
        .find(|p| p.target_id == 1)
        .expect("virtual display planned");
    assert_eq!(existing.source_id, 0);
    assert_ne!(virtual_disp.source_id, existing.source_id);
    assert_eq!(virtual_disp.source_id, 1);
}

#[test]
fn extend_make_virtual_primary_preserves_left_monitors_without_overlap() {
    let phys_luid = AdapterLuid::new(65537, 0);
    let virt_luid = AdapterLuid::new(98305, 0);

    let state = CcdTopologyState {
        os_build: WIN11_24H2_BUILD,
        paths: vec![
            make_test_path(phys_luid, 10, 0, true, -1920, 1920, 1080),
            make_test_path(phys_luid, 11, 1, true, 0, 1920, 1080),
            make_test_path(virt_luid, 1, 0, false, 0, 1920, 1080),
        ],
    };

    let req = StreamingTopologyRequest {
        mode: SunshineTopologyMode::Extend,
        width: 2560,
        height: 1440,
        refresh_num: 60_000,
        refresh_den: 1000,
        hdr_enabled: false,
        extend_origin: None,
        make_virtual_primary: true,
        ..Default::default()
    };
    let plan =
        plan_streaming_topology(&state, virt_luid, 1, &req).expect("extend primary succeeds");
    let mut intervals: Vec<(i32, i32)> = plan
        .desired_paths
        .iter()
        .map(|p| (p.origin_x, p.origin_x + p.width as i32))
        .collect();
    intervals.sort_by_key(|&(start, _)| start);
    assert_eq!(intervals, vec![(-1920, 0), (0, 2560), (2560, 4480)]);

    let straddle_state = CcdTopologyState {
        os_build: WIN11_24H2_BUILD,
        paths: vec![
            make_test_path(phys_luid, 10, 0, true, -2420, 1920, 1080),
            make_test_path(phys_luid, 11, 1, true, -500, 1920, 1080),
            make_test_path(virt_luid, 1, 0, false, 0, 1920, 1080),
        ],
    };
    let straddle_req = StreamingTopologyRequest { width: 1920, ..req };
    let straddle_plan = plan_streaming_topology(&straddle_state, virt_luid, 1, &straddle_req)
        .expect("extend straddle succeeds");
    let mut straddle_intervals: Vec<(i32, i32)> = straddle_plan
        .desired_paths
        .iter()
        .map(|p| (p.origin_x, p.origin_x + p.width as i32))
        .collect();
    straddle_intervals.sort_by_key(|&(start, _)| start);
    assert_eq!(
        straddle_intervals,
        vec![(-2420, -500), (0, 1920), (1920, 3840)]
    );
}

#[test]
fn inactive_virtual_display_with_none_hdr_state_enforces_sdr_off() {
    let virt_luid = AdapterLuid::new(98305, 0);
    let state = CcdTopologyState {
        os_build: WIN11_24H2_BUILD,
        paths: vec![make_test_path(virt_luid, 1, 0, false, 0, 1920, 1080)],
    };

    let req = StreamingTopologyRequest {
        mode: SunshineTopologyMode::Extend,
        width: 1920,
        height: 1080,
        refresh_num: 60_000,
        refresh_den: 1000,
        hdr_enabled: false,
        extend_origin: Some((1920, 0)),
        make_virtual_primary: false,
        ..Default::default()
    };

    let plan = plan_streaming_topology(&state, virt_luid, 1, &req).expect("plan succeeds");
    assert_eq!(plan.desired_paths.len(), 1);
    assert_eq!(
        (
            plan.desired_paths[0].origin_x,
            plan.desired_paths[0].origin_y
        ),
        (0, 0)
    );
    assert_eq!(plan.hdr_updates.len(), 1);
    assert!(!plan.hdr_updates[0].hdr_enabled);
    assert_eq!(plan.hdr_updates[0].target_id, 1);
}

#[test]
fn headless_session_skips_empty_snapshot_and_cleans_stale_empty_backup() {
    let phys_luid = AdapterLuid::new(65537, 0);
    let virt_luid = AdapterLuid::new(98305, 0);
    let headless_state = CcdTopologyState {
        os_build: WIN11_24H2_BUILD,
        paths: vec![
            make_test_path(phys_luid, 10, 0, false, 0, 1920, 1080),
            make_test_path(virt_luid, 1, 0, false, 0, 1920, 1080),
        ],
    };

    let dir = TempDir::new().expect("create tempdir");
    let backup_path = dir.path().join("topology-backup.json");
    let backend = FakeTopologyBackend::new(vec![headless_state]);

    let req = StreamingTopologyRequest {
        mode: SunshineTopologyMode::Isolate,
        width: 1920,
        height: 1080,
        refresh_num: 60_000,
        refresh_den: 1000,
        hdr_enabled: false,
        extend_origin: None,
        make_virtual_primary: true,
        ..Default::default()
    };

    let applied = apply_streaming_session_with_wait(
        &backend,
        virt_luid,
        1,
        &req,
        &SessionApplyOptions {
            snapshot_path: Some(&backup_path),
            created_at_unix_ms: 1_700_000_000_000,
            retry_policy: RetryPolicy {
                max_attempts: 2,
                delay_ms: 10,
            },
        },
        |_, _| {},
    )
    .expect("headless apply succeeds");

    assert!(!applied.snapshot_saved);
    assert!(!backup_path.exists());

    let stale_empty = TopologySnapshot {
        schema_version: TOPOLOGY_SNAPSHOT_SCHEMA_VERSION,
        created_at_unix_ms: 1_700_000_000_000,
        paths: Vec::new(),
        hdr_states: Vec::new(),
    };
    save_snapshot_atomic(&backup_path, &stale_empty).expect("write stale empty snapshot");
    let report = restore_streaming_session(&backend, &backup_path, Some((virt_luid, 1)))
        .expect("restore cleans stale empty snapshot");
    assert!(report.snapshot_found);
    assert_eq!(report.restored_path_count, 0);
    assert!(!backup_path.exists());

    let powered_off_backup = TopologySnapshot {
        schema_version: TOPOLOGY_SNAPSHOT_SCHEMA_VERSION,
        created_at_unix_ms: 1_700_000_000_001,
        paths: vec![make_test_path(phys_luid, 10, 0, true, 0, 2560, 1440)],
        hdr_states: Vec::new(),
    };
    save_snapshot_atomic(&backup_path, &powered_off_backup).expect("write powered-off snapshot");
    let mut unplugged_state = CcdTopologyState {
        os_build: WIN11_24H2_BUILD,
        paths: vec![
            make_test_path(phys_luid, 10, 0, false, 0, 2560, 1440),
            make_test_path(virt_luid, 1, 0, true, 0, 1920, 1080),
        ],
    };
    unplugged_state.paths[0].target_available = false;
    let unplugged_backend = FakeTopologyBackend::new(vec![unplugged_state]);
    let unplugged_report =
        restore_streaming_session(&unplugged_backend, &backup_path, Some((virt_luid, 1)))
            .expect("restore cleanly skips all-unplugged monitors and removes stale snapshot");
    assert!(unplugged_report.snapshot_found);
    assert_eq!(unplugged_report.restored_path_count, 0);
    assert_eq!(
        unplugged_report.skipped_unplugged_targets,
        vec![(phys_luid, 10)]
    );
    assert!(!backup_path.exists());

    assert!(!is_forced_availability_target(0));
    assert!(!is_forced_availability_target(0x1 | 0x2));
    assert!(is_forced_availability_target(0x4));
    assert!(is_forced_availability_target(0x8));
    assert!(is_forced_availability_target(0x10));
    assert!(is_forced_availability_target(
        DISPLAYCONFIG_TARGET_FORCED_AVAILABILITY_MASK
    ));
    assert!(is_live_ccd_display(
        true,
        true,
        0x1,
        r"\\?\DISPLAY#ZVD5644#1&1&UID256#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}",
        true
    ));
    assert!(!is_live_ccd_display(
        true,
        true,
        0x10,
        r"\\?\DISPLAY#ZVD5644#1&1&UID256#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}",
        true
    ));
    assert!(!is_live_ccd_display(true, true, 0x1, "", true));
    assert!(!is_live_ccd_display(
        true,
        true,
        0x1,
        r"\\?\DISPLAY#ZVD5644#1&1&UID256#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}",
        false
    ));
}

#[cfg(windows)]
#[test]
fn windows_live_queries_are_safe_without_driver_or_elevation() {
    use zerovdd_client::ccd::WindowsCcdBackend;
    use zerovdd_client::device::ControlDevice;
    use zerovdd_client::gpu::enumerate_dxgi_adapters;
    use zerovdd_client::hdr::query_os_version;

    let os = query_os_version().expect("RtlGetVersion succeeds");
    assert!(os.major >= 10);
    assert!(os.build > 0);

    let gpus = enumerate_dxgi_adapters().expect("DXGI adapter enumeration succeeds");
    assert!(!gpus.is_empty());

    match WindowsCcdBackend::new().query_topology() {
        Ok(ccd) => assert_eq!(ccd.os_build, os.build),
        Err(ClientError::Ccd { status: 5, .. }) => {}
        Err(other) => panic!("unexpected QueryDisplayConfig error: {other}"),
    }

    match ControlDevice::open() {
        Ok(dev) => {
            let ver = dev.driver_version();
            assert_eq!(ver.major, PROTOCOL_MAJOR);
        }
        Err(ClientError::AccessDenied | ClientError::DeviceNotFound(_)) => {}
        Err(other) => panic!("unexpected ControlDevice::open error: {other}"),
    }
}

#[test]
fn plan_and_apply_in_place_mode_update_preserves_other_monitors() {
    let mut state: CcdTopologyState = serde_json::from_str(FIXTURE_24H2_DUAL_HDR_ACM).unwrap();
    let virt_luid = AdapterLuid::new(0x9999, 0);
    let virt_tid = 256u32;
    let first_origin = (state.paths[0].origin_x, state.paths[0].origin_y);
    let first_dims = (state.paths[0].width, state.paths[0].height);

    let right_edge = state
        .paths
        .iter()
        .filter(|p| p.active)
        .map(|p| p.origin_x + p.width as i32)
        .max()
        .unwrap_or(0);
    let mut virt = state.paths[0].clone();
    virt.adapter_luid = virt_luid;
    virt.target_id = virt_tid;
    virt.source_id = 5;
    virt.gdi_name = r"\\.\DISPLAY9".to_string();
    virt.monitor_friendly_name = "zerovdd".to_string();
    virt.active = true;
    virt.target_available = true;
    virt.origin_x = right_edge;
    virt.origin_y = 0;
    virt.width = 3024;
    virt.height = 1890;
    virt.refresh_num = 120;
    virt.refresh_den = 1;
    state.paths.push(virt);

    let planned = plan_in_place_mode_update(
        &state,
        virt_luid,
        virt_tid,
        2560,
        1440,
        60_000,
        1001,
        Some(true),
    )
    .expect("in-place plan succeeds");
    assert_eq!(planned.desired_paths.iter().filter(|p| p.active).count(), 3);
    assert_eq!(planned.hdr_updates.len(), 1);
    assert!(planned.hdr_updates[0].hdr_enabled);

    let backend = FakeTopologyBackend::new(vec![state]);
    let updated = apply_display_mode_in_place(
        &backend,
        virt_luid,
        virt_tid,
        2560,
        1440,
        60_000,
        1001,
        Some(true),
    )
    .expect("in-place apply succeeds");

    assert_eq!((updated.width, updated.height), (2560, 1440));
    assert_eq!((updated.refresh_num, updated.refresh_den), (60_000, 1001));
    assert_eq!(updated.origin_x, right_edge);

    let final_state = backend.query_topology().unwrap();
    assert_eq!(
        (final_state.paths[0].origin_x, final_state.paths[0].origin_y),
        first_origin
    );
    assert_eq!(
        (final_state.paths[0].width, final_state.paths[0].height),
        first_dims
    );
    assert_eq!(backend.applied_hdr_actions.borrow().len(), 1);
}

#[test]
fn portrait_rotation_swaps_source_dims_normalizes_primary_and_enables_physical() {
    let phys_luid = AdapterLuid::new(0x100, 0);
    let virt_luid = AdapterLuid::new(0x200, 0);
    let state = CcdTopologyState {
        os_build: WIN11_24H2_BUILD,
        paths: vec![
            make_test_path(phys_luid, 10, 0, true, 0, 2560, 1440),
            make_test_path(virt_luid, 1, 0, false, 0, 1920, 1080),
        ],
    };

    let portrait_left = StreamingTopologyRequest {
        mode: SunshineTopologyMode::Extend,
        width: 1920,
        height: 1080,
        refresh_num: 60,
        refresh_den: 1,
        hdr_enabled: false,
        extend_origin: Some((-1080, 0)),
        make_virtual_primary: false,
        rotation: DisplayRotation::Rotate90,
    };
    let plan = plan_streaming_topology(&state, virt_luid, 1, &portrait_left).unwrap();
    let virt = plan
        .desired_paths
        .iter()
        .find(|p| p.target_id == 1)
        .unwrap();
    assert_eq!((virt.width, virt.height), (1080, 1920));
    assert_eq!((virt.origin_x, virt.origin_y), (-1080, 0));
    assert_eq!(virt.rotation, DisplayRotation::Rotate90);

    let portrait_primary = StreamingTopologyRequest {
        make_virtual_primary: true,
        extend_origin: None,
        rotation: DisplayRotation::Rotate270,
        ..portrait_left.clone()
    };
    let prim_plan = plan_streaming_topology(&state, virt_luid, 1, &portrait_primary).unwrap();
    let prim_virt = prim_plan
        .desired_paths
        .iter()
        .find(|p| p.target_id == 1)
        .unwrap();
    let shifted_phys = prim_plan
        .desired_paths
        .iter()
        .find(|p| p.target_id == 10)
        .unwrap();
    assert_eq!((prim_virt.width, prim_virt.height), (1080, 1920));
    assert_eq!((prim_virt.origin_x, prim_virt.origin_y), (0, 0));
    assert_eq!((shifted_phys.origin_x, shifted_phys.origin_y), (1080, 0));

    let portrait_state = CcdTopologyState {
        os_build: WIN11_24H2_BUILD,
        paths: prim_plan.desired_paths,
    };
    let upd =
        plan_in_place_mode_update(&portrait_state, virt_luid, 1, 2560, 1440, 120, 1, None).unwrap();
    let upd_virt = upd.desired_paths.iter().find(|p| p.target_id == 1).unwrap();
    assert_eq!(upd_virt.rotation, DisplayRotation::Rotate270);
    assert_eq!((upd_virt.width, upd_virt.height), (1440, 2560));

    let isolated_phys_off = CcdTopologyState {
        os_build: WIN11_24H2_BUILD,
        paths: vec![
            make_test_path(phys_luid, 10, 0, false, 0, 2560, 1440),
            make_test_path(virt_luid, 1, 0, true, 0, 1920, 1080),
        ],
    };
    let fallback = plan_enable_attached_physical(&isolated_phys_off, Some((virt_luid, 1))).unwrap();
    assert_eq!(fallback.desired_paths.len(), 1);
    assert!(fallback.desired_paths[0].active);
    assert_eq!(fallback.desired_paths[0].target_id, 10);
    assert_eq!(
        (
            fallback.desired_paths[0].origin_x,
            fallback.desired_paths[0].origin_y
        ),
        (0, 0)
    );

    assert_eq!(parse_gpu_selector("auto").unwrap(), GpuSelector::AutoBest);
    assert_eq!(
        parse_gpu_selector("luid:0x100:-1").unwrap(),
        GpuSelector::Luid {
            low: 0x100,
            high: -1
        }
    );
    assert_eq!(
        parse_gpu_selector("pci:3:0.1").unwrap(),
        GpuSelector::PciBusAndName {
            bus: 3,
            device: Some(0),
            function: Some(1),
            name: None,
        }
    );
    assert!(parse_gpu_selector("   ").is_err());
}
