//! Re-entrancy-safe monitor slot registry and specification store.

use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

use zerovdd_modes::{PlannedMode, RationalHz};
use zerovdd_proto::types::{
    AddDisplayResponse, BitDepthPreference, ColorSpacePreference, DisplayInfo,
    HdrMasteringMetadata, HdrProfile, OwnershipMode, MAX_CLIENT_NAME_LEN, MAX_LIST_DISPLAYS,
};

use crate::platform::{AdapterLuid, ConnectorSlot, FileKey, MonitorHandle, MAX_MONITORS};

/// Immutable specification for a virtual monitor shared across PnP callbacks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorSpec {
    /// Stable monitor container GUID.
    pub monitor_guid: [u8; 16],
    /// Client name buffer bounded by `client_name_len`.
    pub client_name: [u8; MAX_CLIENT_NAME_LEN],
    /// Valid byte length of `client_name`.
    pub client_name_len: u8,
    /// Preferred horizontal resolution in pixels.
    pub width: u32,
    /// Preferred vertical resolution in lines.
    pub height: u32,
    /// Preferred refresh rate numerator.
    pub refresh_num: u32,
    /// Preferred refresh rate denominator.
    pub refresh_den: u32,
    /// Configured dynamic range profile.
    pub hdr_profile: HdrProfile,
    /// Configured color space preference.
    pub color_space: ColorSpacePreference,
    /// Configured bit-depth preference.
    pub bit_depth: BitDepthPreference,
    /// Configured pixel density (`0` = default 100 PPI).
    pub target_ppi: u16,
    /// Normalized HDR10 mastering luminance metadata.
    pub hdr_metadata: HdrMasteringMetadata,
    /// Monitor lifetime ownership model.
    pub ownership_mode: OwnershipMode,
    /// Validated or synthesized EDID binary payload.
    pub edid: Vec<u8>,
    /// Precomputed CVT-RBv2 mode list with preferred mode at index 0.
    pub planned_modes: Vec<PlannedMode>,
}

/// Key used by synchronous IddCx callbacks to look up a monitor specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorQueryKey<'a> {
    /// Look up by zero-based virtual connector slot.
    ConnectorSlot(ConnectorSlot),
    /// Look up by `IDDCX_MONITOR` handle token.
    MonitorHandle(MonitorHandle),
    /// Look up by monitor container GUID.
    MonitorGuid([u8; 16]),
    /// Look up by exact EDID byte payload.
    Edid(&'a [u8]),
}

