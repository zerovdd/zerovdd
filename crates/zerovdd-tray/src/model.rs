//! Portable menu model and state formatting for `zerovdd-tray`.

use zerovdd_proto::ipc::{ActiveDisplayInfo, GpuAdapterSummary, ServiceHealth, SessionStatus};

use crate::presets::{MODE_SWITCH_PRESETS, QUICK_ADD_PRESETS};

/// Live state snapshot queried lazily from `zerovdd-svc` before rendering the tray menu.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TraySnapshot {
    /// Whether `zerovdd-svc` responded on the named pipe.
    pub service_online: bool,
    /// Optional service health summary.
    pub health: Option<ServiceHealth>,
    /// Optional active Sunshine streaming session snapshot.
    pub session: Option<SessionStatus>,
    /// Active virtual displays returned by `DisplayList`.
    pub displays: Vec<ActiveDisplayInfo>,
    /// Enumerated DXGI GPUs returned by `ListGpus`.
    pub gpus: Vec<GpuAdapterSummary>,
    /// Currently configured GPU selector summary from `config.toml`.
    pub configured_gpu_selector: String,
    /// Whether the most recent asynchronous GPU switch reported failure.
    pub last_gpu_switch_failed: bool,
}

/// Logical action associated with a clickable tray menu item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    /// Quick-add a preset virtual display (`index` into `QUICK_ADD_PRESETS`).
    QuickAddPreset(usize),
    /// Remove an active virtual display by VidPN `target_id`.
    RemoveDisplay {
        /// OS VidPN target ID.
        target_id: u32,
    },
    /// Toggle HDR10 state on an active virtual display in place.
    ToggleDisplayHdr {
        /// OS VidPN target ID.
        target_id: u32,
        /// Current display width in pixels.
        width: u32,
        /// Current display height in pixels.
        height: u32,
        /// Current refresh rate in millihertz.
        refresh_mhz: u32,
        /// Desired new HDR state.
        enable_hdr: bool,
    },
    /// Switch resolution/refresh on an active virtual display in place.
    SwitchDisplayMode {
        /// OS VidPN target ID.
        target_id: u32,
        /// Target width in pixels.
        width: u32,
        /// Target height in pixels.
        height: u32,
        /// Target refresh rate in millihertz.
        refresh_mhz: u32,
    },
    /// Switch the render GPU adapter (`auto` or `luid:<low>:<high>`).
    SwitchGpu {
        /// GPU selector string understood by `zerovdd-svc`.
        selector: String,
    },
    /// Stop the active Sunshine streaming session (`force = false`).
    StopSunshineSession,
    /// Launch elevated `zerovdd.exe sunshine configure --apply --clear-output-name`.
    ConfigureSunshine,
    /// Restore physical display topology (`IpcRequest::RestoreTopology`).
    RestorePhysicalDisplays,
    /// Open `%ProgramData%\zerovdd\config.toml` (or XDG equivalent) in default editor.
    OpenConfigToml,
    /// Copy `zerovdd doctor --json` output to the Windows clipboard.
    CopyDoctorJson,
    /// Toggle `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\ZeroVDD` autostart.
    ToggleAutostart,
    /// Cleanly exit the tray companion (`NIM_DELETE`, leaving `zerovdd-svc` running).
    Exit,
}

/// Tree node representing one Win32 popup menu entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuNode {
    /// Horizontal menu separator line.
    Separator,
    /// Clickable or disabled leaf menu item.
    Item {
        /// Display label.
        label: String,
        /// Whether the item can be clicked.
        enabled: bool,
        /// Whether a checkmark/radio bullet is shown.
        checked: bool,
        /// Optional action executed when clicked.
        action: Option<MenuAction>,
    },
    /// Nested popup submenu.
    Submenu {
        /// Submenu label.
        label: String,
        /// Whether the submenu is enabled.
        enabled: bool,
        /// Child menu nodes.
        children: Vec<MenuNode>,
    },
}

/// Formats the disabled top status header line from the latest [`TraySnapshot`].
pub fn format_status_header(snapshot: &TraySnapshot) -> String {
    if !snapshot.service_online {
        return "ZeroVDD -- Service Offline".to_string();
    }
    if let Some(ref sess) = snapshot.session {
        if sess.active {
            let client = sess.client_id.as_deref().unwrap_or("sunshine");
            return format!("ZeroVDD -- Sunshine Active ({client})");
        }
    }
    let count = snapshot.displays.len();
    if count == 0 {
        "ZeroVDD -- Idle (0 virtual displays)".to_string()
    } else if count == 1 {
        "ZeroVDD -- 1 Virtual Display".to_string()
    } else {
        format!("ZeroVDD -- {count} Virtual Displays")
    }
}

