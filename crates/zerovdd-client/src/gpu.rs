//! DXGI GPU adapter enumeration and deterministic GPU selector matching.

use serde::{Deserialize, Serialize};

use crate::error::ClientError;

/// Metadata describing a DXGI graphics adapter available for virtual display rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuAdapterInfo {
    /// Human-readable adapter description (from `DXGI_ADAPTER_DESC1::Description`).
    pub name: String,
    /// Low 32 bits of the adapter Windows `LUID`.
    pub adapter_luid_low: u32,
    /// High 32 bits of the adapter Windows `LUID`.
    pub adapter_luid_high: i32,
    /// PCI vendor identifier (`VendorId`).
    pub vendor_id: u32,
    /// PCI device identifier (`DeviceId`).
    pub device_id: u32,
    /// PCI bus number, when reported by PnP device properties.
    #[serde(default)]
    pub pci_bus: Option<u32>,
    /// PCI device (slot) number, when reported by PnP device properties.
    #[serde(default)]
    pub pci_device: Option<u32>,
    /// PCI function number, when reported by PnP device properties.
    #[serde(default)]
    pub pci_function: Option<u32>,
    /// Dedicated video memory in bytes (`DedicatedVideoMemory`).
    pub dedicated_vram_bytes: u64,
    /// `true` if `DXGI_ADAPTER_FLAG_SOFTWARE` is set (e.g. Microsoft Basic Render Driver / WARP).
    pub is_software: bool,
}

/// Strategy for choosing a target render GPU from enumerated [`GpuAdapterInfo`] entries.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum GpuSelector {
    /// Automatically pick the non-software adapter with the most dedicated VRAM.
    #[default]
    AutoBest,
    /// Select the adapter matching an exact Windows `LUID`.
    Luid {
        /// Low 32 bits of the adapter LUID.
        low: u32,
        /// High 32 bits of the adapter LUID.
        high: i32,
    },
    /// Select a non-software adapter by PCI bus location and optional friendly-name substring.
    PciBusAndName {
        /// PCI bus number.
        bus: u32,
        /// Optional PCI device number.
        device: Option<u32>,
        /// Optional PCI function number.
        function: Option<u32>,
        /// Optional case-insensitive adapter name substring filter.
        name: Option<String>,
    },
    /// Select an adapter by case-insensitive exact or substring friendly-name match.
    FriendlyName {
        /// Adapter description or substring to match.
        name: String,
    },
}

impl From<&zerovdd_config::GpuSelector> for GpuSelector {
    fn from(cfg: &zerovdd_config::GpuSelector) -> Self {
        match cfg {
            zerovdd_config::GpuSelector::AutoBest => Self::AutoBest,
            zerovdd_config::GpuSelector::Luid { low, high } => Self::Luid {
                low: *low,
                high: *high,
            },
            zerovdd_config::GpuSelector::PciBus {
                bus,
                device,
                function,
            } => Self::PciBusAndName {
                bus: *bus,
                device: Some(*device),
                function: Some(*function),
                name: None,
            },
            zerovdd_config::GpuSelector::FriendlyName { name } => {
                Self::FriendlyName { name: name.clone() }
            }
        }
    }
}

impl From<zerovdd_config::GpuSelector> for GpuSelector {
    fn from(cfg: zerovdd_config::GpuSelector) -> Self {
        Self::from(&cfg)
    }
}

/// Selects a GPU matching a `zerovdd_config::GpuSelector` from `adapters`.
pub fn select_gpu_from_config<'a>(
    adapters: &'a [GpuAdapterInfo],
    selector: &zerovdd_config::GpuSelector,
) -> Result<&'a GpuAdapterInfo, ClientError> {
    let mapped = GpuSelector::from(selector);
    select_gpu(adapters, &mapped)
}