#[derive(Debug, Clone)]
pub(crate) enum SlotPhase {
    Pending {
        monitor_handle: Option<MonitorHandle>,
        cancel_requested: bool,
    },
    Active {
        monitor_handle: MonitorHandle,
        adapter_luid: AdapterLuid,
        target_id: u32,
        lease_expires_ms: Option<u64>,
    },
    Updating {
        monitor_handle: MonitorHandle,
        adapter_luid: AdapterLuid,
        target_id: u32,
        lease_expires_ms: Option<u64>,
        depart_requested: bool,
    },
    Departing {
        monitor_handle: MonitorHandle,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct SlotEntry {
    pub(crate) instance_id: u64,
    pub(crate) slot: ConnectorSlot,
    pub(crate) owner: FileKey,
    pub(crate) spec: Arc<MonitorSpec>,
    pub(crate) arrival_modes: Vec<(u32, u32, RationalHz)>,
    pub(crate) arrival_hdr_capable: bool,
    pub(crate) phase: SlotPhase,
}

impl SlotEntry {
    fn monitor_handle(&self) -> Option<MonitorHandle> {
        match self.phase {
            SlotPhase::Pending { monitor_handle, .. } => monitor_handle,
            SlotPhase::Active { monitor_handle, .. }
            | SlotPhase::Updating { monitor_handle, .. }
            | SlotPhase::Departing { monitor_handle } => Some(monitor_handle),
        }
    }

    fn matches_query(&self, key: MonitorQueryKey<'_>) -> bool {
        if matches!(self.phase, SlotPhase::Departing { .. }) {
            return false;
        }
        match key {
            MonitorQueryKey::ConnectorSlot(slot) => self.slot == slot,
            MonitorQueryKey::MonitorHandle(handle) => self.monitor_handle() == Some(handle),
            MonitorQueryKey::MonitorGuid(guid) => self.spec.monitor_guid == guid,
            MonitorQueryKey::Edid(edid) => self.spec.edid.as_slice() == edid,
        }
    }
}

pub(crate) enum LookupExistingResult {
    NotPresent,
    Existing(AddDisplayResponse),
    Conflict,
    WaitInFlight,
    ExpiredToDepart {
        slot: ConnectorSlot,
        instance_id: u64,
        handle: MonitorHandle,
    },
}

pub(crate) enum ReservePendingResult {
    Reserved {
        slot: ConnectorSlot,
        instance_id: u64,
    },
    Existing(AddDisplayResponse),
    Conflict,
    WaitInFlight,
    ExpiredToDepart {
        slot: ConnectorSlot,
        instance_id: u64,
        handle: MonitorHandle,
    },
    NoFreeSlots,
}

pub(crate) enum BeginRemoveResult {
    NotFound,
    WaitPending {
        slot: ConnectorSlot,
        instance_id: u64,
    },
    WaitUpdating {
        slot: ConnectorSlot,
        instance_id: u64,
    },
    WaitDeparting {
        slot: ConnectorSlot,
        instance_id: u64,
    },
    Depart {
        slot: ConnectorSlot,
        instance_id: u64,
        handle: MonitorHandle,
    },
}

pub(crate) enum RenewLeaseOutcome {
    Renewed,
    NotFound,
    InFlight,
    NotLeased,
    Expired,
}

pub(crate) struct DepartTarget {
    pub(crate) slot: ConnectorSlot,
    pub(crate) instance_id: u64,
    pub(crate) handle: MonitorHandle,
}

pub(crate) struct CleanupBatch {
    pub(crate) to_depart: Vec<DepartTarget>,
    pub(crate) wait_instances: Vec<(ConnectorSlot, u64)>,
}

pub(crate) struct RegistryStats {
    pub(crate) total_arrivals: u64,
    pub(crate) total_departures: u64,
    pub(crate) active_display_count: u32,
}

pub(crate) enum LookupUpdateResult {
    NotFound,
    InFlight,
    Active {
        slot: ConnectorSlot,
        instance_id: u64,
        spec: Arc<MonitorSpec>,
    },
}

pub(crate) enum BeginUpdateResult {
    NotFound,
    InFlight,
    Started {
        handle: MonitorHandle,
        adapter_luid: AdapterLuid,
        target_id: u32,
        old_spec: Arc<MonitorSpec>,
        in_place_compatible: bool,
    },
}

struct RegistryTable {
    slots: [Option<SlotEntry>; MAX_MONITORS],
    next_instance_id: u64,
    total_arrivals: u64,
    total_departures: u64,
}

impl RegistryTable {
    const fn new() -> Self {
        const EMPTY: Option<SlotEntry> = None;
        Self {
            slots: [EMPTY; MAX_MONITORS],
            next_instance_id: 1,
            total_arrivals: 0,
            total_departures: 0,
        }
    }

    fn find_by_guid_mut(&mut self, guid: [u8; 16]) -> Option<&mut SlotEntry> {
        self.slots
            .iter_mut()
            .flatten()
            .find(|entry| entry.spec.monitor_guid == guid)
    }

    fn inspect_existing_by_guid(
        &mut self,
        file_key: FileKey,
        guid: [u8; 16],
        now_ms: u64,
    ) -> LookupExistingResult {
        let Some(entry) = self.find_by_guid_mut(guid) else {
            return LookupExistingResult::NotPresent;
        };

        let is_owner_compatible = match entry.spec.ownership_mode {
            OwnershipMode::HandleBound => entry.owner == file_key,
            OwnershipMode::Leased => true,
        };

        match entry.phase {
            SlotPhase::Pending { .. } | SlotPhase::Updating { .. } => {
                if !is_owner_compatible {
                    LookupExistingResult::Conflict
                } else {
                    LookupExistingResult::WaitInFlight
                }
            }
            SlotPhase::Departing { .. } => LookupExistingResult::WaitInFlight,
            SlotPhase::Active {
                monitor_handle,
                adapter_luid,
                target_id,
                lease_expires_ms,
            } => {
                if let Some(expires_ms) = lease_expires_ms {
                    if now_ms >= expires_ms {
                        let slot = entry.slot;
                        let instance_id = entry.instance_id;
                        entry.phase = SlotPhase::Departing { monitor_handle };
                        return LookupExistingResult::ExpiredToDepart {
                            slot,
                            instance_id,
                            handle: monitor_handle,
                        };
                    }
                }
                if !is_owner_compatible {
                    return LookupExistingResult::Conflict;
                }
                LookupExistingResult::Existing(AddDisplayResponse {
                    adapter_luid_low: adapter_luid.low,
                    adapter_luid_high: adapter_luid.high,
                    target_id,
                    connector_index: entry.slot.0,
                    created_new: 0,
                    reserved: [0; 3],
                })
            }
        }
    }
}

/// Thread-safe, lock-brief registry of virtual monitor slots.
///
/// The internal `spin::Mutex` is never held across any [`crate::platform::IddPlatform`]
/// call so synchronous re-entrant PnP callbacks cannot deadlock.
pub struct MonitorRegistry {
    table: Mutex<RegistryTable>,
}

impl MonitorRegistry {
    /// Creates an empty [`MonitorRegistry`] with [`MAX_MONITORS`] free slots.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            table: Mutex::new(RegistryTable::new()),
        }
    }

