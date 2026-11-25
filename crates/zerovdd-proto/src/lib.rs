//! Versioned binary IOCTL ABI and service IPC protocol for `zerovdd`.
//!
//! This crate is `#![no_std]`-compatible by default when built without the `std`
//! feature so the UMDF 2 / IddCx driver can share identical wire types and
//! validation logic with user-mode clients.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

/// Control device paths, interface GUID, and Windows CTL_CODE definitions.
pub mod ioctl;
/// Length-prefixed JSON IPC protocol between `zerovdd-cli` and `zerovdd-svc`.
#[cfg(feature = "std")]
pub mod ipc;
/// Fixed-layout `#[repr(C)]` IOCTL request and response payloads.
pub mod types;
/// Strict validation rules for IOCTL payloads and protocol versions.
pub mod validation;
