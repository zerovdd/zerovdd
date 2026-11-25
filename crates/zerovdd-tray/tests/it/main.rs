//! Unit and integration tests for `zerovdd-tray` menu model and presets.

use zerovdd_proto::ipc::{
    ActiveDisplayInfo, DisplayOrientation, GpuAdapterSummary, IpcRequest, ServiceHealth,
    SessionStatus, TopologyMode,
};
use zerovdd_tray::model::{
    build_tray_menu, format_refresh_label, format_status_header, MenuAction, MenuNode, TraySnapshot,
};
use zerovdd_tray::presets::QUICK_ADD_PRESETS;

fn find_submenu<'a>(
    nodes: &'a [MenuNode],
    prefix: &str,
) -> Option<(&'a str, bool, &'a [MenuNode])> {
    nodes.iter().find_map(|node| match node {
        MenuNode::Submenu {
            label,
            enabled,
            children,
        } if label.starts_with(prefix) => Some((label.as_str(), *enabled, children.as_slice())),
        _ => None,
    })
}

fn find_item<'a>(
    nodes: &'a [MenuNode],
    label_match: &str,
) -> Option<(bool, bool, Option<&'a MenuAction>)> {
    nodes.iter().find_map(|node| match node {
        MenuNode::Item {
            label,
            enabled,
            checked,
            action,
        } if label == label_match => Some((*enabled, *checked, action.as_ref())),
        _ => None,
    })
}

#[test]
fn quick_add_presets_produce_valid_ipc_requests() {
    assert_eq!(QUICK_ADD_PRESETS.len(), 6);

    for preset in QUICK_ADD_PRESETS {
        let req = preset.to_ipc_request();
        req.validate()
            .unwrap_or_else(|err| panic!("preset `{}` invalid: {err}", preset.label));
        let wrapped = IpcRequest::DisplayAdd(req.clone());
        let json = serde_json::to_string(&wrapped).expect("serialize DisplayAdd");
        let parsed: IpcRequest = serde_json::from_str(&json).expect("deserialize DisplayAdd");
        assert_eq!(parsed, wrapped);
    }

    let portrait = QUICK_ADD_PRESETS
        .iter()
        .find(|p| p.orientation == DisplayOrientation::Rotate90)
        .expect("portrait preset present");
    assert_eq!(portrait.width, 1920);
    assert_eq!(portrait.height, 1080);

    let iso = QUICK_ADD_PRESETS
        .iter()
        .find(|p| p.topology_mode == TopologyMode::Isolate)
        .expect("4K HDR Isolate preset present");
    let iso_req = iso.to_ipc_request();
    assert_eq!(iso_req.make_primary, Some(true));
    assert!(iso_req.hdr);
}

#[test]
fn offline_menu_disables_service_actions_while_keeping_local_tools_enabled() {
    let snapshot = TraySnapshot::default();
    assert_eq!(
        format_status_header(&snapshot),
        "ZeroVDD -- Service Offline"
    );

    let menu = build_tray_menu(&snapshot, false);
    let (_, quick_enabled, _) = find_submenu(&menu, "Quick Add Display").unwrap();
    assert!(!quick_enabled);

    let (_, active_enabled, _) = find_submenu(&menu, "Active Displays").unwrap();
    assert!(!active_enabled);

    let (_, gpu_enabled, _) = find_submenu(&menu, "Render GPU").unwrap();
    assert!(!gpu_enabled);

    let (restore_enabled, _, _) = find_item(&menu, "Restore Physical Displays").unwrap();
    assert!(!restore_enabled);

    let (config_enabled, _, _) = find_item(&menu, "Open config.toml").unwrap();
    assert!(config_enabled);

    let (doctor_enabled, _, _) = find_item(&menu, "Copy Doctor Report (JSON)").unwrap();
    assert!(doctor_enabled);

    let (auto_enabled, auto_checked, _) = find_item(&menu, "Start with Windows").unwrap();
    assert!(auto_enabled);
    assert!(!auto_checked);

    let (exit_enabled, _, exit_action) = find_item(&menu, "Exit").unwrap();
    assert!(exit_enabled);
    assert_eq!(exit_action, Some(&MenuAction::Exit));
}

