use core::mem::{align_of, size_of};
use std::io::Cursor;

use proptest::prelude::*;
use zerovdd_proto::ioctl::{
    ctl_code, ioctl_access, ioctl_device_type, ioctl_function, ioctl_method,
    CONTROL_DEVICE_DOS_PATH, CONTROL_DEVICE_NT_PATH, CONTROL_DEVICE_SYMBOLIC_LINK,
    FILE_DEVICE_UNKNOWN, FILE_READ_ACCESS, FILE_READ_WRITE_ACCESS, IOCTL_ADD_DISPLAY,
    IOCTL_GET_STATUS, IOCTL_GET_VERSION, IOCTL_LIST_DISPLAYS, IOCTL_REMOVE_DISPLAY,
    IOCTL_RENEW_LEASE, IOCTL_SET_RENDER_ADAPTER, IOCTL_UPDATE_DISPLAY_MODE, METHOD_BUFFERED,
    PROTOCOL_FLAG_COLOR_AND_PPI, PROTOCOL_FLAG_UPDATE_MODES, PROTOCOL_MAJOR, PROTOCOL_MINOR,
    ZEROVDD_CONTROL_INTERFACE_GUID, ZEROVDD_CONTROL_INTERFACE_GUID_STR,
};
use zerovdd_proto::ipc::{
    decode_frame, encode_frame, read_frame, write_frame, ActiveDisplayInfo, BitDepth, ColorSpace,
    DisplayAddRequest, DisplayOrientation, DisplayRemoveRequest, DisplaySetModeRequest,
    GpuAdapterSummary, IpcEnvelope, IpcError, IpcErrorPayload, IpcRequest, IpcResponse,
    ServiceHealth, SessionStatus, StartSessionRequest, StopSessionRequest, SwitchGpuRequest,
    TopologyMode, IPC_PIPE_NAME, IPC_PROTOCOL_VERSION, MAX_IPC_FRAME_BYTES,
};
use zerovdd_proto::types::{
    AddDisplayRequest, AddDisplayResponse, BitDepthPreference, ColorSpacePreference, DisplayInfo,
    DriverStatusResponse, HdrMasteringMetadata, HdrProfile, ListDisplaysResponse, OwnershipMode,
    ProtocolVersion, RemoveDisplayRequest, RenewLeaseRequest, SetRenderAdapterRequest,
    UpdateDisplayModeRequest, UpdateDisplayModeResponse, MAX_CLIENT_NAME_LEN, MAX_LIST_DISPLAYS,
    MAX_SERIAL_LEN, REMOVE_FLAG_BY_TARGET_ID, UPDATE_MODE_FLAG_BY_TARGET_ID,
};
use zerovdd_proto::validation::{ProtoError, Validate, MAX_LEASE_TTL_SECS};

const _: () = {
    assert!(size_of::<ProtocolVersion>() == ProtocolVersion::WIRE_SIZE);
    assert!(align_of::<ProtocolVersion>() == 2);
    assert!(size_of::<HdrMasteringMetadata>() == HdrMasteringMetadata::WIRE_SIZE);
    assert!(align_of::<HdrMasteringMetadata>() == 4);
    assert!(size_of::<AddDisplayRequest>() == AddDisplayRequest::WIRE_SIZE);
    assert!(align_of::<AddDisplayRequest>() == 4);
    assert!(size_of::<AddDisplayResponse>() == AddDisplayResponse::WIRE_SIZE);
    assert!(align_of::<AddDisplayResponse>() == 4);
    assert!(size_of::<RemoveDisplayRequest>() == RemoveDisplayRequest::WIRE_SIZE);
    assert!(align_of::<RemoveDisplayRequest>() == 4);
    assert!(size_of::<DisplayInfo>() == DisplayInfo::WIRE_SIZE);
    assert!(align_of::<DisplayInfo>() == 4);
    assert!(size_of::<ListDisplaysResponse>() == ListDisplaysResponse::WIRE_SIZE);
    assert!(align_of::<ListDisplaysResponse>() == 4);
    assert!(size_of::<SetRenderAdapterRequest>() == SetRenderAdapterRequest::WIRE_SIZE);
    assert!(align_of::<SetRenderAdapterRequest>() == 4);
    assert!(size_of::<RenewLeaseRequest>() == RenewLeaseRequest::WIRE_SIZE);
    assert!(align_of::<RenewLeaseRequest>() == 4);
    assert!(size_of::<DriverStatusResponse>() == DriverStatusResponse::WIRE_SIZE);
    assert!(align_of::<DriverStatusResponse>() == 8);
    assert!(size_of::<UpdateDisplayModeRequest>() == UpdateDisplayModeRequest::WIRE_SIZE);
    assert!(align_of::<UpdateDisplayModeRequest>() == 4);
    assert!(size_of::<UpdateDisplayModeResponse>() == UpdateDisplayModeResponse::WIRE_SIZE);
    assert!(align_of::<UpdateDisplayModeResponse>() == 4);
};

fn sample_guid() -> [u8; 16] {
    [
        0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF,
        0x01,
    ]
}

fn valid_add_request() -> AddDisplayRequest {
    let mut req = AddDisplayRequest::new(
        sample_guid(),
        2560,
        1600,
        120_000,
        1001,
        HdrProfile::Hdr10,
        OwnershipMode::HandleBound,
    );
    req.set_client_name("LivingRoom-Shield").unwrap();
    req.set_serial("SN00012345").unwrap();
    req
}