/// Selects a GPU matching `selector` from `adapters`, filtering out WARP unless explicitly requested.
pub fn select_gpu<'a>(
    adapters: &'a [GpuAdapterInfo],
    selector: &GpuSelector,
) -> Result<&'a GpuAdapterInfo, ClientError> {
    match selector {
        GpuSelector::AutoBest => adapters
            .iter()
            .enumerate()
            .filter(|(_, a)| !a.is_software)
            .max_by(|(idx_a, a), (idx_b, b)| {
                a.dedicated_vram_bytes
                    .cmp(&b.dedicated_vram_bytes)
                    .then_with(|| idx_b.cmp(idx_a))
            })
            .map(|(_, a)| a)
            .ok_or_else(|| ClientError::GpuNotFound {
                query: "auto_best (non-software)".to_string(),
            }),

        GpuSelector::Luid { low, high } => adapters
            .iter()
            .find(|a| a.adapter_luid_low == *low && a.adapter_luid_high == *high)
            .ok_or_else(|| ClientError::GpuNotFound {
                query: format!("luid={low:#x}:{high:#x}"),
            }),

        GpuSelector::PciBusAndName {
            bus,
            device,
            function,
            name,
        } => {
            let name_needle = name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_ascii_lowercase);

            adapters
                .iter()
                .enumerate()
                .filter(|(_, a)| !a.is_software && a.pci_bus == Some(*bus))
                .filter(|(_, a)| device.is_none_or(|d| a.pci_device == Some(d)))
                .filter(|(_, a)| function.is_none_or(|f| a.pci_function == Some(f)))
                .filter(|(_, a)| {
                    name_needle
                        .as_ref()
                        .is_none_or(|n| a.name.to_ascii_lowercase().contains(n))
                })
                .max_by(|(idx_a, a), (idx_b, b)| {
                    a.dedicated_vram_bytes
                        .cmp(&b.dedicated_vram_bytes)
                        .then_with(|| idx_b.cmp(idx_a))
                })
                .map(|(_, a)| a)
                .ok_or_else(|| ClientError::GpuNotFound {
                    query: format!(
                        "pci_bus={bus}, device={device:?}, function={function:?}, name={name:?}"
                    ),
                })
        }

        GpuSelector::FriendlyName { name } => {
            let needle = name.trim().to_ascii_lowercase();
            if needle.is_empty() {
                return Err(ClientError::InvalidInput(
                    "GPU friendly name selector must not be empty".to_string(),
                ));
            }

            // WHY: Prefer hardware adapters on exact/substring matches first, falling back to
            // software adapters only when explicitly named (e.g. "Microsoft Basic Render Driver").
            if let Some(exact_hw) = adapters
                .iter()
                .find(|a| !a.is_software && a.name.trim().eq_ignore_ascii_case(&needle))
            {
                return Ok(exact_hw);
            }
            if let Some(sub_hw) = adapters
                .iter()
                .enumerate()
                .filter(|(_, a)| !a.is_software && a.name.to_ascii_lowercase().contains(&needle))
                .max_by(|(idx_a, a), (idx_b, b)| {
                    a.dedicated_vram_bytes
                        .cmp(&b.dedicated_vram_bytes)
                        .then_with(|| idx_b.cmp(idx_a))
                })
                .map(|(_, a)| a)
            {
                return Ok(sub_hw);
            }
            if let Some(exact_sw) = adapters
                .iter()
                .find(|a| a.is_software && a.name.trim().eq_ignore_ascii_case(&needle))
            {
                return Ok(exact_sw);
            }
            if let Some(sub_sw) = adapters
                .iter()
                .find(|a| a.is_software && a.name.to_ascii_lowercase().contains(&needle))
            {
                return Ok(sub_sw);
            }

            Err(ClientError::GpuNotFound {
                query: format!("friendly_name={name:?}"),
            })
        }
    }
}