/// Formats a refresh rate from `(refresh_num, refresh_den)` cleanly (for example `59.94Hz` or `120Hz`).
pub fn format_refresh_label(num: u32, den: u32) -> String {
    if den == 0 {
        return "0Hz".to_string();
    }
    if den == 1 {
        return format!("{num}Hz");
    }
    let mhz = (u64::from(num) * 1000 + u64::from(den) / 2) / u64::from(den);
    if mhz.is_multiple_of(1000) {
        format!("{}Hz", mhz / 1000)
    } else {
        format!("{:.2}Hz", (mhz as f64) / 1000.0)
    }
}

/// Converts `(refresh_num, refresh_den)` into rounded millihertz.
pub fn rational_to_millihz(num: u32, den: u32) -> u32 {
    if den == 0 {
        return 60_000;
    }
    let mhz = (u64::from(num) * 1000 + u64::from(den) / 2) / u64::from(den);
    u32::try_from(mhz).unwrap_or(60_000).max(1)
}

/// Builds the complete tray menu node tree from `snapshot` and `autostart_enabled`.
pub fn build_tray_menu(snapshot: &TraySnapshot, autostart_enabled: bool) -> Vec<MenuNode> {
    let mut root = Vec::new();
    root.push(MenuNode::Item {
        label: format_status_header(snapshot),
        enabled: false,
        checked: false,
        action: None,
    });
    root.push(MenuNode::Separator);

    let quick_children: Vec<MenuNode> = QUICK_ADD_PRESETS
        .iter()
        .enumerate()
        .map(|(idx, preset)| MenuNode::Item {
            label: preset.label.to_string(),
            enabled: snapshot.service_online,
            checked: false,
            action: Some(MenuAction::QuickAddPreset(idx)),
        })
        .collect();
    root.push(MenuNode::Submenu {
        label: "Quick Add Display".to_string(),
        enabled: snapshot.service_online,
        children: quick_children,
    });

    let active_children: Vec<MenuNode> = if snapshot.displays.is_empty() {
        vec![MenuNode::Item {
            label: "(No active virtual displays)".to_string(),
            enabled: false,
            checked: false,
            action: None,
        }]
    } else {
        snapshot
            .displays
            .iter()
            .map(|disp| {
                let refresh_mhz = rational_to_millihz(disp.refresh_num, disp.refresh_den);
                let mode_children: Vec<MenuNode> = MODE_SWITCH_PRESETS
                    .iter()
                    .map(|preset| MenuNode::Item {
                        label: preset.label.to_string(),
                        enabled: true,
                        checked: disp.width == preset.width
                            && disp.height == preset.height
                            && refresh_mhz.abs_diff(preset.refresh_mhz) <= 50,
                        action: Some(MenuAction::SwitchDisplayMode {
                            target_id: disp.target_id,
                            width: preset.width,
                            height: preset.height,
                            refresh_mhz: preset.refresh_mhz,
                        }),
                    })
                    .collect();
                let hdr_tag = if disp.hdr_active { " HDR" } else { "" };
                let label = format!(
                    "{} ({}x{} @ {}{})",
                    disp.client_name,
                    disp.width,
                    disp.height,
                    format_refresh_label(disp.refresh_num, disp.refresh_den),
                    hdr_tag
                );
                MenuNode::Submenu {
                    label,
                    enabled: true,
                    children: vec![
                        MenuNode::Item {
                            label: "Remove Display".to_string(),
                            enabled: true,
                            checked: false,
                            action: Some(MenuAction::RemoveDisplay {
                                target_id: disp.target_id,
                            }),
                        },
                        MenuNode::Item {
                            label: if disp.hdr_active {
                                "Toggle HDR (On -> Off)".to_string()
                            } else {
                                "Toggle HDR (Off -> On)".to_string()
                            },
                            enabled: true,
                            checked: disp.hdr_active,
                            action: Some(MenuAction::ToggleDisplayHdr {
                                target_id: disp.target_id,
                                width: disp.width,
                                height: disp.height,
                                refresh_mhz,
                                enable_hdr: !disp.hdr_active,
                            }),
                        },
                        MenuNode::Submenu {
                            label: "Switch Resolution".to_string(),
                            enabled: true,
                            children: mode_children,
                        },
                    ],
                }
            })
            .collect()
    };
    root.push(MenuNode::Submenu {
        label: format!("Active Displays ({})", snapshot.displays.len()),
        enabled: snapshot.service_online,
        children: active_children,
    });

    let is_auto_selected = snapshot
        .configured_gpu_selector
        .eq_ignore_ascii_case("auto");
    let mut gpu_children = vec![MenuNode::Item {
        label: "Auto (Best Dedicated GPU)".to_string(),
        enabled: snapshot.service_online,
        checked: is_auto_selected,
        action: Some(MenuAction::SwitchGpu {
            selector: "auto".to_string(),
        }),
    }];
    if !snapshot.gpus.is_empty() {
        gpu_children.push(MenuNode::Separator);
        for gpu in &snapshot.gpus {
            let luid_selector =
                format!("luid:{:#x}:{}", gpu.adapter_luid_low, gpu.adapter_luid_high);
            let matched_explicit = !is_auto_selected
                && (snapshot
                    .configured_gpu_selector
                    .eq_ignore_ascii_case(&luid_selector)
                    || snapshot
                        .configured_gpu_selector
                        .eq_ignore_ascii_case(&gpu.friendly_name));
            let vram_gb = gpu.dedicated_vram_bytes / (1024 * 1024 * 1024);
            let suffix = if gpu.is_software {
                " [Software]".to_string()
            } else if gpu.is_best {
                format!(" [{vram_gb} GiB, Best]")
            } else {
                format!(" [{vram_gb} GiB]")
            };
            gpu_children.push(MenuNode::Item {
                label: format!("{}{suffix}", gpu.friendly_name),
                enabled: snapshot.service_online,
                checked: matched_explicit,
                action: Some(MenuAction::SwitchGpu {
                    selector: luid_selector,
                }),
            });
        }
    }
    root.push(MenuNode::Submenu {
        label: "Render GPU".to_string(),
        enabled: snapshot.service_online,
        children: gpu_children,
    });

    let session_active = snapshot.session.as_ref().is_some_and(|s| s.active);
    let sunshine_status_line = if let Some(ref sess) = snapshot.session {
        if sess.active {
            format!(
                "Status: Active ({} {}x{} @ {})",
                sess.client_id.as_deref().unwrap_or("client"),
                sess.width,
                sess.height,
                format_refresh_label(sess.refresh_mhz, 1000)
            )
        } else {
            "Status: Idle".to_string()
        }
    } else if snapshot.service_online {
        "Status: Idle".to_string()
    } else {
        "Status: Service Offline".to_string()
    };
    let mut sunshine_children = vec![MenuNode::Item {
        label: sunshine_status_line,
        enabled: false,
        checked: false,
        action: None,
    }];
    if session_active {
        sunshine_children.push(MenuNode::Item {
            label: "Stop Active Session".to_string(),
            enabled: snapshot.service_online,
            checked: false,
            action: Some(MenuAction::StopSunshineSession),
        });
    }
    sunshine_children.push(MenuNode::Item {
        label: "Configure Sunshine...".to_string(),
        enabled: true,
        checked: false,
        action: Some(MenuAction::ConfigureSunshine),
    });
    root.push(MenuNode::Submenu {
        label: "Sunshine".to_string(),
        enabled: true,
        children: sunshine_children,
    });

    root.push(MenuNode::Item {
        label: "Restore Physical Displays".to_string(),
        enabled: snapshot.service_online,
        checked: false,
        action: Some(MenuAction::RestorePhysicalDisplays),
    });
    root.push(MenuNode::Separator);
    root.push(MenuNode::Item {
        label: "Open config.toml".to_string(),
        enabled: true,
        checked: false,
        action: Some(MenuAction::OpenConfigToml),
    });
    root.push(MenuNode::Item {
        label: "Copy Doctor Report (JSON)".to_string(),
        enabled: true,
        checked: false,
        action: Some(MenuAction::CopyDoctorJson),
    });
    root.push(MenuNode::Item {
        label: "Start with Windows".to_string(),
        enabled: true,
        checked: autostart_enabled,
        action: Some(MenuAction::ToggleAutostart),
    });
    root.push(MenuNode::Separator);
    root.push(MenuNode::Item {
        label: "Exit".to_string(),
        enabled: true,
        checked: false,
        action: Some(MenuAction::Exit),
    });

    root
}
