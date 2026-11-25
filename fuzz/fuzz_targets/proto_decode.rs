//! LibFuzzer target for `zerovdd-proto` binary IOCTL wire codecs and length-prefixed IPC frames.

#![no_main]

use libfuzzer_sys::fuzz_target;
use zerovdd_proto::ipc::{decode_frame, encode_frame, IpcEnvelope, IpcRequest, IpcResponse};
use zerovdd_proto::types::{
    AddDisplayRequest, AddDisplayResponse, DriverStatusResponse, ListDisplaysResponse,
    ProtocolVersion, RemoveDisplayRequest, RenewLeaseRequest, SetRenderAdapterRequest,
    UpdateDisplayModeRequest, UpdateDisplayModeResponse,
};
use zerovdd_proto::validation::Validate;

fuzz_target!(|data: &[u8]| {
    let _ = ProtocolVersion::from_bytes(data).map(|v| v.validate());

    if let Ok(req) = AddDisplayRequest::from_bytes(data) {
        let _ = req.validate();
        let _ = req.client_name_str();
        let _ = req.serial_str();
        let _ = req.custom_edid_slice();
        let _ = req.refresh_mhz();
        let encoded = req.to_bytes();
        assert_eq!(AddDisplayRequest::from_bytes(&encoded), Ok(req));
    }

    if let Ok(resp) = AddDisplayResponse::from_bytes(data) {
        let _ = resp.validate();
        let encoded = resp.to_bytes();
        assert_eq!(AddDisplayResponse::from_bytes(&encoded), Ok(resp));
    }

    if let Ok(req) = RemoveDisplayRequest::from_bytes(data) {
        let _ = req.validate();
        let encoded = req.to_bytes();
        assert_eq!(RemoveDisplayRequest::from_bytes(&encoded), Ok(req));
    }

    if let Ok(req) = SetRenderAdapterRequest::from_bytes(data) {
        let _ = req.validate();
        let encoded = req.to_bytes();
        assert_eq!(SetRenderAdapterRequest::from_bytes(&encoded), Ok(req));
    }

    if let Ok(req) = RenewLeaseRequest::from_bytes(data) {
        let _ = req.validate();
        let encoded = req.to_bytes();
        assert_eq!(RenewLeaseRequest::from_bytes(&encoded), Ok(req));
    }

    if let Ok(resp) = DriverStatusResponse::from_bytes(data) {
        let _ = resp.validate();
        let encoded = resp.to_bytes();
        assert_eq!(DriverStatusResponse::from_bytes(&encoded), Ok(resp));
    }

    if let Ok(resp) = ListDisplaysResponse::from_bytes(data) {
        let _ = resp.validate();
        if let Ok(entries) = resp.entries() {
            for entry in entries {
                let _ = entry.validate();
                let _ = entry.client_name_str();
            }
        }
        let encoded = resp.to_bytes();
        assert_eq!(ListDisplaysResponse::from_bytes(&encoded), Ok(resp));
    }

    if let Ok(req) = UpdateDisplayModeRequest::from_bytes(data) {
        let _ = req.validate();
        let encoded = req.to_bytes();
        assert_eq!(UpdateDisplayModeRequest::from_bytes(&encoded), Ok(req));
    }

    if let Ok(resp) = UpdateDisplayModeResponse::from_bytes(data) {
        let _ = resp.validate();
        let encoded = resp.to_bytes();
        assert_eq!(UpdateDisplayModeResponse::from_bytes(&encoded), Ok(resp));
    }

    if let Ok(req) = decode_frame::<IpcRequest>(data) {
        match &req {
            IpcRequest::StartSession(inner) => {
                let _ = inner.validate();
            }
            IpcRequest::StopSession(inner) => {
                let _ = inner.validate();
            }
            IpcRequest::DisplayAdd(inner) => {
                let _ = inner.validate();
            }
            IpcRequest::DisplayRemove(inner) => {
                let _ = inner.validate();
            }
            IpcRequest::DisplaySetMode(inner) => {
                let _ = inner.validate();
            }
            IpcRequest::SwitchGpu(inner) => {
                let _ = inner.validate();
            }
            IpcRequest::GetSessionStatus
            | IpcRequest::GetHealth
            | IpcRequest::DisplayList
            | IpcRequest::ListGpus
            | IpcRequest::RestoreTopology => {}
        }
        let _ = encode_frame(&req);
    }

    if let Ok(env) = decode_frame::<IpcEnvelope<IpcRequest>>(data) {
        let _ = env.validate_version();
    }

    let _ = decode_frame::<IpcEnvelope<IpcResponse>>(data);
});
