//! Portable tray model, curated quick-add presets, and IPC client library for `zerovdd-tray`.

/// Synchronous named-pipe IPC client over `\\.\pipe\zerovdd-svc`.
pub mod ipc_client;
/// Portable tray menu model and status formatting logic.
pub mod model;
/// Curated quick-add and resolution switch presets.
pub mod presets;