#[test]
fn control_device_constants_and_ioctl_codes_are_consistent() {
    assert_eq!(CONTROL_DEVICE_DOS_PATH, r"\\.\ZeroVddControl");
    assert_eq!(CONTROL_DEVICE_NT_PATH, r"\Device\ZeroVddControl");
    assert_eq!(CONTROL_DEVICE_SYMBOLIC_LINK, r"\DosDevices\ZeroVddControl");
    assert!(ZEROVDD_CONTROL_INTERFACE_GUID_STR.starts_with('{'));
    assert_eq!(ZEROVDD_CONTROL_INTERFACE_GUID.data1, 0x2963_AE53);

    let all_ioctls = [
        (IOCTL_GET_VERSION, 0x800, FILE_READ_ACCESS),
        (IOCTL_ADD_DISPLAY, 0x801, FILE_READ_WRITE_ACCESS),
        (IOCTL_REMOVE_DISPLAY, 0x802, FILE_READ_WRITE_ACCESS),
        (IOCTL_LIST_DISPLAYS, 0x803, FILE_READ_ACCESS),
        (IOCTL_SET_RENDER_ADAPTER, 0x804, FILE_READ_WRITE_ACCESS),
        (IOCTL_RENEW_LEASE, 0x805, FILE_READ_WRITE_ACCESS),
        (IOCTL_GET_STATUS, 0x806, FILE_READ_ACCESS),
    ];

    for (idx, &(code, func, access)) in all_ioctls.iter().enumerate() {
        assert_eq!(
            code,
            ctl_code(
                FILE_DEVICE_UNKNOWN,
                u32::from(func),
                METHOD_BUFFERED,
                access
            )
        );
        assert_eq!(ioctl_device_type(code), FILE_DEVICE_UNKNOWN as u16);
        assert_eq!(ioctl_function(code), func);
        assert_eq!(ioctl_method(code), METHOD_BUFFERED as u8);
        assert_eq!(ioctl_access(code), access as u8);
        for &(other_code, _, _) in &all_ioctls[idx + 1..] {
            assert_ne!(code, other_code);
        }
    }
}

