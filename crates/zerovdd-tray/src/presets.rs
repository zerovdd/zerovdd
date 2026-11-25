//! Curated quick-add virtual display presets and mode-switch ladders for `zerovdd-tray`.

use zerovdd_proto::ipc::{DisplayAddRequest, DisplayOrientation, TopologyMode};

/// Definition for a single curated quick-add menu entry in `zerovdd-tray`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuickAddPreset {
    /// Human-readable tray menu label.
    pub label: &'static str,
    /// Stable client label used to derive the monitor GUID.
    pub client_id: &'static str,
    /// Active panel width in pixels.
    pub width: u32,
    /// Active panel height in pixels.
    pub height: u32,
    /// Refresh rate in millihertz.
    pub refresh_mhz: u32,
    /// Whether HDR10 should be enabled.
    pub hdr: bool,
    /// Desktop topology mode applied after arrival.
    pub topology_mode: TopologyMode,
    /// Clockwise display orientation.
    pub orientation: DisplayOrientation,
}

impl QuickAddPreset {
    /// Converts this preset into a validated [`DisplayAddRequest`].
    pub fn to_ipc_request(&self) -> DisplayAddRequest {
        DisplayAddRequest {
            client_id: self.client_id.to_string(),
            width: self.width,
            height: self.height,
            refresh_mhz: self.refresh_mhz,
            hdr: self.hdr,
            lease_ttl_secs: None,
            topology_mode: Some(self.topology_mode),
            orientation: (self.orientation != DisplayOrientation::Identity)
                .then_some(self.orientation),
            make_primary: (self.topology_mode == TopologyMode::Isolate).then_some(true),
            ..Default::default()
        }
    }
}

/// Curated quick-add virtual display presets shown in the tray menu.
pub const QUICK_ADD_PRESETS: [QuickAddPreset; 6] = [
    QuickAddPreset {
        label: "1920x1080 @ 60 (Extend)",
        client_id: "tray-1080p60",
        width: 1920,
        height: 1080,
        refresh_mhz: 60_000,
        hdr: false,
        topology_mode: TopologyMode::Extend,
        orientation: DisplayOrientation::Identity,
    },
    QuickAddPreset {
        label: "2560x1440 @ 120 (Extend)",
        client_id: "tray-1440p120",
        width: 2560,
        height: 1440,
        refresh_mhz: 120_000,
        hdr: false,
        topology_mode: TopologyMode::Extend,
        orientation: DisplayOrientation::Identity,
    },
    QuickAddPreset {
        label: "3840x2160 @ 60 HDR (Extend)",
        client_id: "tray-4k60-hdr",
        width: 3840,
        height: 2160,
        refresh_mhz: 60_000,
        hdr: true,
        topology_mode: TopologyMode::Extend,
        orientation: DisplayOrientation::Identity,
    },
    QuickAddPreset {
        label: "3840x2160 @ 120 HDR (Isolate)",
        client_id: "tray-4k120-hdr-iso",
        width: 3840,
        height: 2160,
        refresh_mhz: 120_000,
        hdr: true,
        topology_mode: TopologyMode::Isolate,
        orientation: DisplayOrientation::Identity,
    },
    QuickAddPreset {
        label: "1280x800 @ 90 (Steam Deck, Isolate)",
        client_id: "tray-steam-deck",
        width: 1280,
        height: 800,
        refresh_mhz: 90_000,
        hdr: false,
        topology_mode: TopologyMode::Isolate,
        orientation: DisplayOrientation::Identity,
    },
    QuickAddPreset {
        label: "1080x1920 @ 60 (Portrait, Extend)",
        client_id: "tray-portrait-1080x1920",
        width: 1920,
        height: 1080,
        refresh_mhz: 60_000,
        hdr: false,
        topology_mode: TopologyMode::Extend,
        orientation: DisplayOrientation::Rotate90,
    },
];

/// Quick resolution switch targets exposed per active virtual display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeSwitchPreset {
    /// Menu label shown under `Switch Resolution >`.
    pub label: &'static str,
    /// Target width in pixels.
    pub width: u32,
    /// Target height in pixels.
    pub height: u32,
    /// Target refresh rate in millihertz.
    pub refresh_mhz: u32,
}

/// Curated quick resolution options shown per active display.
pub const MODE_SWITCH_PRESETS: [ModeSwitchPreset; 4] = [
    ModeSwitchPreset {
        label: "1080p @ 60 Hz",
        width: 1920,
        height: 1080,
        refresh_mhz: 60_000,
    },
    ModeSwitchPreset {
        label: "1440p @ 120 Hz",
        width: 2560,
        height: 1440,
        refresh_mhz: 120_000,
    },
    ModeSwitchPreset {
        label: "4K @ 60 Hz",
        width: 3840,
        height: 2160,
        refresh_mhz: 60_000,
    },
    ModeSwitchPreset {
        label: "4K @ 120 Hz",
        width: 3840,
        height: 2160,
        refresh_mhz: 120_000,
    },
];
