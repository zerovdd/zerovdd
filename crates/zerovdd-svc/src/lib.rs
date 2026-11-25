//! Managed Session-0 Windows service, ACL'd named-pipe IPC server, active console-session
//! CCD trampoline, and Sunshine session crash guardian for `zerovdd`.

#![warn(missing_docs)]

/// Session-0 to active console-session CCD launcher (`zerovdd.exe internal-ccd`).
pub mod console_launcher;
/// Persistent and headless-standby virtual display profile reconciler.
pub mod persistent;
/// ACL-protected named pipe server and length-prefixed JSON IPC dispatcher.
pub mod pipe;
/// Windows SCM service lifecycle and `--console` foreground runner.
pub mod service;
/// Sunshine streaming session state machine, overlap preservation, and crash guardian.
pub mod session;
