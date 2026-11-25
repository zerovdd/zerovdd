//! User-mode Windows client for `zerovdd`: Control Device IOCTLs, CCD topology, HDR/ACM, and GPU selection.

#![warn(missing_docs)]

/// Windows CCD topology backend, virtual display resolution, and orchestration.
pub mod ccd;
/// Privileged Control Device discovery, handshake, and typed IOCTL client.
pub mod device;
/// Error types returned by `zerovdd-client`.
pub mod error;
/// DXGI GPU enumeration and adapter selection matching.
pub mod gpu;
/// Windows 11 24H2+ HDR and Auto Color Management (WCG) state management.
pub mod hdr;
/// Portable display topology state model, atomic JSON backup persistence, and planner.
pub mod topology;

pub use error::ClientError;

use uuid::Uuid;

/// Fixed UUIDv5 namespace used to derive deterministic monitor container GUIDs from display names.
pub const MONITOR_GUID_NAMESPACE: Uuid = Uuid::from_bytes([
    0x13, 0xAF, 0x27, 0x75, 0xDD, 0xC4, 0x45, 0x82, 0xBD, 0xA0, 0xF5, 0xD2, 0x7E, 0xAE, 0xC2, 0x5A,
]);

/// Derives a deterministic 16-byte monitor container GUID from a logical display `name`.
pub fn monitor_guid_for_name(name: &str) -> [u8; 16] {
    *Uuid::new_v5(&MONITOR_GUID_NAMESPACE, name.as_bytes()).as_bytes()
}
