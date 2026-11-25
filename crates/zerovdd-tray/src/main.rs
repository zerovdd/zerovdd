//! Native Win32 system tray companion entrypoint (`zerovdd-tray.exe`).

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod win32_tray;

use anyhow::Result;
use clap::Parser;
use serde::Serialize;

use zerovdd_tray::ipc_client;
use zerovdd_tray::model::{build_tray_menu, format_status_header};

/// Command-line flags for `zerovdd-tray`.
#[derive(Debug, Parser)]
#[command(
    name = "zerovdd-tray",
    version,
    about = "Native Win32 system tray companion for ZeroVDD"
)]
struct TrayArgs {
    /// Query zerovdd-svc IPC, build the menu model, print a JSON summary, and exit cleanly.
    #[arg(long)]
    check_only: bool,
    /// Enable verbose diagnostic logging.
    #[arg(long)]
    debug: bool,
}

#[derive(Debug, Serialize)]
struct CheckOnlySummary {
    service_online: bool,
    status_header: String,
    active_displays: usize,
    gpu_count: usize,
    menu_item_count: usize,
}

fn main() -> Result<()> {
    let args = TrayArgs::parse();

    if args.check_only {
        let snapshot = ipc_client::fetch_snapshot();
        let menu = build_tray_menu(&snapshot, false);
        let summary = CheckOnlySummary {
            service_online: snapshot.service_online,
            status_header: format_status_header(&snapshot),
            active_displays: snapshot.displays.len(),
            gpu_count: snapshot.gpus.len(),
            menu_item_count: menu.len(),
        };
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }

    #[cfg(windows)]
    {
        win32_tray::run_tray_loop(args.debug)
    }

    #[cfg(not(windows))]
    {
        let _ = args.debug;
        anyhow::bail!("zerovdd-tray GUI loop is only supported on Windows")
    }
}