#[test]
fn ioctl_payloads_roundtrip_wire_encoding() {
    let ver = ProtocolVersion::CURRENT;
    assert_eq!(ProtocolVersion::from_bytes(&ver.to_bytes()).unwrap(), ver);

    let mut add_req = valid_add_request();
    let mut custom_edid = [0u8; 128];
    custom_edid[0..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
    add_req.set_custom_edid(&custom_edid).unwrap();
    let add_decoded = AddDisplayRequest::from_bytes(&add_req.to_bytes()).unwrap();
    assert_eq!(add_decoded, add_req);
    assert_eq!(add_decoded.client_name_str().unwrap(), "LivingRoom-Shield");
    assert_eq!(add_decoded.serial_str().unwrap(), "SN00012345");
    assert_eq!(add_decoded.custom_edid_slice().unwrap(), &custom_edid);
    assert_eq!(add_decoded.refresh_mhz(), 119_880);

    let add_resp = AddDisplayResponse {
        adapter_luid_low: 0xDEAD_BEEF,
        adapter_luid_high: 1,
        target_id: 0x0001_0002,
        connector_index: 3,
        created_new: 1,
        reserved: [0; 3],
    };
    assert_eq!(
        AddDisplayResponse::from_bytes(&add_resp.to_bytes()).unwrap(),
        add_resp
    );

    let rem_req = RemoveDisplayRequest::by_guid(sample_guid());
    assert_eq!(
        RemoveDisplayRequest::from_bytes(&rem_req.to_bytes()).unwrap(),
        rem_req
    );
    let rem_by_target = RemoveDisplayRequest::by_target_id(77);
    assert_eq!(
        RemoveDisplayRequest::from_bytes(&rem_by_target.to_bytes()).unwrap(),
        rem_by_target
    );

    let mut list_resp = ListDisplaysResponse::empty();
    list_resp.count = 1;
    list_resp.total_active = 1;
    list_resp.displays[0] = DisplayInfo {
        monitor_guid: sample_guid(),
        adapter_luid_low: 42,
        adapter_luid_high: 0,
        target_id: 99,
        connector_index: 0,
        width: 3840,
        height: 2160,
        refresh_num: 120,
        refresh_den: 1,
        lease_remaining_secs: 0,
        hdr_profile: HdrProfile::Hdr10.as_u8(),
        ownership_mode: OwnershipMode::HandleBound.as_u8(),
        client_name_len: 4,
        active: 1,
        client_name: {
            let mut arr = [0u8; MAX_CLIENT_NAME_LEN];
            arr[..4].copy_from_slice(b"Deck");
            arr
        },
    };
    let list_decoded = ListDisplaysResponse::from_bytes(&list_resp.to_bytes()).unwrap();
    assert_eq!(list_decoded, list_resp);
    assert_eq!(list_decoded.entries().unwrap().len(), 1);
    assert_eq!(
        list_decoded.entries().unwrap()[0]
            .client_name_str()
            .unwrap(),
        "Deck"
    );

    let gpu_req = SetRenderAdapterRequest::new(0x1234_5678, 0);
    assert_eq!(
        SetRenderAdapterRequest::from_bytes(&gpu_req.to_bytes()).unwrap(),
        gpu_req
    );

    let lease_req = RenewLeaseRequest::new(sample_guid(), 120);
    assert_eq!(
        RenewLeaseRequest::from_bytes(&lease_req.to_bytes()).unwrap(),
        lease_req
    );

    let status_resp = DriverStatusResponse {
        protocol_version: ProtocolVersion::CURRENT,
        total_arrivals: 5,
        total_departures: 4,
        uptime_secs: 3600,
        render_adapter_luid_low: 0xABCD_0001,
        render_adapter_luid_high: 0,
        active_display_count: 1,
        max_displays: 16,
        adapter_ready: 1,
        hdr_capable: 1,
        last_gpu_switch_failed: 0,
        reserved: [0; 5],
    };
    assert_eq!(
        DriverStatusResponse::from_bytes(&status_resp.to_bytes()).unwrap(),
        status_resp
    );
}

#[test]
fn handles_full_64_byte_non_nul_terminated_name_and_16_byte_serial_safely() {
    let mut req = valid_add_request();
    let full_name = "A".repeat(MAX_CLIENT_NAME_LEN);
    let full_serial = "S".repeat(MAX_SERIAL_LEN);
    req.set_client_name(&full_name).unwrap();
    req.set_serial(&full_serial).unwrap();

    // WHY: Explicitly verify no byte in the fixed array is NUL so decoding proves
    // bounded slice handling without strlen assumptions.
    assert!(!req.client_name.contains(&0));
    assert!(!req.serial.contains(&0));

    let decoded = AddDisplayRequest::from_bytes(&req.to_bytes()).unwrap();
    assert_eq!(decoded.client_name_str().unwrap(), full_name);
    assert_eq!(decoded.serial_str().unwrap(), full_serial);

    let mut padded = valid_add_request();
    padded.client_name = [0xFF; MAX_CLIENT_NAME_LEN];
    padded.client_name[..6].copy_from_slice(b"Laptop");
    padded.client_name_len = 6;
    let decoded_padded = AddDisplayRequest::from_bytes(&padded.to_bytes()).unwrap();
    assert_eq!(decoded_padded.client_name_str().unwrap(), "Laptop");
}

#[test]
fn rejects_truncated_and_oversized_wire_buffers() {
    let bytes = valid_add_request().to_bytes();
    assert!(matches!(
        AddDisplayRequest::from_bytes(&bytes[..bytes.len() - 1]),
        Err(ProtoError::BufferTooShort { .. })
    ));
    let mut oversized = bytes.to_vec();
    oversized.push(0);
    assert!(matches!(
        AddDisplayRequest::from_bytes(&oversized),
        Err(ProtoError::BufferSizeMismatch { .. })
    ));

    assert!(matches!(
        ProtocolVersion::from_bytes(&[1, 0, 0]),
        Err(ProtoError::BufferTooShort { .. })
    ));
}

#[test]
fn rejects_invalid_fields_and_unsupported_versions() {
    let mut zero_w = valid_add_request();
    zero_w.width = 0;
    assert!(matches!(
        zero_w.validate(),
        Err(ProtoError::InvalidDimensions { .. })
    ));

    let mut huge_h = valid_add_request();
    huge_h.height = 20_000;
    assert!(matches!(
        huge_h.validate(),
        Err(ProtoError::InvalidDimensions { .. })
    ));

    let mut zero_refresh = valid_add_request();
    zero_refresh.refresh_num = 0;
    assert!(matches!(
        zero_refresh.validate(),
        Err(ProtoError::InvalidRefreshRate { .. })
    ));

    let mut zero_den = valid_add_request();
    zero_den.refresh_den = 0;
    assert!(matches!(
        zero_den.validate(),
        Err(ProtoError::InvalidRefreshRate { .. })
    ));
    assert_eq!(zero_den.refresh_mhz(), 0);

    let mut bad_utf8 = valid_add_request();
    bad_utf8.client_name_len = 2;
    bad_utf8.client_name[0] = 0xFF;
    bad_utf8.client_name[1] = 0xFE;
    assert!(matches!(
        bad_utf8.validate(),
        Err(ProtoError::InvalidUtf8 {
            field: "client_name"
        })
    ));

    let mut ctrl_name = valid_add_request();
    ctrl_name.client_name_len = 4;
    ctrl_name.client_name[..4].copy_from_slice(b"hi\0!");
    assert!(matches!(
        ctrl_name.validate(),
        Err(ProtoError::NonPrintableAscii {
            field: "client_name"
        })
    ));

    let mut bad_len = valid_add_request();
    bad_len.client_name_len = (MAX_CLIENT_NAME_LEN as u8) + 1;
    assert!(matches!(
        bad_len.validate(),
        Err(ProtoError::FieldLengthOutOfBounds {
            field: "client_name",
            ..
        })
    ));

    let mut bad_serial = valid_add_request();
    bad_serial.serial_len = 2;
    bad_serial.serial[0] = b'A';
    bad_serial.serial[1] = 0x80;
    assert!(matches!(
        bad_serial.validate(),
        Err(ProtoError::InvalidUtf8 { field: "serial" })
    ));

    let mut bad_mode = valid_add_request();
    bad_mode.ownership_mode = 99;
    assert!(matches!(
        bad_mode.validate(),
        Err(ProtoError::InvalidOwnershipMode(99))
    ));

    let mut bad_leased_ttl = valid_add_request();
    bad_leased_ttl.ownership_mode = OwnershipMode::Leased.as_u8();
    bad_leased_ttl.lease_ttl_secs = 0;
    assert!(matches!(
        bad_leased_ttl.validate(),
        Err(ProtoError::InvalidLeaseTtl(0))
    ));

    let mut bad_hdr = valid_add_request();
    bad_hdr.hdr_profile = 5;
    assert!(matches!(
        bad_hdr.validate(),
        Err(ProtoError::InvalidHdrProfile(5))
    ));

    let mut bad_edid_len = valid_add_request();
    bad_edid_len.custom_edid_len = 64;
    assert!(matches!(
        bad_edid_len.validate(),
        Err(ProtoError::InvalidCustomEdidLength(64))
    ));

    let mut bad_major = valid_add_request();
    bad_major.protocol_version.major = PROTOCOL_MAJOR + 1;
    assert!(matches!(
        bad_major.validate(),
        Err(ProtoError::UnsupportedMajorVersion { .. })
    ));

    let bad_resp = AddDisplayResponse {
        adapter_luid_low: 1,
        adapter_luid_high: 0,
        target_id: 2,
        connector_index: 0,
        created_new: 2,
        reserved: [0; 3],
    };
    assert!(matches!(
        bad_resp.validate(),
        Err(ProtoError::InvalidBooleanByte {
            field: "created_new",
            value: 2
        })
    ));

    let mut bad_list = ListDisplaysResponse::empty();
    bad_list.count = (MAX_LIST_DISPLAYS as u32) + 1;
    assert!(matches!(
        bad_list.validate(),
        Err(ProtoError::ListCountOutOfBounds { .. })
    ));

    let mut zero_guid_add = valid_add_request();
    zero_guid_add.monitor_guid = [0; 16];
    assert!(matches!(
        zero_guid_add.validate(),
        Err(ProtoError::InvalidSelector)
    ));

    let mut bad_hdr_meta = valid_add_request();
    bad_hdr_meta.hdr_metadata = HdrMasteringMetadata {
        max_luminance_nits: 400,
        max_fall_nits: 200,
        min_luminance_ten_thousandths: 5_000_000,
    };
    assert!(matches!(
        bad_hdr_meta.validate(),
        Err(ProtoError::InvalidHdrMetadata { .. })
    ));

    let mut bad_remove_flags = RemoveDisplayRequest::by_guid(sample_guid());
    bad_remove_flags.flags = 0x2;
    assert!(matches!(
        bad_remove_flags.validate(),
        Err(ProtoError::InvalidFlags(0x2))
    ));

    let bad_remove_zero_target = RemoveDisplayRequest {
        protocol_version: ProtocolVersion::CURRENT,
        monitor_guid: sample_guid(),
        target_id: 0,
        flags: REMOVE_FLAG_BY_TARGET_ID,
    };
    assert!(matches!(
        bad_remove_zero_target.validate(),
        Err(ProtoError::InvalidSelector)
    ));

    let bad_remove_zero_guid = RemoveDisplayRequest {
        protocol_version: ProtocolVersion::CURRENT,
        monitor_guid: [0; 16],
        target_id: 42,
        flags: 0,
    };
    assert!(matches!(
        bad_remove_zero_guid.validate(),
        Err(ProtoError::InvalidSelector)
    ));
}

#[test]
fn ipc_envelopes_roundtrip_via_frame_and_stream_codecs() {
    let start_req = IpcEnvelope::new(
        101,
        IpcRequest::StartSession(StartSessionRequest {
            client_id: "moonlight-living-room".to_string(),
            width: 3840,
            height: 2160,
            refresh_mhz: 119_880,
            hdr: true,
            owner_pid: 4242,
            owner_creation_time: 133_700_000_000_000_000,
            topology_mode: TopologyMode::Isolate,
        }),
    );
    start_req.validate_version().unwrap();
    if let IpcRequest::StartSession(ref inner) = start_req.payload {
        inner.validate().unwrap();
    }

    let encoded = encode_frame(&start_req).unwrap();
    let decoded: IpcEnvelope<IpcRequest> = decode_frame(&encoded).unwrap();
    assert_eq!(decoded, start_req);

    let other_requests = [
        IpcEnvelope::new(
            102,
            IpcRequest::StopSession(StopSessionRequest {
                client_id: Some("moonlight-living-room".to_string()),
                owner_pid: Some(4242),
                force: false,
            }),
        ),
        IpcEnvelope::new(103, IpcRequest::GetSessionStatus),
        IpcEnvelope::new(
            104,
            IpcRequest::DisplayAdd(DisplayAddRequest {
                client_id: "headless".to_string(),
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
                hdr: false,
                lease_ttl_secs: Some(300),
                topology_mode: Some(TopologyMode::Extend),
                ..Default::default()
            }),
        ),
        IpcEnvelope::new(
            105,
            IpcRequest::DisplayRemove(DisplayRemoveRequest {
                client_id: Some("headless".to_string()),
                target_id: None,
            }),
        ),
        IpcEnvelope::new(106, IpcRequest::GetHealth),
    ];

    let mut stream = Vec::new();
    for msg in &other_requests {
        write_frame(&mut stream, msg).unwrap();
    }
    let mut cursor = Cursor::new(stream);
    for expected in &other_requests {
        let got: IpcEnvelope<IpcRequest> = read_frame(&mut cursor).unwrap();
        assert_eq!(&got, expected);
    }

    let responses = [
        IpcEnvelope::new(
            101,
            IpcResponse::SessionStarted(SessionStatus {
                active: true,
                client_id: Some("moonlight-living-room".to_string()),
                display_name: Some(r"\\.\DISPLAY2".to_string()),
                target_id: Some(257),
                width: 3840,
                height: 2160,
                refresh_mhz: 119_880,
                hdr_active: true,
                owner_pid: Some(4242),
                topology_mode: Some(TopologyMode::Isolate),
            }),
        ),
        IpcEnvelope::new(
            106,
            IpcResponse::Health(ServiceHealth {
                service_version: "0.1.0".to_string(),
                driver_connected: true,
                protocol_version: Some((1, 0, 0)),
                active_displays: 1,
                uptime_secs: 90,
                backup_present: true,
            }),
        ),
        IpcEnvelope::new(
            107,
            IpcResponse::Error(IpcErrorPayload {
                code: "DRIVER_UNAVAILABLE".to_string(),
                message: "control device not found".to_string(),
            }),
        ),
    ];

    for resp in &responses {
        let frame = encode_frame(resp).unwrap();
        let decoded_resp: IpcEnvelope<IpcResponse> = decode_frame(&frame).unwrap();
        assert_eq!(&decoded_resp, resp);
    }
}

#[test]
fn display_add_request_topology_mode_json_compat_and_roundtrip() {
    let legacy_json = r#"{"client_id":"legacy","width":1920,"height":1080,"refresh_mhz":60000,"hdr":false,"lease_ttl_secs":null}"#;
    let decoded: DisplayAddRequest = serde_json::from_str(legacy_json).unwrap();
    assert_eq!(decoded.topology_mode, None);

    let encoded_none = serde_json::to_string(&decoded).unwrap();
    assert!(!encoded_none.contains("topology_mode"));

    for mode in [TopologyMode::Extend, TopologyMode::Isolate] {
        let req = DisplayAddRequest {
            client_id: "sunshine-test".to_string(),
            width: 3024,
            height: 1890,
            refresh_mhz: 120_000,
            hdr: true,
            lease_ttl_secs: None,
            topology_mode: Some(mode),
            ..Default::default()
        };
        let env = IpcEnvelope::new(77, IpcRequest::DisplayAdd(req));
        let frame = encode_frame(&env).unwrap();
        let back: IpcEnvelope<IpcRequest> = decode_frame(&frame).unwrap();
        assert_eq!(back, env);
    }
}

#[test]
fn ipc_rejects_oversized_truncated_empty_and_bad_version_frames() {
    assert!(matches!(
        decode_frame::<IpcEnvelope<IpcRequest>>(&[1, 2, 3]),
        Err(IpcError::TruncatedHeader { actual: 3 })
    ));

    let empty_len = 0u32.to_le_bytes();
    assert!(matches!(
        decode_frame::<IpcEnvelope<IpcRequest>>(&empty_len),
        Err(IpcError::EmptyFrame)
    ));

    let too_big_len = ((MAX_IPC_FRAME_BYTES as u32) + 1).to_le_bytes();
    assert!(matches!(
        decode_frame::<IpcEnvelope<IpcRequest>>(&too_big_len),
        Err(IpcError::FrameTooLarge { .. })
    ));

    let mut huge_stream = Cursor::new(too_big_len);
    assert!(matches!(
        read_frame::<_, IpcEnvelope<IpcRequest>>(&mut huge_stream),
        Err(IpcError::FrameTooLarge { .. })
    ));

    let mismatch_frame = [10, 0, 0, 0, b'{', b'}'];
    assert!(matches!(
        decode_frame::<IpcEnvelope<IpcRequest>>(&mismatch_frame),
        Err(IpcError::FrameLengthMismatch {
            declared: 10,
            actual: 2
        })
    ));

    let huge_payload = StartSessionRequest {
        client_id: "x".repeat(MAX_IPC_FRAME_BYTES),
        width: 1920,
        height: 1080,
        refresh_mhz: 60_000,
        hdr: false,
        owner_pid: 1,
        owner_creation_time: 1,
        topology_mode: TopologyMode::Extend,
    };
    assert!(matches!(
        encode_frame(&huge_payload),
        Err(IpcError::FrameTooLarge { .. })
    ));

    let bad_ver = IpcEnvelope {
        version: IPC_PROTOCOL_VERSION + 1,
        request_id: 1,
        payload: IpcRequest::GetHealth,
    };
    assert!(matches!(
        bad_ver.validate_version(),
        Err(IpcError::UnsupportedVersion { .. })
    ));

    assert_eq!(IPC_PIPE_NAME, r"\\.\pipe\zerovdd-svc");

    let mut bad_add_ttl = DisplayAddRequest {
        client_id: "deck".to_string(),
        width: 1920,
        height: 1080,
        refresh_mhz: 60_000,
        hdr: false,
        lease_ttl_secs: Some(0),
        topology_mode: None,
        ..Default::default()
    };
    assert!(matches!(
        bad_add_ttl.validate(),
        Err(IpcError::InvalidPayload(ProtoError::InvalidLeaseTtl(0)))
    ));
    bad_add_ttl.lease_ttl_secs = Some(MAX_LEASE_TTL_SECS + 1);
    assert!(matches!(
        bad_add_ttl.validate(),
        Err(IpcError::InvalidPayload(ProtoError::InvalidLeaseTtl(_)))
    ));
    bad_add_ttl.lease_ttl_secs = Some(MAX_LEASE_TTL_SECS);
    bad_add_ttl.validate().unwrap();

    let ok_stop = StopSessionRequest {
        client_id: Some("deck".to_string()),
        owner_pid: Some(1234),
        force: false,
    };
    ok_stop.validate().unwrap();
    let bad_stop = StopSessionRequest {
        client_id: Some("bad\nname".to_string()),
        owner_pid: None,
        force: false,
    };
    assert!(matches!(
        bad_stop.validate(),
        Err(IpcError::InvalidClientId)
    ));

    let ok_remove_id = DisplayRemoveRequest {
        client_id: None,
        target_id: Some(7),
    };
    ok_remove_id.validate().unwrap();
    let empty_remove = DisplayRemoveRequest {
        client_id: None,
        target_id: None,
    };
    assert!(matches!(
        empty_remove.validate(),
        Err(IpcError::InvalidPayload(ProtoError::InvalidSelector))
    ));
    let zero_target_remove = DisplayRemoveRequest {
        client_id: None,
        target_id: Some(0),
    };
    assert!(matches!(
        zero_target_remove.validate(),
        Err(IpcError::InvalidPayload(ProtoError::InvalidSelector))
    ));
    let bad_client_remove = DisplayRemoveRequest {
        client_id: Some("   ".to_string()),
        target_id: Some(7),
    };
    assert!(matches!(
        bad_client_remove.validate(),
        Err(IpcError::InvalidClientId)
    ));
}

proptest! {
    #[test]
    fn prop_valid_add_display_request_roundtrips(
        width in 320u32..=16384u32,
        height in 320u32..=16384u32,
        refresh_num in 10u32..=1000u32,
        guid in any::<[u8; 16]>().prop_filter("non-zero monitor_guid", |g| *g != [0u8; 16]),
        hdr in any::<bool>(),
        leased in any::<bool>(),
        ttl in 1u32..=86400u32,
        name in "[ -~]{0,64}",
        serial in "[ -~]{0,16}",
    ) {
        let ownership = if leased { OwnershipMode::Leased } else { OwnershipMode::HandleBound };
        let profile = if hdr { HdrProfile::Hdr10 } else { HdrProfile::Sdr };
        let mut req = AddDisplayRequest::new(guid, width, height, refresh_num, 1, profile, ownership);
        if leased {
            req.lease_ttl_secs = ttl;
        }
        req.set_client_name(&name).unwrap();
        req.set_serial(&serial).unwrap();

        let wire = req.to_bytes();
        let decoded = AddDisplayRequest::from_bytes(&wire).unwrap();
        prop_assert_eq!(decoded, req);
        prop_assert_eq!(decoded.client_name_str().unwrap(), name.as_str());
        prop_assert_eq!(decoded.serial_str().unwrap(), serial.as_str());
    }

    #[test]
    fn prop_arbitrary_bytes_never_panic_when_decoding(bytes in prop::collection::vec(any::<u8>(), 0..=800)) {
        let _ = AddDisplayRequest::from_bytes(&bytes);
        let _ = AddDisplayResponse::from_bytes(&bytes);
        let _ = RemoveDisplayRequest::from_bytes(&bytes);
        let _ = SetRenderAdapterRequest::from_bytes(&bytes);
        let _ = RenewLeaseRequest::from_bytes(&bytes);
        let _ = DriverStatusResponse::from_bytes(&bytes);
    }
}

#[test]
fn corpus_regression_replay() {
    let truncated = include_bytes!("../fixtures/corpus_truncated_header.bin");
    assert!(matches!(
        decode_frame::<IpcEnvelope<IpcRequest>>(truncated),
        Err(IpcError::TruncatedHeader { actual: 3 })
    ));
    assert!(matches!(
        AddDisplayRequest::from_bytes(truncated),
        Err(ProtoError::BufferTooShort { .. })
    ));
    assert!(matches!(
        RemoveDisplayRequest::from_bytes(truncated),
        Err(ProtoError::BufferTooShort { .. })
    ));
    assert!(matches!(
        SetRenderAdapterRequest::from_bytes(truncated),
        Err(ProtoError::BufferTooShort { .. })
    ));
    assert!(matches!(
        RenewLeaseRequest::from_bytes(truncated),
        Err(ProtoError::BufferTooShort { .. })
    ));
    assert!(matches!(
        DriverStatusResponse::from_bytes(truncated),
        Err(ProtoError::BufferTooShort { .. })
    ));
    assert!(matches!(
        ListDisplaysResponse::from_bytes(truncated),
        Err(ProtoError::BufferTooShort { .. })
    ));

    let empty_ipc = include_bytes!("../fixtures/corpus_empty_ipc_frame.bin");
    assert!(matches!(
        decode_frame::<IpcEnvelope<IpcRequest>>(empty_ipc),
        Err(IpcError::EmptyFrame)
    ));

    let oversized_ipc = include_bytes!("../fixtures/corpus_oversized_ipc_frame.bin");
    assert!(matches!(
        decode_frame::<IpcEnvelope<IpcRequest>>(oversized_ipc),
        Err(IpcError::FrameTooLarge { .. })
    ));

    let non_nul = include_bytes!("../fixtures/corpus_non_nul_name.bin");
    let decoded = AddDisplayRequest::from_bytes(non_nul).expect("non-NUL 64B name must decode");
    assert_eq!(
        decoded.client_name_str().unwrap().len(),
        MAX_CLIENT_NAME_LEN
    );
    assert_eq!(decoded.serial_str().unwrap().len(), MAX_SERIAL_LEN);
    assert_eq!(decoded.refresh_mhz(), 119_880);

    let bad_ver = include_bytes!("../fixtures/corpus_bad_version_add.bin");
    assert!(matches!(
        AddDisplayRequest::from_bytes(bad_ver),
        Err(ProtoError::UnsupportedMajorVersion {
            expected: PROTOCOL_MAJOR,
            actual: 2
        })
    ));

    let valid_ipc = include_bytes!("../fixtures/corpus_valid_ipc_start.bin");
    let env: IpcEnvelope<IpcRequest> =
        decode_frame(valid_ipc).expect("valid start_session IPC corpus must decode");
    env.validate_version().unwrap();
    match env.payload {
        IpcRequest::StartSession(req) => req.validate().unwrap(),
        other => panic!("unexpected IPC request variant: {other:?}"),
    }
}

#[test]
fn update_display_mode_ioctl_and_ipc_roundtrip_and_validation() {
    assert_eq!(ioctl_function(IOCTL_UPDATE_DISPLAY_MODE), 0x807);
    assert_eq!(
        ioctl_access(IOCTL_UPDATE_DISPLAY_MODE),
        FILE_READ_WRITE_ACCESS as u8
    );
    assert_ne!(
        ProtocolVersion::CURRENT.flags & PROTOCOL_FLAG_UPDATE_MODES,
        0
    );

    let conflicting_ipc_selector = DisplaySetModeRequest {
        client_id: Some("alpha".to_string()),
        target_id: Some(42),
        width: 1920,
        height: 1080,
        refresh_mhz: 60_000,
        hdr: None,
        ..Default::default()
    };
    assert!(matches!(
        conflicting_ipc_selector.validate(),
        Err(IpcError::InvalidPayload(ProtoError::InvalidSelector))
    ));

    let req = UpdateDisplayModeRequest::by_guid(
        sample_guid(),
        3024,
        1890,
        60_000,
        1001,
        HdrProfile::Hdr10,
    );
    let wire = req.to_bytes();
    let decoded = UpdateDisplayModeRequest::from_bytes(&wire).unwrap();
    assert_eq!(decoded, req);

    let by_tid = UpdateDisplayModeRequest::by_target_id(42, 2560, 1440, 120, 1, HdrProfile::Sdr);
    assert_ne!(by_tid.flags & UPDATE_MODE_FLAG_BY_TARGET_ID, 0);
    assert_eq!(
        UpdateDisplayModeRequest::from_bytes(&by_tid.to_bytes()).unwrap(),
        by_tid
    );

    let mut bad_zero_sel = req;
    bad_zero_sel.monitor_guid = [0; 16];
    assert!(matches!(
        bad_zero_sel.validate(),
        Err(ProtoError::InvalidSelector)
    ));

    let mut bad_flags = by_tid;
    bad_flags.flags |= 0x2;
    assert!(matches!(
        bad_flags.validate(),
        Err(ProtoError::InvalidFlags(0x3))
    ));

    let resp = UpdateDisplayModeResponse {
        adapter_luid_low: 0x1000,
        adapter_luid_high: 0,
        target_id: 256,
        connector_index: 0,
        mode_count: 14,
    };
    assert_eq!(
        UpdateDisplayModeResponse::from_bytes(&resp.to_bytes()).unwrap(),
        resp
    );

    let ipc_req = IpcEnvelope::new(
        9,
        IpcRequest::DisplaySetMode(DisplaySetModeRequest {
            client_id: None,
            target_id: None,
            width: 3024,
            height: 1890,
            refresh_mhz: 60_000,
            hdr: Some(true),
            ..Default::default()
        }),
    );
    let framed = encode_frame(&ipc_req).unwrap();
    let decoded_ipc: IpcEnvelope<IpcRequest> = decode_frame(&framed).unwrap();
    assert_eq!(decoded_ipc, ipc_req);
}

#[test]
fn protocol_v1_1_color_space_bit_depth_and_target_ppi_validation_and_roundtrip() {
    assert_eq!(PROTOCOL_MINOR, 1);
    assert_eq!(AddDisplayRequest::WIRE_SIZE, 656);
    assert_eq!(UpdateDisplayModeRequest::WIRE_SIZE, 60);
    assert_ne!(
        ProtocolVersion::CURRENT.flags & PROTOCOL_FLAG_COLOR_AND_PPI,
        0
    );

    assert!(matches!(
        ColorSpacePreference::try_from(99),
        Err(ProtoError::InvalidColorSpace(99))
    ));
    assert!(matches!(
        BitDepthPreference::try_from(99),
        Err(ProtoError::InvalidBitDepth(99))
    ));

    let mut add = valid_add_request();
    add.target_ppi = 140;
    add.color_space = ColorSpacePreference::DciP3.as_u8();
    add.bit_depth = BitDepthPreference::Bpc10.as_u8();
    assert_eq!(AddDisplayRequest::from_bytes(&add.to_bytes()).unwrap(), add);

    add.hdr_profile = HdrProfile::Sdr.as_u8();
    add.color_space = ColorSpacePreference::Bt709.as_u8();
    add.bit_depth = BitDepthPreference::Bpc8.as_u8();
    add.target_ppi = 48;
    assert_eq!(AddDisplayRequest::from_bytes(&add.to_bytes()).unwrap(), add);

    let mut bad_ppi = valid_add_request();
    bad_ppi.target_ppi = 47;
    assert!(matches!(
        bad_ppi.validate(),
        Err(ProtoError::InvalidTargetPpi(47))
    ));
    bad_ppi.target_ppi = 601;
    assert!(matches!(
        bad_ppi.validate(),
        Err(ProtoError::InvalidTargetPpi(601))
    ));

    let mut hdr_bpc8 = valid_add_request();
    hdr_bpc8.bit_depth = BitDepthPreference::Bpc8.as_u8();
    assert!(matches!(
        hdr_bpc8.validate(),
        Err(ProtoError::IncompatibleColorSettings { .. })
    ));

    let mut hdr_bt709 = valid_add_request();
    hdr_bt709.color_space = ColorSpacePreference::Bt709.as_u8();
    assert!(matches!(
        hdr_bt709.validate(),
        Err(ProtoError::IncompatibleColorSettings { .. })
    ));

    let mut sdr_p3 = valid_add_request();
    sdr_p3.hdr_profile = HdrProfile::Sdr.as_u8();
    sdr_p3.color_space = ColorSpacePreference::DciP3.as_u8();
    assert!(matches!(
        sdr_p3.validate(),
        Err(ProtoError::IncompatibleColorSettings { .. })
    ));

    let mut upd = UpdateDisplayModeRequest::by_guid(
        sample_guid(),
        3840,
        2160,
        120_000,
        1001,
        HdrProfile::Hdr10,
    );
    upd.color_space = ColorSpacePreference::DciP3.as_u8();
    upd.bit_depth = BitDepthPreference::Bpc12.as_u8();
    upd.hdr_metadata = HdrMasteringMetadata {
        max_luminance_nits: 1500,
        max_fall_nits: 500,
        min_luminance_ten_thousandths: 250,
    };
    assert_eq!(
        UpdateDisplayModeRequest::from_bytes(&upd.to_bytes()).unwrap(),
        upd
    );

    upd.bit_depth = BitDepthPreference::Bpc8.as_u8();
    assert!(matches!(
        upd.validate(),
        Err(ProtoError::IncompatibleColorSettings { .. })
    ));

    let ipc_add = DisplayAddRequest {
        client_id: "p3-display".to_string(),
        width: 3840,
        height: 2160,
        refresh_mhz: 120_000,
        hdr: true,
        target_ppi: Some(140),
        color_space: Some(ColorSpace::DciP3),
        bit_depth: Some(BitDepth::Bpc10),
        min_luminance_ten_thousandths: Some(250),
        ..Default::default()
    };
    ipc_add.validate().unwrap();
}

#[test]
fn ipc_parity_requests_responses_and_layout_fields_roundtrip_and_validate() {
    let add_layout = DisplayAddRequest {
        client_id: "portrait-panel".to_string(),
        width: 1920,
        height: 1080,
        refresh_mhz: 60_000,
        guid: Some("11223344-5566-7788-99aa-bbccddeeff01".to_string()),
        gpu: Some("auto".to_string()),
        orientation: Some(DisplayOrientation::Rotate90),
        position: Some((-1080, 0)),
        make_primary: Some(true),
        ..Default::default()
    };
    add_layout.validate().unwrap();

    let mut bad_guid = add_layout.clone();
    bad_guid.guid = Some("not-a-uuid".to_string());
    assert!(matches!(
        bad_guid.validate(),
        Err(IpcError::InvalidPayload(ProtoError::InvalidSelector))
    ));

    let frames = [
        IpcEnvelope::new(1, IpcRequest::DisplayAdd(add_layout)),
        IpcEnvelope::new(2, IpcRequest::DisplayList),
        IpcEnvelope::new(3, IpcRequest::ListGpus),
        IpcEnvelope::new(
            4,
            IpcRequest::SwitchGpu(SwitchGpuRequest {
                selector: "pci:1:0.0".to_string(),
            }),
        ),
        IpcEnvelope::new(5, IpcRequest::RestoreTopology),
    ];
    for req in &frames {
        let encoded = encode_frame(req).unwrap();
        let decoded: IpcEnvelope<IpcRequest> = decode_frame(&encoded).unwrap();
        assert_eq!(&decoded, req);
    }

    let replies = [
        IpcEnvelope::new(
            2,
            IpcResponse::DisplayList {
                displays: vec![ActiveDisplayInfo {
                    monitor_guid: "11223344-5566-7788-99aa-bbccddeeff01".to_string(),
                    client_name: "portrait-panel".to_string(),
                    adapter_luid_low: 0x100,
                    adapter_luid_high: 0,
                    target_id: 256,
                    connector_index: 0,
                    width: 1920,
                    height: 1080,
                    refresh_num: 60,
                    refresh_den: 1,
                    hdr_active: false,
                    ownership_mode: 0,
                    lease_remaining_secs: 0,
                }],
            },
        ),
        IpcEnvelope::new(
            3,
            IpcResponse::GpuList {
                gpus: vec![GpuAdapterSummary {
                    friendly_name: "RTX 4090".to_string(),
                    adapter_luid_low: 0x200,
                    adapter_luid_high: 0,
                    pci_bus: Some(1),
                    pci_device: Some(0),
                    pci_function: Some(0),
                    dedicated_vram_bytes: 24_000_000_000,
                    is_software: false,
                    is_best: true,
                }],
                configured_selector: "auto".to_string(),
                last_gpu_switch_failed: false,
            },
        ),
        IpcEnvelope::new(
            4,
            IpcResponse::GpuSwitched {
                adapter_luid_low: 0x200,
                adapter_luid_high: 0,
                friendly_name: "RTX 4090".to_string(),
            },
        ),
        IpcEnvelope::new(
            5,
            IpcResponse::TopologyRestored {
                restored_from_backup: true,
                departed_displays: 1,
            },
        ),
    ];
    for reply in &replies {
        let encoded = encode_frame(reply).unwrap();
        let decoded: IpcEnvelope<IpcResponse> = decode_frame(&encoded).unwrap();
        assert_eq!(&decoded, reply);
    }
}
