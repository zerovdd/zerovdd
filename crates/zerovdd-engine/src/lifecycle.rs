//! Core virtual display lifecycle orchestration and re-entrant callback handlers.

use alloc::sync::Arc;
use alloc::vec::Vec;

use zerovdd_edid::{
    synthesize_edid_vec, validate_custom_edid, ColorProfile, EdidSpec, EotfFlags, HdrStaticMetadata,
};
use zerovdd_modes::{
    plan_modes, BitDepthMask, ColorCapability, ModePlanRequest, PlannedMode, RationalHz,
    VideoSignalInfo,
};
use zerovdd_proto::types::{
    AddDisplayRequest, AddDisplayResponse, BitDepthPreference, ColorSpacePreference,
    DriverStatusResponse, HdrMasteringMetadata, HdrProfile, ListDisplaysResponse, OwnershipMode,
    ProtocolVersion, RemoveDisplayRequest, RenewLeaseRequest, UpdateDisplayModeRequest,
    UpdateDisplayModeResponse, REMOVE_FLAG_BY_TARGET_ID, UPDATE_MODE_FLAG_BY_TARGET_ID,
};
use zerovdd_proto::validation::Validate;

use crate::gpu::{GpuController, GpuSwitchError, PendingGpuSwitch, SwapchainAssignAction};
use crate::platform::{AdapterLuid, AdapterState, FileKey, IddPlatform, MAX_MONITORS};
use crate::registry::{
    BeginRemoveResult, BeginUpdateResult, LookupExistingResult, LookupUpdateResult,
    MonitorQueryKey, MonitorRegistry, MonitorSpec, ReservePendingResult,
};
use crate::{watchdog, EngineError};

/// Output returned to `EvtIddCxParseMonitorDescription[2]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseMonitorResult {
    /// Cloned specification for the queried monitor.
    pub spec: Arc<MonitorSpec>,
    /// Raw EDID bytes associated with the monitor.
    pub edid: Vec<u8>,
    /// Preferred mode guaranteed to match index `0`.
    pub preferred_mode: PlannedMode,
    /// Monitor-description timings (`vsync_freq_divider = 0`).
    pub monitor_signals: Vec<VideoSignalInfo>,
}

/// Output returned to `EvtIddCxMonitorQueryTargetModes[2]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetModesResult {
    /// Cloned specification for the queried monitor.
    pub spec: Arc<MonitorSpec>,
    /// Target mode timings (`vsync_freq_divider = 1`) aligned field-for-field with [`ParseMonitorResult`].
    pub target_signals: Vec<VideoSignalInfo>,
    /// Full planned modes including bit-depth masks and preferred flag.
    pub planned_modes: Vec<PlannedMode>,
}

/// Re-entrancy-safe state engine coordinating monitor lifecycle, power state, and GPU switching.
pub struct VddEngine {
    registry: MonitorRegistry,
    gpu: GpuController,
}