    pub(crate) fn check_existing(
        &self,
        file_key: FileKey,
        guid: [u8; 16],
        now_ms: u64,
    ) -> LookupExistingResult {
        let mut guard = self.table.lock();
        guard.inspect_existing_by_guid(file_key, guid, now_ms)
    }

    pub(crate) fn reserve_pending(
        &self,
        file_key: FileKey,
        spec: Arc<MonitorSpec>,
        now_ms: u64,
    ) -> ReservePendingResult {
        let mut guard = self.table.lock();
        match guard.inspect_existing_by_guid(file_key, spec.monitor_guid, now_ms) {
            LookupExistingResult::NotPresent => {}
            LookupExistingResult::Existing(resp) => return ReservePendingResult::Existing(resp),
            LookupExistingResult::Conflict => return ReservePendingResult::Conflict,
            LookupExistingResult::WaitInFlight => return ReservePendingResult::WaitInFlight,
            LookupExistingResult::ExpiredToDepart {
                slot,
                instance_id,
                handle,
            } => {
                return ReservePendingResult::ExpiredToDepart {
                    slot,
                    instance_id,
                    handle,
                };
            }
        }

        let Some(slot_idx) = guard.slots.iter().position(Option::is_none) else {
            return ReservePendingResult::NoFreeSlots;
        };

        let instance_id = guard.next_instance_id;
        guard.next_instance_id = guard.next_instance_id.saturating_add(1);
        // WHY: slot_idx < MAX_MONITORS (16), so truncation to u32 is impossible.
        let slot = ConnectorSlot(slot_idx as u32);
        let arrival_modes = spec
            .planned_modes
            .iter()
            .map(|m| (m.width, m.height, m.refresh))
            .collect();
        let arrival_hdr_capable = spec.hdr_profile == HdrProfile::Hdr10;
        guard.slots[slot_idx] = Some(SlotEntry {
            instance_id,
            slot,
            owner: file_key,
            spec,
            arrival_modes,
            arrival_hdr_capable,
            phase: SlotPhase::Pending {
                monitor_handle: None,
                cancel_requested: false,
            },
        });

        ReservePendingResult::Reserved { slot, instance_id }
    }