/// Converts a Win32 device interface path (`\\?\PCI#...#{guid}`) into a PnP device instance ID.
pub fn interface_path_to_instance_id(interface_path: &str) -> Option<String> {
    let trimmed = interface_path.trim();
    if trimmed.is_empty() {
        return None;
    }
    let without_prefix = trimmed
        .strip_prefix(r"\\?\")
        .or_else(|| trimmed.strip_prefix(r"\\.\"))
        .unwrap_or(trimmed);

    let without_guid = match without_prefix.rfind("#{") {
        Some(idx) => &without_prefix[..idx],
        None => without_prefix,
    };

    if without_guid.is_empty() {
        return None;
    }
    Some(without_guid.replace('#', r"\"))
}

#[cfg(windows)]
mod win32 {
    use core::ffi::c_void;
    use core::mem::size_of;

    use windows::core::{GUID, PCWSTR};
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_DevNode_Registry_PropertyW, CM_Get_Device_Interface_ListW,
        CM_Get_Device_Interface_List_SizeW, CM_Locate_DevNodeW, CM_DRP_ADDRESS, CM_DRP_BUSNUMBER,
        CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_LOCATE_DEVNODE_NORMAL, CR_BUFFER_SMALL,
        CR_SUCCESS,
    };
    use windows::Win32::Devices::Display::{
        DisplayConfigGetDeviceInfo, DISPLAYCONFIG_ADAPTER_NAME,
        DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    };
    use windows::Win32::Foundation::{ERROR_SUCCESS, LUID};
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
        DXGI_ERROR_NOT_FOUND,
    };
    use windows::Win32::System::Registry::REG_DWORD;

    use super::{interface_path_to_instance_id, GpuAdapterInfo};
    use crate::device::parse_multi_sz_utf16;
    use crate::error::ClientError;

    const GUID_DEVINTERFACE_DISPLAY_ADAPTER: GUID = GUID::from_values(
        0x5B45_201D,
        0xF2F2,
        0x4F3B,
        [0x85, 0xBB, 0x30, 0xFF, 0x1F, 0x95, 0x35, 0x99],
    );

    fn utf16_nul_to_string(buf: &[u16]) -> String {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len])
    }

    fn query_devnode_u32_property(devinst: u32, property: u32) -> Option<u32> {
        let mut reg_type: u32 = 0;
        let mut value: u32 = 0;
        let mut len: u32 = size_of::<u32>() as u32;
        // SAFETY: `value` is a valid stack `u32` and `len` equals `size_of::<u32>()`.
        let cr = unsafe {
            CM_Get_DevNode_Registry_PropertyW(
                devinst,
                property,
                Some(&mut reg_type),
                Some((&raw mut value).cast::<c_void>()),
                &mut len,
                0,
            )
        };
        if cr == CR_SUCCESS && reg_type == REG_DWORD.0 && len == size_of::<u32>() as u32 {
            Some(value)
        } else {
            None
        }
    }

    fn query_pci_from_interface_path(
        interface_path: &str,
    ) -> Option<(Option<u32>, Option<u32>, Option<u32>)> {
        let instance_id = interface_path_to_instance_id(interface_path)?;
        let wide_id: Vec<u16> = instance_id
            .encode_utf16()
            .chain(core::iter::once(0))
            .collect();
        let mut devinst: u32 = 0;
        // SAFETY: `wide_id` is NUL-terminated UTF-16 and `devinst` points to a valid `u32`.
        let cr = unsafe {
            CM_Locate_DevNodeW(
                &mut devinst,
                PCWSTR(wide_id.as_ptr()),
                CM_LOCATE_DEVNODE_NORMAL,
            )
        };
        if cr != CR_SUCCESS {
            return None;
        }
        let bus = query_devnode_u32_property(devinst, CM_DRP_BUSNUMBER);
        let addr = query_devnode_u32_property(devinst, CM_DRP_ADDRESS);
        let device = addr.map(|v| (v >> 16) & 0xFFFF);
        let function = addr.map(|v| v & 0xFFFF);
        if bus.is_some() || device.is_some() || function.is_some() {
            Some((bus, device, function))
        } else {
            None
        }
    }

    fn query_display_adapter_interfaces() -> Vec<String> {
        for _ in 0..4 {
            let mut len: u32 = 0;
            // SAFETY: `len` points to a valid `u32`; `PCWSTR::null()` queries all display adapters.
            let cr = unsafe {
                CM_Get_Device_Interface_List_SizeW(
                    &mut len,
                    &GUID_DEVINTERFACE_DISPLAY_ADAPTER,
                    PCWSTR::null(),
                    CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
                )
            };
            if cr != CR_SUCCESS || len <= 1 {
                return Vec::new();
            }
            let mut buf = vec![0u16; len as usize];
            // SAFETY: `buf` is uniquely owned with capacity `len` UTF-16 code units.
            let cr = unsafe {
                CM_Get_Device_Interface_ListW(
                    &GUID_DEVINTERFACE_DISPLAY_ADAPTER,
                    PCWSTR::null(),
                    &mut buf,
                    CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
                )
            };
            if cr == CR_SUCCESS {
                return parse_multi_sz_utf16(&buf);
            }
            if cr != CR_BUFFER_SMALL {
                return Vec::new();
            }
        }
        Vec::new()
    }

    fn query_pci_location(
        adapter_luid: LUID,
        vendor_id: u32,
        device_id: u32,
        cached_interfaces: &[String],
    ) -> (Option<u32>, Option<u32>, Option<u32>) {
        let mut pkt = DISPLAYCONFIG_ADAPTER_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME,
                size: size_of::<DISPLAYCONFIG_ADAPTER_NAME>() as u32,
                adapterId: adapter_luid,
                id: 0,
            },
            ..Default::default()
        };
        // SAFETY: `pkt` is stack-allocated with exact `header.size`.
        let status = unsafe { DisplayConfigGetDeviceInfo(&mut pkt.header) };
        if status as u32 == ERROR_SUCCESS.0 {
            let path = utf16_nul_to_string(&pkt.adapterDevicePath);
            if let Some(loc) = query_pci_from_interface_path(&path) {
                return loc;
            }
        }

        let needle = format!("ven_{vendor_id:04x}&dev_{device_id:04x}");
        for iface in cached_interfaces {
            if iface.to_ascii_lowercase().contains(&needle) {
                if let Some(loc) = query_pci_from_interface_path(iface) {
                    return loc;
                }
            }
        }

        (None, None, None)
    }

    /// Enumerates all DXGI 1.1 adapters on the local system, preserving software flags and PCI location.
    pub fn enumerate_dxgi_adapters() -> Result<Vec<GpuAdapterInfo>, ClientError> {
        // SAFETY: `CreateDXGIFactory1` creates a COM `IDXGIFactory1` interface with valid IID.
        let factory: IDXGIFactory1 =
            unsafe { CreateDXGIFactory1() }.map_err(|err| ClientError::Win32 {
                operation: "CreateDXGIFactory1",
                code: err.code().0 as u32,
            })?;

        let cached_interfaces = query_display_adapter_interfaces();
        let mut adapters = Vec::new();
        let mut index: u32 = 0;

        loop {
            // SAFETY: `factory` is a valid `IDXGIFactory1` COM object.
            let adapter: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(index) } {
                Ok(adp) => adp,
                Err(err) if err.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(err) => {
                    return Err(ClientError::Win32 {
                        operation: "IDXGIFactory1::EnumAdapters1",
                        code: err.code().0 as u32,
                    });
                }
            };
            index = index.saturating_add(1);

            // SAFETY: `adapter` is a valid `IDXGIAdapter1` COM object.
            let desc = unsafe { adapter.GetDesc1() }.map_err(|err| ClientError::Win32 {
                operation: "IDXGIAdapter1::GetDesc1",
                code: err.code().0 as u32,
            })?;

            let is_software = (desc.Flags & (DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32)) != 0;
            let (pci_bus, pci_device, pci_function) = if is_software {
                (None, None, None)
            } else {
                query_pci_location(
                    desc.AdapterLuid,
                    desc.VendorId,
                    desc.DeviceId,
                    &cached_interfaces,
                )
            };

            adapters.push(GpuAdapterInfo {
                name: utf16_nul_to_string(&desc.Description),
                adapter_luid_low: desc.AdapterLuid.LowPart,
                adapter_luid_high: desc.AdapterLuid.HighPart,
                vendor_id: desc.VendorId,
                device_id: desc.DeviceId,
                pci_bus,
                pci_device,
                pci_function,
                dedicated_vram_bytes: u64::try_from(desc.DedicatedVideoMemory).unwrap_or(u64::MAX),
                is_software,
            });
        }

        Ok(adapters)
    }
}

