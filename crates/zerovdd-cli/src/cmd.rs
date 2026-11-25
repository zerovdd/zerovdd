//! Top-level `clap` command definitions and subcommand dispatch for `zerovdd`.

use anyhow::Result;
use clap::{Parser, Subcommand};

/// Virtual display setup, management, and diagnostics command implementations.
pub mod display;
/// System diagnostics and ETW capture command implementation.
pub mod doctor;
/// Custom EDID inspection and validation subcommands.
pub mod edid;
/// Hidden interactive console-session CCD helper invoked by `zerovdd-svc`.
pub mod internal_ccd;
/// Driver package and device-node installation subcommands.
pub mod setup;
/// LizardByte Sunshine streaming session and configuration subcommands.
pub mod sunshine;

/// Command-line interface for managing the `zerovdd` virtual display driver and displays.
#[derive(Debug, Parser)]
#[command(
    name = "zerovdd",
    version,
    about = "Manage zerovdd virtual displays, GPU rendering, and driver setup"
)]
pub struct Cli {
    /// Subcommand to execute.
    #[command(subcommand)]
    pub command: Commands,
}

/// Supported top-level subcommands for `zerovdd`.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Install, upgrade, uninstall, or inspect the zerovdd virtual display driver.
    Setup(setup::SetupArgs),
    /// Add, remove, list, or switch render GPU for virtual displays.
    Display(display::DisplayArgs),
    /// Inspect and validate raw EDID binary files.
    Edid(edid::EdidArgs),
    /// Diagnose driver installation, WUDFHost, GPUs, OS HDR capabilities, and Control Device health.
    Doctor(doctor::DoctorArgs),
    /// Manage Sunshine streaming sessions and configure sunshine.conf integration.
    Sunshine(sunshine::SunshineArgs),
    /// Internal console-session CCD helper invoked by zerovdd-svc.
    #[command(hide = true, name = "internal-ccd")]
    InternalCcd(internal_ccd::InternalCcdArgs),
}

/// Executes the parsed CLI subcommand.
pub fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Setup(args) => setup::execute(args),
        Commands::Display(args) => display::execute(args),
        Commands::Edid(args) => edid::execute(args),
        Commands::Doctor(args) => doctor::execute(args),
        Commands::Sunshine(args) => sunshine::execute(args),
        Commands::InternalCcd(args) => internal_ccd::execute(args),
    }
}