    pub(crate) fn attach_handle_or_cancel(
        &self,
        slot: ConnectorSlot,
        instance_id: u64,
        handle: MonitorHandle,
    ) -> bool {
        let mut guard = self.table.lock();
        let Some(entry) = guard
            .slots
            .get_mut(slot.0 as usize)
            .and_then(Option::as_mut)
        else {
            return false;
        };
        if entry.instance_id != instance_id {
            return false;
        }
        match &mut entry.phase {
            SlotPhase::Pending {
                monitor_handle,
                cancel_requested,
            } => {
                if *cancel_requested {
                    entry.phase = SlotPhase::Departing {
                        monitor_handle: handle,
                    };
                    false
                } else {
                    *monitor_handle = Some(handle);
                    true
                }
            }
            _ => false,
        }
    }

    pub(crate) fn promote_active(
        &self,
        slot: ConnectorSlot,
        instance_id: u64,
        handle: MonitorHandle,
        adapter_luid: AdapterLuid,
        target_id: u32,
        lease_expires_ms: Option<u64>,
    ) -> bool {
        let mut guard = self.table.lock();
        let Some(entry) = guard
            .slots
            .get_mut(slot.0 as usize)
            .and_then(Option::as_mut)
        else {
            return false;
        };
        if entry.instance_id != instance_id {
            return false;
        }
        match entry.phase {
            SlotPhase::Pending {
                cancel_requested, ..
            } => {
                if cancel_requested {
                    entry.phase = SlotPhase::Departing {
                        monitor_handle: handle,
                    };
                    false
                } else {
                    entry.phase = SlotPhase::Active {
                        monitor_handle: handle,
                        adapter_luid,
                        target_id,
                        lease_expires_ms,
                    };
                    guard.total_arrivals = guard.total_arrivals.saturating_add(1);
                    true
                }
            }
            _ => false,
        }
    }

    pub(crate) fn mark_departing(
        &self,
        slot: ConnectorSlot,
        instance_id: u64,
        handle: MonitorHandle,
    ) {
        let mut guard = self.table.lock();
        if let Some(entry) = guard
            .slots
            .get_mut(slot.0 as usize)
            .and_then(Option::as_mut)
        {
            if entry.instance_id == instance_id {
                entry.phase = SlotPhase::Departing {
                    monitor_handle: handle,
                };
            }
        }
    }

    pub(crate) fn remove_slot(&self, slot: ConnectorSlot, instance_id: u64, count_departure: bool) {
        let mut guard = self.table.lock();
        if let Some(slot_ref) = guard.slots.get_mut(slot.0 as usize) {
            if slot_ref
                .as_ref()
                .is_some_and(|entry| entry.instance_id == instance_id)
            {
                *slot_ref = None;
                if count_departure {
                    guard.total_departures = guard.total_departures.saturating_add(1);
                }
            }
        }
    }

    pub(crate) fn is_instance_present(&self, slot: ConnectorSlot, instance_id: u64) -> bool {
        let guard = self.table.lock();
        guard
            .slots
            .get(slot.0 as usize)
            .and_then(Option::as_ref)
            .is_some_and(|entry| entry.instance_id == instance_id)
    }

    pub(crate) fn is_instance_updating(&self, slot: ConnectorSlot, instance_id: u64) -> bool {
        let guard = self.table.lock();
        guard
            .slots
            .get(slot.0 as usize)
            .and_then(Option::as_ref)
            .is_some_and(|entry| {
                entry.instance_id == instance_id
                    && matches!(entry.phase, SlotPhase::Updating { .. })
            })
    }