#[cfg(windows)]
pub use win32::enumerate_dxgi_adapters;

fn parse_u32_radix(s: &str) -> Result<u32, ()> {
    let t = s.trim();
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).map_err(|_| ())
    } else {
        t.parse::<u32>().map_err(|_| ())
    }
}

fn parse_i32_radix(s: &str) -> Result<i32, ()> {
    let t = s.trim();
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16)
            .map(|v| v as i32)
            .map_err(|_| ())
    } else {
        t.parse::<i32>().map_err(|_| ())
    }
}

/// Parses a GPU selector string (`auto`, `luid:<low>:<high>`, `pci:<bus>[:<dev>[.<fn>]]`, or friendly name).
pub fn parse_gpu_selector(raw: &str) -> Result<GpuSelector, ClientError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ClientError::InvalidInput(
            "GPU selector must not be empty".to_string(),
        ));
    }
    if trimmed.eq_ignore_ascii_case("auto")
        || trimmed.eq_ignore_ascii_case("auto-best")
        || trimmed.eq_ignore_ascii_case("auto_best")
    {
        return Ok(GpuSelector::AutoBest);
    }

    if let Some(rest) = trimmed
        .get(..5)
        .filter(|p| p.eq_ignore_ascii_case("luid:"))
        .map(|_| &trimmed[5..])
    {
        let Some((low_str, high_str)) = rest.split_once(':') else {
            return Err(ClientError::InvalidInput(format!(
                "invalid LUID selector `{raw}`: expected luid:<low>:<high>"
            )));
        };
        let low = parse_u32_radix(low_str).map_err(|()| {
            ClientError::InvalidInput(format!("invalid LUID low part `{low_str}`"))
        })?;
        let high = parse_i32_radix(high_str).map_err(|()| {
            ClientError::InvalidInput(format!("invalid LUID high part `{high_str}`"))
        })?;
        return Ok(GpuSelector::Luid { low, high });
    }

    if let Some(rest) = trimmed
        .get(..4)
        .filter(|p| p.eq_ignore_ascii_case("pci:"))
        .map(|_| &trimmed[4..])
    {
        let (bus_str, dev_fn) = match rest.split_once(':') {
            Some((b, df)) => (b, Some(df)),
            None => (rest, None),
        };
        let bus: u32 = bus_str
            .trim()
            .parse()
            .map_err(|_| ClientError::InvalidInput(format!("invalid PCI bus `{bus_str}`")))?;
        let (device, function) = match dev_fn {
            None => (None, None),
            Some(df) => match df.split_once('.') {
                Some((d, f)) => (
                    Some(d.trim().parse().map_err(|_| {
                        ClientError::InvalidInput(format!("invalid PCI device `{d}`"))
                    })?),
                    Some(f.trim().parse().map_err(|_| {
                        ClientError::InvalidInput(format!("invalid PCI function `{f}`"))
                    })?),
                ),
                None => (
                    Some(df.trim().parse().map_err(|_| {
                        ClientError::InvalidInput(format!("invalid PCI device `{df}`"))
                    })?),
                    None,
                ),
            },
        };
        return Ok(GpuSelector::PciBusAndName {
            bus,
            device,
            function,
            name: None,
        });
    }

    Ok(GpuSelector::FriendlyName {
        name: trimmed.to_string(),
    })
}
