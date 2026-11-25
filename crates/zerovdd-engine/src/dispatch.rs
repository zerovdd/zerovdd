//! Typed IOCTL dispatcher translating wire buffers into [`VddEngine`] commands.

use zerovdd_proto::ioctl::{
    IOCTL_ADD_DISPLAY, IOCTL_GET_STATUS, IOCTL_GET_VERSION, IOCTL_LIST_DISPLAYS,
    IOCTL_REMOVE_DISPLAY, IOCTL_RENEW_LEASE, IOCTL_SET_RENDER_ADAPTER, IOCTL_UPDATE_DISPLAY_MODE,
};
use zerovdd_proto::types::{
    AddDisplayRequest, AddDisplayResponse, DriverStatusResponse, ListDisplaysResponse,
    ProtocolVersion, RemoveDisplayRequest, RenewLeaseRequest, SetRenderAdapterRequest,
    UpdateDisplayModeRequest, UpdateDisplayModeResponse,
};
use zerovdd_proto::validation::ProtoError;

use crate::lifecycle::VddEngine;
use crate::platform::{AdapterLuid, FileKey, IddPlatform};
use crate::EngineError;

fn ensure_output_len(output_buf: &mut [u8], required: usize) -> Result<&mut [u8], EngineError> {
    if output_buf.len() < required {
        return Err(EngineError::Proto(ProtoError::BufferTooShort {
            expected: required,
            actual: output_buf.len(),
        }));
    }
    Ok(&mut output_buf[..required])
}

/// Decodes, validates, and dispatches a Control Device IOCTL payload.
///
/// Returns the exact number of bytes written into `output_buf`.
pub fn dispatch_ioctl(
    engine: &VddEngine,
    file_key: FileKey,
    ioctl_code: u32,
    input_bytes: &[u8],
    output_buf: &mut [u8],
    platform: &dyn IddPlatform,
) -> Result<usize, EngineError> {
    match ioctl_code {
        IOCTL_GET_VERSION => {
            let out = ensure_output_len(output_buf, ProtocolVersion::WIRE_SIZE)?;
            ProtocolVersion::CURRENT.write_to(out)?;
            Ok(ProtocolVersion::WIRE_SIZE)
        }
        IOCTL_ADD_DISPLAY => {
            let out = ensure_output_len(output_buf, AddDisplayResponse::WIRE_SIZE)?;
            let req = AddDisplayRequest::from_bytes(input_bytes)?;
            let resp = engine.add_display(file_key, &req, platform)?;
            resp.write_to(out)?;
            Ok(AddDisplayResponse::WIRE_SIZE)
        }
        IOCTL_REMOVE_DISPLAY => {
            let req = RemoveDisplayRequest::from_bytes(input_bytes)?;
            engine.remove_display(file_key, &req, platform)?;
            Ok(0)
        }
        IOCTL_LIST_DISPLAYS => {
            let out = ensure_output_len(output_buf, ListDisplaysResponse::WIRE_SIZE)?;
            let resp = engine.list_displays(platform);
            resp.write_to(out)?;
            Ok(ListDisplaysResponse::WIRE_SIZE)
        }
        IOCTL_SET_RENDER_ADAPTER => {
            let req = SetRenderAdapterRequest::from_bytes(input_bytes)?;
            engine.request_gpu_switch(
                AdapterLuid::new(req.adapter_luid_low, req.adapter_luid_high),
                platform,
            )?;
            Ok(0)
        }
        IOCTL_RENEW_LEASE => {
            let req = RenewLeaseRequest::from_bytes(input_bytes)?;
            engine.renew_lease(file_key, &req, platform)?;
            Ok(0)
        }
        IOCTL_GET_STATUS => {
            let out = ensure_output_len(output_buf, DriverStatusResponse::WIRE_SIZE)?;
            let resp = engine.get_status(platform);
            resp.write_to(out)?;
            Ok(DriverStatusResponse::WIRE_SIZE)
        }
        IOCTL_UPDATE_DISPLAY_MODE => {
            let out = ensure_output_len(output_buf, UpdateDisplayModeResponse::WIRE_SIZE)?;
            let req = UpdateDisplayModeRequest::from_bytes(input_bytes)?;
            let resp = engine.update_display_mode(file_key, &req, platform)?;
            resp.write_to(out)?;
            Ok(UpdateDisplayModeResponse::WIRE_SIZE)
        }
        other => Err(EngineError::UnsupportedIoctl(other)),
    }
}