    pub(crate) fn clone_spec(&self, key: MonitorQueryKey<'_>) -> Option<Arc<MonitorSpec>> {
        let guard = self.table.lock();
        guard
            .slots
            .iter()
            .flatten()
            .find(|entry| entry.matches_query(key))
            .map(|entry| Arc::clone(&entry.spec))
    }

    pub(crate) fn begin_remove_by_guid(&self, guid: [u8; 16]) -> BeginRemoveResult {
        let mut guard = self.table.lock();
        let Some(entry) = guard
            .slots
            .iter_mut()
            .flatten()
            .find(|entry| entry.spec.monitor_guid == guid)
        else {
            return BeginRemoveResult::NotFound;
        };
        Self::begin_remove_entry(entry)
    }

    pub(crate) fn begin_remove_by_target_id(&self, target_id: u32) -> BeginRemoveResult {
        let mut guard = self.table.lock();
        let Some(entry) = guard.slots.iter_mut().flatten().find(|entry| {
            matches!(
                entry.phase,
                SlotPhase::Active {
                    target_id: active_target,
                    ..
                } | SlotPhase::Updating {
                    target_id: active_target,
                    ..
                } if active_target == target_id
            )
        }) else {
            return BeginRemoveResult::NotFound;
        };
        Self::begin_remove_entry(entry)
    }

    pub(crate) fn lookup_active_by_guid(&self, guid: [u8; 16]) -> LookupUpdateResult {
        let guard = self.table.lock();
        let Some(entry) = guard
            .slots
            .iter()
            .flatten()
            .find(|entry| entry.spec.monitor_guid == guid)
        else {
            return LookupUpdateResult::NotFound;
        };
        Self::classify_update_entry(entry)
    }

    pub(crate) fn lookup_active_by_target_id(&self, target_id: u32) -> LookupUpdateResult {
        let guard = self.table.lock();
        let Some(entry) = guard.slots.iter().flatten().find(|entry| {
            matches!(
                entry.phase,
                SlotPhase::Active {
                    target_id: active_target,
                    ..
                } | SlotPhase::Updating {
                    target_id: active_target,
                    ..
                } if active_target == target_id
            )
        }) else {
            return LookupUpdateResult::NotFound;
        };
        Self::classify_update_entry(entry)
    }

    fn classify_update_entry(entry: &SlotEntry) -> LookupUpdateResult {
        match entry.phase {
            SlotPhase::Pending { .. } | SlotPhase::Updating { .. } => LookupUpdateResult::InFlight,
            SlotPhase::Departing { .. } => LookupUpdateResult::NotFound,
            SlotPhase::Active { .. } => LookupUpdateResult::Active {
                slot: entry.slot,
                instance_id: entry.instance_id,
                spec: Arc::clone(&entry.spec),
            },
        }
    }

    pub(crate) fn begin_update_spec(
        &self,
        slot: ConnectorSlot,
        instance_id: u64,
        mut new_spec: Arc<MonitorSpec>,
        fresh_edid: Vec<u8>,
    ) -> BeginUpdateResult {
        let mut guard = self.table.lock();
        let Some(entry) = guard
            .slots
            .get_mut(slot.0 as usize)
            .and_then(Option::as_mut)
        else {
            return BeginUpdateResult::NotFound;
        };
        if entry.instance_id != instance_id {
            return BeginUpdateResult::NotFound;
        }
        match entry.phase {
            SlotPhase::Pending { .. } | SlotPhase::Updating { .. } => BeginUpdateResult::InFlight,
            SlotPhase::Departing { .. } => BeginUpdateResult::NotFound,
            SlotPhase::Active {
                monitor_handle,
                adapter_luid,
                target_id,
                lease_expires_ms,
            } => {
                let preferred_refresh = new_spec.planned_modes.first().map(|m| m.refresh);
                let in_place_compatible = preferred_refresh.is_some_and(|r| {
                    entry
                        .arrival_modes
                        .contains(&(new_spec.width, new_spec.height, r))
                }) && (new_spec.hdr_profile == HdrProfile::Sdr
                    || entry.arrival_hdr_capable)
                    && new_spec.color_space == entry.spec.color_space
                    && new_spec.bit_depth == entry.spec.bit_depth
                    && new_spec.hdr_metadata == entry.spec.hdr_metadata;
                if !in_place_compatible {
                    Arc::make_mut(&mut new_spec).edid = fresh_edid;
                }
                entry.phase = SlotPhase::Updating {
                    monitor_handle,
                    adapter_luid,
                    target_id,
                    lease_expires_ms,
                    depart_requested: false,
                };
                let old_spec = core::mem::replace(&mut entry.spec, new_spec);
                BeginUpdateResult::Started {
                    handle: monitor_handle,
                    adapter_luid,
                    target_id,
                    old_spec,
                    in_place_compatible,
                }
            }
        }
    }