impl VddEngine {
    /// Creates a new [`VddEngine`] with empty monitor slots and uninitialized adapter state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            registry: MonitorRegistry::new(),
            gpu: GpuController::new(),
        }
    }

    /// Creates or idempotently attaches a virtual display on behalf of `file_key`.
    pub fn add_display(
        &self,
        file_key: FileKey,
        req: &AddDisplayRequest,
        platform: &dyn IddPlatform,
    ) -> Result<AddDisplayResponse, EngineError> {
        req.validate()?;

        let mut cached_spec: Option<Arc<MonitorSpec>> = None;

        let (slot, instance_id, spec) = loop {
            let now_ms = platform.now_monotonic_ms();
            if cached_spec.is_none() {
                match self
                    .registry
                    .check_existing(file_key, req.monitor_guid, now_ms)
                {
                    LookupExistingResult::NotPresent => {}
                    LookupExistingResult::Existing(resp) => return Ok(resp),
                    LookupExistingResult::Conflict => return Err(EngineError::MonitorConflict),
                    LookupExistingResult::WaitInFlight => {
                        core::hint::spin_loop();
                        continue;
                    }
                    LookupExistingResult::ExpiredToDepart {
                        slot,
                        instance_id,
                        handle,
                    } => {
                        let depart_res = platform.depart_monitor(handle);
                        self.registry
                            .remove_slot(slot, instance_id, depart_res.is_ok());
                        depart_res?;
                        continue;
                    }
                }
                cached_spec = Some(build_monitor_spec(req)?);
            }

            let spec_ref = cached_spec.as_ref().ok_or(EngineError::MonitorNotFound)?;
            match self
                .registry
                .reserve_pending(file_key, Arc::clone(spec_ref), now_ms)
            {
                ReservePendingResult::Reserved { slot, instance_id } => {
                    break (slot, instance_id, Arc::clone(spec_ref));
                }
                ReservePendingResult::Existing(resp) => return Ok(resp),
                ReservePendingResult::Conflict => return Err(EngineError::MonitorConflict),
                ReservePendingResult::WaitInFlight => {
                    core::hint::spin_loop();
                    continue;
                }
                ReservePendingResult::ExpiredToDepart {
                    slot,
                    instance_id,
                    handle,
                } => {
                    let depart_res = platform.depart_monitor(handle);
                    self.registry
                        .remove_slot(slot, instance_id, depart_res.is_ok());
                    depart_res?;
                    continue;
                }
                ReservePendingResult::NoFreeSlots => {
                    return Err(EngineError::NoFreeConnectorSlots { max: MAX_MONITORS });
                }
            }
        };

        // WHY: Platform calls execute strictly outside the registry mutex so synchronous
        // re-entrant ParseMonitorDescription / QueryTargetModes callbacks never deadlock.
        let handle = match platform.create_monitor(slot, spec.monitor_guid, &spec.edid) {
            Ok(handle) => handle,
            Err(err) => {
                self.registry.remove_slot(slot, instance_id, false);
                return Err(EngineError::Platform(err));
            }
        };

        if !self
            .registry
            .attach_handle_or_cancel(slot, instance_id, handle)
        {
            let _ = platform.depart_monitor(handle);
            self.registry.remove_slot(slot, instance_id, false);
            return Err(EngineError::CancelledDuringArrival);
        }

        let arrival = match platform.arrive_monitor(handle, slot, spec.monitor_guid) {
            Ok(arrival) => arrival,
            Err(err) => {
                // WHY: Keep the slot occupied as Departing until depart_monitor finishes so
                // concurrent add_display cannot reuse the slot and cleanup cannot return early.
                self.registry.mark_departing(slot, instance_id, handle);
                let _ = platform.depart_monitor(handle);
                self.registry.remove_slot(slot, instance_id, false);
                return Err(EngineError::Platform(err));
            }
        };

        let lease_expires_ms = match spec.ownership_mode {
            OwnershipMode::HandleBound => None,
            OwnershipMode::Leased => {
                let ttl_ms = u64::from(req.lease_ttl_secs).saturating_mul(1000);
                Some(platform.now_monotonic_ms().saturating_add(ttl_ms))
            }
        };

        if !self.registry.promote_active(
            slot,
            instance_id,
            handle,
            arrival.adapter_luid,
            arrival.target_id,
            lease_expires_ms,
        ) {
            let _ = platform.depart_monitor(handle);
            self.registry.remove_slot(slot, instance_id, false);
            return Err(EngineError::CancelledDuringArrival);
        }

        Ok(AddDisplayResponse {
            adapter_luid_low: arrival.adapter_luid.low,
            adapter_luid_high: arrival.adapter_luid.high,
            target_id: arrival.target_id,
            connector_index: slot.0,
            created_new: 1,
            reserved: [0; 3],
        })
    }

    /// Departs and removes an active or pending virtual display matching `req`.
    ///
    /// Because the Control Device itself is privileged, administrative removal targets any
    /// matching monitor regardless of the originating `FileKey`, whereas [`Self::on_file_cleanup`]
    /// only departs `HandleBound` monitors owned by the closing `FileKey`.
    pub fn remove_display(
        &self,
        _file_key: FileKey,
        req: &RemoveDisplayRequest,
        platform: &dyn IddPlatform,
    ) -> Result<(), EngineError> {
        req.validate()?;

        loop {
            let outcome = if (req.flags & REMOVE_FLAG_BY_TARGET_ID) != 0 {
                self.registry.begin_remove_by_target_id(req.target_id)
            } else {
                self.registry.begin_remove_by_guid(req.monitor_guid)
            };

            match outcome {
                BeginRemoveResult::NotFound => return Err(EngineError::MonitorNotFound),
                BeginRemoveResult::WaitPending { slot, instance_id } => {
                    while self.registry.is_instance_present(slot, instance_id) {
                        core::hint::spin_loop();
                    }
                    return Ok(());
                }
                BeginRemoveResult::WaitUpdating { slot, instance_id } => {
                    while self.registry.is_instance_updating(slot, instance_id) {
                        core::hint::spin_loop();
                    }
                    continue;
                }
                BeginRemoveResult::WaitDeparting { slot, instance_id } => {
                    while self.registry.is_instance_present(slot, instance_id) {
                        core::hint::spin_loop();
                    }
                    continue;
                }
                BeginRemoveResult::Depart {
                    slot,
                    instance_id,
                    handle,
                } => {
                    let depart_res = platform.depart_monitor(handle);
                    self.registry
                        .remove_slot(slot, instance_id, depart_res.is_ok());
                    depart_res?;
                    return Ok(());
                }
            }
        }
    }

    /// Updates the planned target mode list on an active virtual monitor in place via `IddCxMonitorUpdateModes2`.
    pub fn update_display_mode(
        &self,
        _file_key: FileKey,
        req: &UpdateDisplayModeRequest,
        platform: &dyn IddPlatform,
    ) -> Result<UpdateDisplayModeResponse, EngineError> {
        req.validate()?;

        let lookup = if (req.flags & UPDATE_MODE_FLAG_BY_TARGET_ID) != 0 {
            self.registry.lookup_active_by_target_id(req.target_id)
        } else {
            self.registry.lookup_active_by_guid(req.monitor_guid)
        };

        let (slot, instance_id, prev_spec) = match lookup {
            LookupUpdateResult::NotFound => return Err(EngineError::MonitorNotFound),
            LookupUpdateResult::InFlight => return Err(EngineError::MonitorConflict),
            LookupUpdateResult::Active {
                slot,
                instance_id,
                spec,
            } => (slot, instance_id, spec),
        };

        let hdr_profile = HdrProfile::try_from(req.hdr_profile)?;
        let color_space = ColorSpacePreference::try_from(req.color_space)?;
        let bit_depth = BitDepthPreference::try_from(req.bit_depth)?;
        let normalized_hdr_meta = normalize_hdr_metadata(hdr_profile, req.hdr_metadata);
        let refresh = RationalHz::new(u64::from(req.refresh_num), u64::from(req.refresh_den))?;
        let color_capability = match hdr_profile {
            HdrProfile::Sdr => ColorCapability::Sdr,
            HdrProfile::Hdr10 => ColorCapability::Hdr,
        };
        let mut plan_req = ModePlanRequest::new(req.width, req.height, refresh, color_capability);
        plan_req.bit_depths_override = bit_depth_to_mask(bit_depth);
        let planned_modes = plan_modes(&plan_req)?;
        let mode_count = u32::try_from(planned_modes.len()).unwrap_or(u32::MAX);
        let client_name = core::str::from_utf8(
            prev_spec
                .client_name
                .get(..usize::from(prev_spec.client_name_len))
                .unwrap_or(&[]),
        )
        .unwrap_or("zerovdd");
        let refresh_mhz =
            u32::try_from((u64::from(req.refresh_num) * 1000) / u64::from(req.refresh_den.max(1)))
                .unwrap_or(60_000)
                .max(1);
        let fresh_edid = synthesize_default_edid(
            prev_spec.monitor_guid,
            client_name,
            req.width,
            req.height,
            refresh_mhz,
            hdr_profile,
            color_space,
            bit_depth,
            prev_spec.target_ppi,
            Some(normalized_hdr_meta),
        )?;
        let new_arrival_modes: Vec<(u32, u32, RationalHz)> = planned_modes
            .iter()
            .map(|m| (m.width, m.height, m.refresh))
            .collect();
        let new_arrival_hdr = hdr_profile == HdrProfile::Hdr10;

        let new_spec = Arc::new(MonitorSpec {
            monitor_guid: prev_spec.monitor_guid,
            client_name: prev_spec.client_name,
            client_name_len: prev_spec.client_name_len,
            width: req.width,
            height: req.height,
            refresh_num: req.refresh_num,
            refresh_den: req.refresh_den,
            hdr_profile,
            color_space,
            bit_depth,
            target_ppi: prev_spec.target_ppi,
            hdr_metadata: normalized_hdr_meta,
            ownership_mode: prev_spec.ownership_mode,
            edid: prev_spec.edid.clone(),
            planned_modes,
        });

        let (handle, adapter_luid, target_id, old_spec, in_place_compatible) = match self
            .registry
            .begin_update_spec(slot, instance_id, Arc::clone(&new_spec), fresh_edid)
        {
            BeginUpdateResult::NotFound => return Err(EngineError::MonitorNotFound),
            BeginUpdateResult::InFlight => return Err(EngineError::MonitorConflict),
            BeginUpdateResult::Started {
                handle,
                adapter_luid,
                target_id,
                old_spec,
                in_place_compatible,
            } => (
                handle,
                adapter_luid,
                target_id,
                old_spec,
                in_place_compatible,
            ),
        };

        if in_place_compatible {
            // WHY: Publish the updated MonitorSpec while transitioning to SlotPhase::Updating before
            // calling IddCxMonitorUpdateModes2 so synchronous callbacks see the new mode list and
            // concurrent cleanups/expiries defer IddCxMonitorDeparture until the update completes.
            let update_res = platform.update_monitor_modes(handle, &new_spec.planned_modes);
            let rollback = update_res.is_err().then_some(old_spec);
            if let Some(depart_handle) = self.registry.finish_update(slot, instance_id, rollback) {
                let depart_res = platform.depart_monitor(depart_handle);
                self.registry
                    .remove_slot(slot, instance_id, depart_res.is_ok());
                update_res?;
                depart_res?;
                return Err(EngineError::MonitorNotFound);
            }
            update_res?;

            return Ok(UpdateDisplayModeResponse {
                adapter_luid_low: adapter_luid.low,
                adapter_luid_high: adapter_luid.high,
                target_id,
                connector_index: slot.0,
                mode_count,
            });
        }

        // WHY: Windows Display Mode Manager intersects target modes with arrival EDID monitor modes
        // and CTA-861 HDR metadata, so out-of-EDID resolutions or SDR-to-HDR upgrades require re-arrival.
        if let Err(err) = platform.depart_monitor(handle) {
            if let Some(depart_handle) =
                self.registry
                    .finish_update(slot, instance_id, Some(old_spec))
            {
                let _ = platform.depart_monitor(depart_handle);
                self.registry.remove_slot(slot, instance_id, false);
            }
            return Err(EngineError::Platform(err));
        }

        let recreated_edid = self
            .registry
            .clone_spec(MonitorQueryKey::ConnectorSlot(slot))
            .map(|spec| spec.edid.clone())
            .unwrap_or_default();
        let new_handle =
            match platform.create_monitor(slot, prev_spec.monitor_guid, &recreated_edid) {
                Ok(h) => h,
                Err(err) => {
                    self.registry.remove_slot(slot, instance_id, true);
                    return Err(EngineError::Platform(err));
                }
            };

        self.registry
            .update_updating_handle(slot, instance_id, new_handle);
        let arrival = match platform.arrive_monitor(new_handle, slot, prev_spec.monitor_guid) {
            Ok(arrival) => arrival,
            Err(err) => {
                self.registry.mark_departing(slot, instance_id, new_handle);
                let _ = platform.depart_monitor(new_handle);
                self.registry.remove_slot(slot, instance_id, true);
                return Err(EngineError::Platform(err));
            }
        };

        if let Some(depart_handle) = self.registry.finish_rearrive(
            slot,
            instance_id,
            new_handle,
            arrival.adapter_luid,
            arrival.target_id,
            new_arrival_modes,
            new_arrival_hdr,
        ) {
            let depart_res = platform.depart_monitor(depart_handle);
            self.registry
                .remove_slot(slot, instance_id, depart_res.is_ok());
            depart_res?;
            return Err(EngineError::MonitorNotFound);
        }

        Ok(UpdateDisplayModeResponse {
            adapter_luid_low: arrival.adapter_luid.low,
            adapter_luid_high: arrival.adapter_luid.high,
            target_id: arrival.target_id,
            connector_index: slot.0,
            mode_count,
        })
    }

    /// Lists all active virtual monitors and remaining lease durations.
    #[must_use]
    pub fn list_displays(&self, platform: &dyn IddPlatform) -> ListDisplaysResponse {
        let now_ms = platform.now_monotonic_ms();
        let (entries, total_active) = self.registry.snapshot_displays(now_ms);
        let mut resp = ListDisplaysResponse::empty();
        // WHY: snapshot_displays caps entries at MAX_LIST_DISPLAYS (16), so cast to u32 cannot truncate.
        resp.count = entries.len() as u32;
        resp.total_active = total_active;
        for (idx, info) in entries.into_iter().enumerate() {
            resp.displays[idx] = info;
        }
        resp
    }

    /// Handles synchronous `EvtIddCxParseMonitorDescription[2]` re-entrancy safely.
    ///
    /// Acquires the registry lock only long enough to clone `Arc<MonitorSpec>`.
    pub fn on_parse_monitor_description(
        &self,
        key: MonitorQueryKey<'_>,
    ) -> Result<ParseMonitorResult, EngineError> {
        let spec = self
            .registry
            .clone_spec(key)
            .ok_or(EngineError::MonitorNotFound)?;
        let preferred_mode = *spec
            .planned_modes
            .first()
            .ok_or(EngineError::MonitorNotFound)?;
        let monitor_signals = spec
            .planned_modes
            .iter()
            .map(|mode| mode.monitor_signal)
            .collect();
        let edid = spec.edid.clone();

        Ok(ParseMonitorResult {
            spec,
            edid,
            preferred_mode,
            monitor_signals,
        })
    }

    /// Handles `EvtIddCxMonitorQueryTargetModes[2]` safely using the same `MonitorSpec`.
    pub fn on_query_target_modes(
        &self,
        key: MonitorQueryKey<'_>,
    ) -> Result<TargetModesResult, EngineError> {
        let spec = self
            .registry
            .clone_spec(key)
            .ok_or(EngineError::MonitorNotFound)?;
        let target_signals = spec
            .planned_modes
            .iter()
            .map(|mode| mode.target_signal)
            .collect();
        let planned_modes = spec.planned_modes.clone();

        Ok(TargetModesResult {
            spec,
            target_signals,
            planned_modes,
        })
    }

    /// Renews the lease on an active leased virtual monitor.
    pub fn renew_lease(
        &self,
        file_key: FileKey,
        req: &RenewLeaseRequest,
        platform: &dyn IddPlatform,
    ) -> Result<(), EngineError> {
        watchdog::renew_lease(&self.registry, file_key, req, platform)
    }

    /// Departs all expired leased virtual monitors.
    pub fn reap_expired_leases(&self, platform: &dyn IddPlatform) -> Result<usize, EngineError> {
        watchdog::reap_expired_leases(&self.registry, platform)
    }

    /// Departs all `HandleBound` monitors owned by `file_key` on handle cleanup.
    pub fn on_file_cleanup(
        &self,
        file_key: FileKey,
        platform: &dyn IddPlatform,
    ) -> Result<usize, EngineError> {
        watchdog::on_file_cleanup(&self.registry, file_key, platform)
    }

    /// Handles `EvtDeviceD0Entry` idempotently across initial boot and sleep/resume.
    pub fn on_d0_entry(&self, platform: &dyn IddPlatform) -> Result<bool, EngineError> {
        self.gpu.on_d0_entry(platform)
    }

    /// Handles `EvtDeviceD0Exit`, moving `Ready -> Suspended`.
    pub fn on_d0_exit(&self) -> AdapterState {
        self.gpu.on_d0_exit()
    }

    /// Transitions `Uninitialized -> Initializing` strictly once.
    pub fn should_init_adapter(&self) -> bool {
        self.gpu.should_init_adapter()
    }

    /// Completes an asynchronous adapter initialization started via [`Self::should_init_adapter`].
    pub fn on_adapter_init_finished(&self, succeeded: bool) {
        self.gpu.on_adapter_init_finished(succeeded);
    }

    /// Sets whether the adapter exposes FP16 / HDR10 swapchain support.
    pub fn set_hdr_capable(&self, capable: bool) {
        self.gpu.set_hdr_capable(capable);
    }

    /// Requests switching the render GPU adapter to `target_luid`.
    pub fn request_gpu_switch(
        &self,
        target_luid: AdapterLuid,
        platform: &dyn IddPlatform,
    ) -> Result<(), EngineError> {
        self.gpu.request_gpu_switch(target_luid, platform)
    }

    /// Evaluates swapchain assignment against any pending render GPU switch.
    pub fn on_assign_swapchain(&self, current_adapter_luid: AdapterLuid) -> SwapchainAssignAction {
        self.gpu.on_assign_swapchain(current_adapter_luid)
    }

    /// Returns the current [`AdapterState`].
    #[must_use]
    pub fn adapter_state(&self) -> AdapterState {
        self.gpu.adapter_state()
    }

    /// Returns the active [`PendingGpuSwitch`], if any.
    #[must_use]
    pub fn pending_gpu_switch(&self) -> Option<PendingGpuSwitch> {
        self.gpu.pending_gpu_switch()
    }

    /// Returns the most recent [`GpuSwitchError`], if any.
    #[must_use]
    pub fn last_gpu_switch_error(&self) -> Option<GpuSwitchError> {
        self.gpu.last_gpu_switch_error()
    }

    /// Builds a wire [`DriverStatusResponse`] snapshot for `IOCTL_GET_STATUS`.
    #[must_use]
    pub fn get_status(&self, platform: &dyn IddPlatform) -> DriverStatusResponse {
        let now_ms = platform.now_monotonic_ms();
        let reg = self.registry.stats();
        let gpu = self.gpu.snapshot();
        let uptime_secs = gpu
            .init_time_ms
            .map_or(0, |init_ms| now_ms.saturating_sub(init_ms) / 1000);

        DriverStatusResponse {
            protocol_version: ProtocolVersion::CURRENT,
            total_arrivals: reg.total_arrivals,
            total_departures: reg.total_departures,
            uptime_secs,
            render_adapter_luid_low: gpu.render_adapter_luid.low,
            render_adapter_luid_high: gpu.render_adapter_luid.high,
            active_display_count: reg.active_display_count,
            // WHY: MAX_MONITORS is 16, so cast to u32 cannot truncate.
            max_displays: MAX_MONITORS as u32,
            adapter_ready: u8::from(gpu.adapter_state == AdapterState::Ready),
            hdr_capable: u8::from(gpu.hdr_capable),
            last_gpu_switch_failed: u8::from(gpu.last_gpu_switch_failed),
            reserved: [0; 5],
        }
    }

    /// Returns the number of currently active virtual monitors.
    #[must_use]
    pub fn active_display_count(&self) -> usize {
        self.registry.active_count()
    }

    /// Returns the number of unoccupied connector slots.
    #[must_use]
    pub fn free_slot_count(&self) -> usize {
        self.registry.free_slot_count()
    }
}