#[test]
fn idle_and_sunshine_active_headers_format_accurately() {
    let mut snapshot = TraySnapshot {
        service_online: true,
        health: Some(ServiceHealth {
            service_version: "0.1.0".to_string(),
            driver_connected: true,
            protocol_version: Some((1, 1, 0)),
            active_displays: 0,
            uptime_secs: 42,
            backup_present: false,
        }),
        configured_gpu_selector: "auto".to_string(),
        ..Default::default()
    };
    assert_eq!(
        format_status_header(&snapshot),
        "ZeroVDD -- Idle (0 virtual displays)"
    );

    snapshot.session = Some(SessionStatus {
        active: true,
        client_id: Some("macbook-pro".to_string()),
        display_name: Some(r"\\.\DISPLAY2".to_string()),
        target_id: Some(77),
        width: 3840,
        height: 2160,
        refresh_mhz: 120_000,
        hdr_active: true,
        owner_pid: Some(4321),
        topology_mode: Some(TopologyMode::Isolate),
    });
    assert_eq!(
        format_status_header(&snapshot),
        "ZeroVDD -- Sunshine Active (macbook-pro)"
    );

    let menu = build_tray_menu(&snapshot, true);
    let (_, _, sun_children) = find_submenu(&menu, "Sunshine").unwrap();
    let (stop_enabled, _, stop_action) = find_item(sun_children, "Stop Active Session").unwrap();
    assert!(stop_enabled);
    assert_eq!(stop_action, Some(&MenuAction::StopSunshineSession));
}

#[test]
fn active_displays_and_gpu_selection_checkmarks_reflect_snapshot() {
    let snapshot = TraySnapshot {
        service_online: true,
        configured_gpu_selector: "luid:0x1a2b:0".to_string(),
        displays: vec![ActiveDisplayInfo {
            monitor_guid: "00000000-0000-0000-0000-000000000001".to_string(),
            client_name: "desk-4k".to_string(),
            adapter_luid_low: 0x1a2b,
            adapter_luid_high: 0,
            target_id: 99,
            connector_index: 0,
            width: 3840,
            height: 2160,
            refresh_num: 120,
            refresh_den: 1,
            hdr_active: true,
            ownership_mode: 0,
            lease_remaining_secs: 0,
        }],
        gpus: vec![
            GpuAdapterSummary {
                friendly_name: "NVIDIA GeForce RTX 4090".to_string(),
                adapter_luid_low: 0x1a2b,
                adapter_luid_high: 0,
                pci_bus: Some(1),
                pci_device: Some(0),
                pci_function: Some(0),
                dedicated_vram_bytes: 24 * 1024 * 1024 * 1024,
                is_software: false,
                is_best: true,
            },
            GpuAdapterSummary {
                friendly_name: "Microsoft Basic Render Driver".to_string(),
                adapter_luid_low: 0x9999,
                adapter_luid_high: 0,
                pci_bus: None,
                pci_device: None,
                pci_function: None,
                dedicated_vram_bytes: 0,
                is_software: true,
                is_best: false,
            },
        ],
        ..Default::default()
    };

    assert_eq!(format_refresh_label(60_000, 1001), "59.94Hz");
    assert_eq!(
        format_status_header(&snapshot),
        "ZeroVDD -- 1 Virtual Display"
    );

    let menu = build_tray_menu(&snapshot, false);
    let (_, _, active_children) = find_submenu(&menu, "Active Displays (1)").unwrap();
    let (_, _, display_actions) =
        find_submenu(active_children, "desk-4k (3840x2160 @ 120Hz HDR)").unwrap();

    let (_, hdr_checked, hdr_action) =
        find_item(display_actions, "Toggle HDR (On -> Off)").unwrap();
    assert!(hdr_checked);
    assert_eq!(
        hdr_action,
        Some(&MenuAction::ToggleDisplayHdr {
            target_id: 99,
            width: 3840,
            height: 2160,
            refresh_mhz: 120_000,
            enable_hdr: false,
        })
    );

    let (_, _, mode_children) = find_submenu(display_actions, "Switch Resolution").unwrap();
    let (_, mode_checked, _) = find_item(mode_children, "4K @ 120 Hz").unwrap();
    assert!(mode_checked);

    let (_, _, gpu_children) = find_submenu(&menu, "Render GPU").unwrap();
    let (_, auto_checked, _) = find_item(gpu_children, "Auto (Best Dedicated GPU)").unwrap();
    assert!(!auto_checked);
    let (_, rtx_checked, _) =
        find_item(gpu_children, "NVIDIA GeForce RTX 4090 [24 GiB, Best]").unwrap();
    assert!(rtx_checked);
}