    pub(crate) fn update_updating_handle(
        &self,
        slot: ConnectorSlot,
        instance_id: u64,
        new_handle: MonitorHandle,
    ) {
        let mut guard = self.table.lock();
        if let Some(entry) = guard
            .slots
            .get_mut(slot.0 as usize)
            .and_then(Option::as_mut)
        {
            if entry.instance_id == instance_id {
                if let SlotPhase::Updating { monitor_handle, .. } = &mut entry.phase {
                    *monitor_handle = new_handle;
                }
            }
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "internal helper groups re-arrival metadata directly"
    )]
    pub(crate) fn finish_rearrive(
        &self,
        slot: ConnectorSlot,
        instance_id: u64,
        new_handle: MonitorHandle,
        adapter_luid: AdapterLuid,
        target_id: u32,
        arrival_modes: Vec<(u32, u32, RationalHz)>,
        arrival_hdr_capable: bool,
    ) -> Option<MonitorHandle> {
        let mut guard = self.table.lock();
        let entry = guard
            .slots
            .get_mut(slot.0 as usize)
            .and_then(Option::as_mut)?;
        if entry.instance_id != instance_id {
            return None;
        }
        entry.arrival_modes = arrival_modes;
        entry.arrival_hdr_capable = arrival_hdr_capable;
        let outcome = match entry.phase {
            SlotPhase::Updating {
                lease_expires_ms,
                depart_requested,
                ..
            } => {
                if depart_requested {
                    entry.phase = SlotPhase::Departing {
                        monitor_handle: new_handle,
                    };
                    Some(Some(new_handle))
                } else {
                    entry.phase = SlotPhase::Active {
                        monitor_handle: new_handle,
                        adapter_luid,
                        target_id,
                        lease_expires_ms,
                    };
                    Some(None)
                }
            }
            _ => None,
        };
        let depart_opt = outcome?;
        guard.total_departures = guard.total_departures.saturating_add(1);
        guard.total_arrivals = guard.total_arrivals.saturating_add(1);
        depart_opt
    }

    pub(crate) fn finish_update(
        &self,
        slot: ConnectorSlot,
        instance_id: u64,
        rollback_spec: Option<Arc<MonitorSpec>>,
    ) -> Option<MonitorHandle> {
        let mut guard = self.table.lock();
        let entry = guard
            .slots
            .get_mut(slot.0 as usize)
            .and_then(Option::as_mut)?;
        if entry.instance_id != instance_id {
            return None;
        }
        if let Some(old_spec) = rollback_spec {
            entry.spec = old_spec;
        }
        match entry.phase {
            SlotPhase::Updating {
                monitor_handle,
                adapter_luid,
                target_id,
                lease_expires_ms,
                depart_requested,
            } => {
                if depart_requested {
                    entry.phase = SlotPhase::Departing { monitor_handle };
                    Some(monitor_handle)
                } else {
                    entry.phase = SlotPhase::Active {
                        monitor_handle,
                        adapter_luid,
                        target_id,
                        lease_expires_ms,
                    };
                    None
                }
            }
            _ => None,
        }
    }