impl Default for VddEngine {
    fn default() -> Self {
        Self::new()
    }
}

const fn bit_depth_to_mask(pref: BitDepthPreference) -> Option<BitDepthMask> {
    match pref {
        BitDepthPreference::Auto => None,
        BitDepthPreference::Bpc8 => Some(BitDepthMask::BITS_8),
        BitDepthPreference::Bpc10 => Some(BitDepthMask::BITS_10),
        BitDepthPreference::Bpc12 => Some(BitDepthMask::BITS_12),
    }
}

const fn bit_depth_to_bpc(pref: BitDepthPreference) -> Option<u8> {
    match pref {
        BitDepthPreference::Auto => None,
        BitDepthPreference::Bpc8 => Some(8),
        BitDepthPreference::Bpc10 => Some(10),
        BitDepthPreference::Bpc12 => Some(12),
    }
}

const fn normalize_hdr_metadata(
    hdr_profile: HdrProfile,
    meta: HdrMasteringMetadata,
) -> HdrMasteringMetadata {
    match hdr_profile {
        HdrProfile::Sdr => HdrMasteringMetadata {
            max_luminance_nits: 0,
            max_fall_nits: 0,
            min_luminance_ten_thousandths: 0,
        },
        HdrProfile::Hdr10 => {
            let defaults = HdrMasteringMetadata::DEFAULT_HDR10;
            HdrMasteringMetadata {
                max_luminance_nits: if meta.max_luminance_nits == 0 {
                    defaults.max_luminance_nits
                } else {
                    meta.max_luminance_nits
                },
                max_fall_nits: if meta.max_fall_nits == 0 {
                    defaults.max_fall_nits
                } else {
                    meta.max_fall_nits
                },
                min_luminance_ten_thousandths: if meta.min_luminance_ten_thousandths == 0 {
                    defaults.min_luminance_ten_thousandths
                } else {
                    meta.min_luminance_ten_thousandths
                },
            }
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "EDID synthesis inputs mirror validated AddDisplay/UpdateDisplayMode wire fields"
)]
fn synthesize_default_edid(
    monitor_guid: [u8; 16],
    client_name: &str,
    width: u32,
    height: u32,
    refresh_mhz: u32,
    hdr_profile: HdrProfile,
    color_space: ColorSpacePreference,
    bit_depth: BitDepthPreference,
    target_ppi: u16,
    hdr_metadata: Option<HdrMasteringMetadata>,
) -> Result<Vec<u8>, EngineError> {
    let width_u16 =
        u16::try_from(width).map_err(|_| EngineError::DimensionsOutOfRange { width, height })?;
    let height_u16 =
        u16::try_from(height).map_err(|_| EngineError::DimensionsOutOfRange { width, height })?;
    let color_profile = match (hdr_profile, color_space) {
        (HdrProfile::Sdr, ColorSpacePreference::Auto | ColorSpacePreference::Bt709) => {
            ColorProfile::SdrBt709
        }
        (HdrProfile::Hdr10, ColorSpacePreference::Auto | ColorSpacePreference::Bt2020) => {
            ColorProfile::HdrBt2020
        }
        (HdrProfile::Hdr10, ColorSpacePreference::DciP3) => ColorProfile::HdrDciP3,
        (HdrProfile::Sdr, ColorSpacePreference::Bt2020 | ColorSpacePreference::DciP3)
        | (HdrProfile::Hdr10, ColorSpacePreference::Bt709) => {
            return Err(EngineError::Proto(
                zerovdd_proto::validation::ProtoError::IncompatibleColorSettings {
                    hdr_profile: hdr_profile.as_u8(),
                    color_space: color_space.as_u8(),
                    bit_depth: bit_depth.as_u8(),
                },
            ));
        }
    };
    let hdr_override = match hdr_profile {
        HdrProfile::Sdr => Some(None),
        HdrProfile::Hdr10 => {
            let defaults = HdrStaticMetadata::default();
            let meta = hdr_metadata.unwrap_or(HdrMasteringMetadata {
                max_luminance_nits: 0,
                max_fall_nits: 0,
                min_luminance_ten_thousandths: 0,
            });
            Some(Some(HdrStaticMetadata {
                eotf: EotfFlags::PQ_AND_HLG,
                static_metadata_type1: true,
                max_luminance_nits: if meta.max_luminance_nits == 0 {
                    defaults.max_luminance_nits
                } else {
                    Some(meta.max_luminance_nits)
                },
                max_fall_nits: if meta.max_fall_nits == 0 {
                    defaults.max_fall_nits
                } else {
                    Some(meta.max_fall_nits)
                },
                min_luminance_ten_thousandths: if meta.min_luminance_ten_thousandths == 0 {
                    defaults.min_luminance_ten_thousandths
                } else {
                    Some(meta.min_luminance_ten_thousandths)
                },
            }))
        }
    };

    let mut edid_spec = EdidSpec::new(
        monitor_guid,
        client_name,
        width_u16,
        height_u16,
        refresh_mhz.max(1),
    );
    edid_spec.target_ppi = (target_ppi != 0).then_some(target_ppi);
    edid_spec.color_profile = color_profile;
    edid_spec.bit_depth_bpc = bit_depth_to_bpc(bit_depth);
    edid_spec.hdr_override = hdr_override;
    Ok(synthesize_edid_vec(&edid_spec)?)
}

fn build_monitor_spec(req: &AddDisplayRequest) -> Result<Arc<MonitorSpec>, EngineError> {
    let hdr_profile = HdrProfile::try_from(req.hdr_profile)?;
    let color_space = ColorSpacePreference::try_from(req.color_space)?;
    let bit_depth = BitDepthPreference::try_from(req.bit_depth)?;
    let normalized_hdr_meta = normalize_hdr_metadata(hdr_profile, req.hdr_metadata);
    let ownership_mode = OwnershipMode::try_from(req.ownership_mode)?;
    let client_name = req.client_name_str()?;
    let custom_edid = req.custom_edid_slice()?;

    let edid = if !custom_edid.is_empty() {
        validate_custom_edid(custom_edid)?;
        custom_edid.to_vec()
    } else {
        synthesize_default_edid(
            req.monitor_guid,
            client_name,
            req.width,
            req.height,
            req.refresh_mhz(),
            hdr_profile,
            color_space,
            bit_depth,
            req.target_ppi,
            Some(normalized_hdr_meta),
        )?
    };

    let refresh = RationalHz::new(u64::from(req.refresh_num), u64::from(req.refresh_den))?;
    let color_capability = match hdr_profile {
        HdrProfile::Sdr => ColorCapability::Sdr,
        HdrProfile::Hdr10 => ColorCapability::Hdr,
    };
    let mut plan_req = ModePlanRequest::new(req.width, req.height, refresh, color_capability);
    plan_req.bit_depths_override = bit_depth_to_mask(bit_depth);
    let planned_modes = plan_modes(&plan_req)?;

    Ok(Arc::new(MonitorSpec {
        monitor_guid: req.monitor_guid,
        client_name: req.client_name,
        client_name_len: req.client_name_len,
        width: req.width,
        height: req.height,
        refresh_num: req.refresh_num,
        refresh_den: req.refresh_den,
        hdr_profile,
        color_space,
        bit_depth,
        target_ppi: req.target_ppi,
        hdr_metadata: normalized_hdr_meta,
        ownership_mode,
        edid,
        planned_modes,
    }))
}