    fn begin_remove_entry(entry: &mut SlotEntry) -> BeginRemoveResult {
        let slot = entry.slot;
        let instance_id = entry.instance_id;
        match &mut entry.phase {
            SlotPhase::Pending {
                cancel_requested, ..
            } => {
                *cancel_requested = true;
                BeginRemoveResult::WaitPending { slot, instance_id }
            }
            SlotPhase::Updating { .. } => BeginRemoveResult::WaitUpdating { slot, instance_id },
            SlotPhase::Departing { .. } => BeginRemoveResult::WaitDeparting { slot, instance_id },
            SlotPhase::Active { monitor_handle, .. } => {
                let handle = *monitor_handle;
                entry.phase = SlotPhase::Departing {
                    monitor_handle: handle,
                };
                BeginRemoveResult::Depart {
                    slot,
                    instance_id,
                    handle,
                }
            }
        }
    }

    pub(crate) fn begin_cleanup_for_file(&self, file_key: FileKey) -> CleanupBatch {
        let mut guard = self.table.lock();
        let mut to_depart = Vec::new();
        let mut wait_instances = Vec::new();

        for entry in guard.slots.iter_mut().flatten() {
            if entry.owner != file_key || entry.spec.ownership_mode != OwnershipMode::HandleBound {
                continue;
            }
            let slot = entry.slot;
            let instance_id = entry.instance_id;
            match &mut entry.phase {
                SlotPhase::Active { monitor_handle, .. } => {
                    let handle = *monitor_handle;
                    entry.phase = SlotPhase::Departing {
                        monitor_handle: handle,
                    };
                    to_depart.push(DepartTarget {
                        slot,
                        instance_id,
                        handle,
                    });
                }
                SlotPhase::Pending {
                    cancel_requested, ..
                } => {
                    *cancel_requested = true;
                    wait_instances.push((slot, instance_id));
                }
                SlotPhase::Updating {
                    depart_requested, ..
                } => {
                    *depart_requested = true;
                    wait_instances.push((slot, instance_id));
                }
                SlotPhase::Departing { .. } => {
                    wait_instances.push((slot, instance_id));
                }
            }
        }

        CleanupBatch {
            to_depart,
            wait_instances,
        }
    }

    pub(crate) fn collect_expired_leases(&self, now_ms: u64) -> Vec<DepartTarget> {
        let mut guard = self.table.lock();
        let mut expired = Vec::new();
        for entry in guard.slots.iter_mut().flatten() {
            if entry.spec.ownership_mode != OwnershipMode::Leased {
                continue;
            }
            match &mut entry.phase {
                SlotPhase::Active {
                    monitor_handle,
                    lease_expires_ms: Some(expires_ms),
                    ..
                } if now_ms >= *expires_ms => {
                    let handle = *monitor_handle;
                    entry.phase = SlotPhase::Departing {
                        monitor_handle: handle,
                    };
                    expired.push(DepartTarget {
                        slot: entry.slot,
                        instance_id: entry.instance_id,
                        handle,
                    });
                }
                SlotPhase::Updating {
                    lease_expires_ms: Some(expires_ms),
                    depart_requested,
                    ..
                } if now_ms >= *expires_ms => {
                    *depart_requested = true;
                }
                _ => {}
            }
        }
        expired
    }

    pub(crate) fn renew_lease(
        &self,
        guid: [u8; 16],
        now_ms: u64,
        ttl_secs: u32,
    ) -> RenewLeaseOutcome {
        let mut guard = self.table.lock();
        let Some(entry) = guard.find_by_guid_mut(guid) else {
            return RenewLeaseOutcome::NotFound;
        };

        if entry.spec.ownership_mode != OwnershipMode::Leased {
            return RenewLeaseOutcome::NotLeased;
        }

        match &mut entry.phase {
            SlotPhase::Pending { .. } | SlotPhase::Departing { .. } => RenewLeaseOutcome::InFlight,
            SlotPhase::Active {
                lease_expires_ms, ..
            }
            | SlotPhase::Updating {
                lease_expires_ms, ..
            } => {
                if let Some(expires_ms) = *lease_expires_ms {
                    if now_ms >= expires_ms {
                        return RenewLeaseOutcome::Expired;
                    }
                }
                let ttl_ms = u64::from(ttl_secs).saturating_mul(1000);
                *lease_expires_ms = Some(now_ms.saturating_add(ttl_ms));
                RenewLeaseOutcome::Renewed
            }
        }
    }

    pub(crate) fn snapshot_displays(&self, now_ms: u64) -> (Vec<DisplayInfo>, u32) {
        let guard = self.table.lock();
        let mut out = Vec::new();
        let mut total_active: u32 = 0;

        for entry in guard.slots.iter().flatten() {
            let (adapter_luid, target_id, lease_expires_ms) = match entry.phase {
                SlotPhase::Active {
                    adapter_luid,
                    target_id,
                    lease_expires_ms,
                    ..
                }
                | SlotPhase::Updating {
                    adapter_luid,
                    target_id,
                    lease_expires_ms,
                    ..
                } => (adapter_luid, target_id, lease_expires_ms),
                SlotPhase::Pending { .. } | SlotPhase::Departing { .. } => continue,
            };

            total_active = total_active.saturating_add(1);
            if out.len() < MAX_LIST_DISPLAYS {
                let lease_remaining_secs = match lease_expires_ms {
                    Some(expires_ms) => {
                        let rem_ms = expires_ms.saturating_sub(now_ms);
                        let secs = rem_ms.div_ceil(1000);
                        u32::try_from(secs).unwrap_or(u32::MAX)
                    }
                    None => 0,
                };
                out.push(DisplayInfo {
                    monitor_guid: entry.spec.monitor_guid,
                    adapter_luid_low: adapter_luid.low,
                    adapter_luid_high: adapter_luid.high,
                    target_id,
                    connector_index: entry.slot.0,
                    width: entry.spec.width,
                    height: entry.spec.height,
                    refresh_num: entry.spec.refresh_num,
                    refresh_den: entry.spec.refresh_den,
                    lease_remaining_secs,
                    hdr_profile: entry.spec.hdr_profile.as_u8(),
                    ownership_mode: entry.spec.ownership_mode.as_u8(),
                    client_name_len: entry.spec.client_name_len,
                    active: 1,
                    client_name: entry.spec.client_name,
                });
            }
        }

        (out, total_active)
    }

    pub(crate) fn stats(&self) -> RegistryStats {
        let guard = self.table.lock();
        let active_display_count = guard
            .slots
            .iter()
            .flatten()
            .filter(|entry| {
                matches!(
                    entry.phase,
                    SlotPhase::Active { .. } | SlotPhase::Updating { .. }
                )
            })
            .count();
        RegistryStats {
            total_arrivals: guard.total_arrivals,
            total_departures: guard.total_departures,
            // WHY: active_display_count <= MAX_MONITORS (16), so cast to u32 cannot truncate.
            active_display_count: active_display_count as u32,
        }
    }

    /// Returns the number of currently active virtual monitors.
    #[must_use]
    pub fn active_count(&self) -> usize {
        let guard = self.table.lock();
        guard
            .slots
            .iter()
            .flatten()
            .filter(|entry| {
                matches!(
                    entry.phase,
                    SlotPhase::Active { .. } | SlotPhase::Updating { .. }
                )
            })
            .count()
    }

    /// Returns the number of unoccupied connector slots.
    #[must_use]
    pub fn free_slot_count(&self) -> usize {
        let guard = self.table.lock();
        guard.slots.iter().filter(|slot| slot.is_none()).count()
    }
}

impl Default for MonitorRegistry {
    fn default() -> Self {
        Self::new()
    }
}
