use std::sync::{Arc, Barrier};
use std::thread;

use zerovdd_engine::{
    AdapterLuid, AdapterState, ConnectorSlot, EngineError, FakeIddPlatform, FileKey,
    GpuSwitchError, IddPlatform, MonitorArrivalResult, MonitorHandle, PlatformError,
    SwapchainAssignAction, VddEngine, MAX_MONITORS,
};
use zerovdd_proto::types::{AddDisplayRequest, HdrProfile, OwnershipMode, RenewLeaseRequest};

#[test]
fn injected_arrive_failure_rolls_back_slot_and_leaves_no_ghost_monitors() {
    let engine = VddEngine::new();
    let platform = FakeIddPlatform::new();
    platform.set_fail_arrive_monitor(true);

    let req = AddDisplayRequest::new(
        [0x22; 16],
        1920,
        1080,
        60,
        1,
        HdrProfile::Sdr,
        OwnershipMode::HandleBound,
    );

    let err = engine
        .add_display(FileKey(1), &req, &platform)
        .expect_err("injected arrive failure must fail add_display");
    assert!(matches!(err, EngineError::Platform(_)));

    assert_eq!(engine.active_display_count(), 0);
    assert_eq!(engine.free_slot_count(), MAX_MONITORS);
    assert_eq!(engine.list_displays(&platform).count, 0);
    assert_eq!(engine.list_displays(&platform).total_active, 0);
    assert_eq!(platform.created_monitor_count(), 0);
    assert_eq!(platform.create_monitor_calls(), 1);
    assert_eq!(platform.depart_monitor_calls(), 1);

    platform.set_fail_create_monitor(true);
    let err2 = engine
        .add_display(FileKey(1), &req, &platform)
        .expect_err("injected create failure must fail add_display");
    assert!(matches!(err2, EngineError::Platform(_)));
    assert_eq!(engine.free_slot_count(), MAX_MONITORS);
    assert_eq!(platform.depart_monitor_calls(), 1);

    // WHY: Subsequent healthy arrival after faults must succeed in slot 0.
    platform.set_fail_create_monitor(false);
    platform.set_fail_arrive_monitor(false);
    let ok = engine
        .add_display(FileKey(1), &req, &platform)
        .expect("healthy add_display after rollback should reuse slot 0");
    assert_eq!(ok.connector_index, 0);
    assert_eq!(engine.active_display_count(), 1);
}

#[test]
fn gpu_switch_circuit_breaker_caps_abandon_attempts_and_records_failure() {
    let engine = VddEngine::new();
    let platform = FakeIddPlatform::new();
    let initial_luid = AdapterLuid::new(0x100, 0);
    let bad_luid = AdapterLuid::new(0xDEAD, 1);
    let good_luid = AdapterLuid::new(0xBEEF, 2);

    assert_eq!(
        engine.on_assign_swapchain(initial_luid),
        SwapchainAssignAction::Proceed
    );

    engine
        .request_gpu_switch(bad_luid, &platform)
        .expect("request_gpu_switch records pending switch");
    assert_eq!(platform.current_render_adapter(), bad_luid);
    assert!(engine.pending_gpu_switch().is_some());

    // WHY: Simulate DWM repeatedly offering the old adapter LUID despite SetRenderAdapter.
    assert_eq!(
        engine.on_assign_swapchain(initial_luid),
        SwapchainAssignAction::AbandonSwapChain
    );
    assert_eq!(
        engine.on_assign_swapchain(initial_luid),
        SwapchainAssignAction::AbandonSwapChain
    );
    assert_eq!(
        engine.on_assign_swapchain(initial_luid),
        SwapchainAssignAction::Proceed
    );

    assert!(engine.pending_gpu_switch().is_none());
    assert_eq!(
        engine.last_gpu_switch_error(),
        Some(GpuSwitchError::MaxAttemptsExceeded {
            target_luid: bad_luid,
            attempts: 2,
        })
    );
    assert_eq!(engine.get_status(&platform).last_gpu_switch_failed, 1);

    // WHY: A subsequent valid GPU switch clears the latched failure status upon completion.
    engine
        .request_gpu_switch(good_luid, &platform)
        .expect("new valid GPU switch should succeed");
    assert_eq!(
        engine.on_assign_swapchain(good_luid),
        SwapchainAssignAction::Proceed
    );
    assert!(engine.pending_gpu_switch().is_none());
    assert!(engine.last_gpu_switch_error().is_none());
    assert_eq!(engine.get_status(&platform).last_gpu_switch_failed, 0);
    assert_eq!(
        engine.get_status(&platform).render_adapter_luid_low,
        good_luid.low
    );
}

#[test]
fn leased_monitor_renews_and_expires_via_monotonic_clock() {
    let engine = VddEngine::new();
    let platform = FakeIddPlatform::new();
    let guid = [0x33; 16];

    let mut req = AddDisplayRequest::new(
        guid,
        3840,
        2160,
        60,
        1,
        HdrProfile::Sdr,
        OwnershipMode::Leased,
    );
    req.lease_ttl_secs = 60;

    let resp = engine.add_display(FileKey(10), &req, &platform).unwrap();
    assert_eq!(resp.created_new, 1);

    // WHY: Closing the originating handle must NOT remove a Leased display.
    assert_eq!(engine.on_file_cleanup(FileKey(10), &platform).unwrap(), 0);
    assert_eq!(engine.active_display_count(), 1);

    platform.advance_ms(30_000);
    assert_eq!(engine.reap_expired_leases(&platform).unwrap(), 0);
    assert_eq!(engine.active_display_count(), 1);

    let renew = RenewLeaseRequest::new(guid, 60);
    engine.renew_lease(FileKey(11), &renew, &platform).unwrap();

    // WHY: Advance past original 60s expiry (now at 75s), still valid because renewed to 90s.
    platform.advance_ms(45_000);
    assert_eq!(engine.reap_expired_leases(&platform).unwrap(), 0);
    assert_eq!(engine.active_display_count(), 1);

    // WHY: Advance past renewed 90s expiry (now at 95s); reaper departs it cleanly.
    platform.advance_ms(20_000);
    assert_eq!(
        engine.renew_lease(FileKey(11), &renew, &platform),
        Err(EngineError::LeaseExpired)
    );
    assert_eq!(engine.reap_expired_leases(&platform).unwrap(), 1);
    assert_eq!(engine.active_display_count(), 0);
    assert_eq!(engine.free_slot_count(), MAX_MONITORS);
    assert_eq!(platform.arrived_monitor_count(), 0);
}

#[test]
fn d0_entry_exit_initializes_adapter_strictly_once() {
    let engine = VddEngine::new();
    let platform = FakeIddPlatform::new();

    assert_eq!(engine.adapter_state(), AdapterState::Uninitialized);

    platform.set_fail_init_adapter(true);
    let err = engine
        .on_d0_entry(&platform)
        .expect_err("failed init must propagate error");
    assert!(matches!(err, EngineError::Platform(_)));
    assert_eq!(engine.adapter_state(), AdapterState::Uninitialized);

    platform.set_fail_init_adapter(false);
    platform.advance_ms(1_000);
    assert!(engine.on_d0_entry(&platform).unwrap());
    assert_eq!(engine.adapter_state(), AdapterState::Ready);
    assert_eq!(platform.init_adapter_calls(), 2);

    assert_eq!(engine.on_d0_exit(), AdapterState::Suspended);
    assert!(!engine.on_d0_entry(&platform).unwrap());
    assert_eq!(engine.adapter_state(), AdapterState::Ready);
    assert_eq!(platform.init_adapter_calls(), 2);

    assert!(!engine.should_init_adapter());
    platform.advance_ms(5_000);
    let status = engine.get_status(&platform);
    assert_eq!(status.adapter_ready, 1);
    assert_eq!(status.uptime_secs, 5);
}

#[test]
fn should_init_adapter_and_on_adapter_init_finished_transition_cleanly() {
    let engine = VddEngine::new();
    assert_eq!(engine.adapter_state(), AdapterState::Uninitialized);
    assert!(engine.should_init_adapter());
    assert_eq!(engine.adapter_state(), AdapterState::Initializing);
    assert!(!engine.should_init_adapter());

    engine.on_adapter_init_finished(false);
    assert_eq!(engine.adapter_state(), AdapterState::Uninitialized);

    assert!(engine.should_init_adapter());
    assert_eq!(engine.adapter_state(), AdapterState::Initializing);
    engine.on_adapter_init_finished(true);
    assert_eq!(engine.adapter_state(), AdapterState::Ready);
    assert!(!engine.should_init_adapter());
}

struct OverlappingGpuPlatform {
    slow_luid: AdapterLuid,
    slow_entered: Arc<Barrier>,
    slow_release: Arc<Barrier>,
}

impl IddPlatform for OverlappingGpuPlatform {
    fn create_monitor(
        &self,
        _slot: ConnectorSlot,
        _monitor_guid: [u8; 16],
        _edid: &[u8],
    ) -> Result<MonitorHandle, PlatformError> {
        Ok(MonitorHandle(1))
    }

    fn arrive_monitor(
        &self,
        _handle: MonitorHandle,
        _slot: ConnectorSlot,
        _monitor_guid: [u8; 16],
    ) -> Result<MonitorArrivalResult, PlatformError> {
        Ok(MonitorArrivalResult {
            adapter_luid: AdapterLuid::ZERO,
            target_id: 1,
        })
    }

    fn depart_monitor(&self, _handle: MonitorHandle) -> Result<(), PlatformError> {
        Ok(())
    }

    fn set_render_adapter(&self, luid: AdapterLuid) -> Result<(), PlatformError> {
        if luid == self.slow_luid {
            self.slow_entered.wait();
            self.slow_release.wait();
            return Err(PlatformError::SetRenderAdapterFailed {
                low: luid.low,
                high: luid.high,
                code: -1,
            });
        }
        Ok(())
    }

    fn now_monotonic_ms(&self) -> u64 {
        0
    }
}

#[test]
fn overlapping_gpu_switch_failure_does_not_clobber_newer_request() {
    let engine = Arc::new(VddEngine::new());
    let slow_luid = AdapterLuid::new(0x1111, 0);
    let newer_luid = AdapterLuid::new(0x2222, 0);
    let slow_entered = Arc::new(Barrier::new(2));
    let slow_release = Arc::new(Barrier::new(2));

    let platform = Arc::new(OverlappingGpuPlatform {
        slow_luid,
        slow_entered: Arc::clone(&slow_entered),
        slow_release: Arc::clone(&slow_release),
    });

    let eng1 = Arc::clone(&engine);
    let plat1 = Arc::clone(&platform);
    let t1 = thread::spawn(move || eng1.request_gpu_switch(slow_luid, plat1.as_ref()));

    slow_entered.wait();
    engine
        .request_gpu_switch(newer_luid, platform.as_ref())
        .expect("newer switch request must succeed");
    slow_release.wait();

    let t1_res = t1.join().unwrap();
    assert!(matches!(t1_res, Err(EngineError::Platform(_))));

    let pending = engine
        .pending_gpu_switch()
        .expect("newer pending switch must remain intact");
    assert_eq!(pending.target_luid, newer_luid);
    assert!(engine.last_gpu_switch_error().is_none());
    assert_eq!(
        engine.get_status(platform.as_ref()).last_gpu_switch_failed,
        0
    );
}

#[test]
fn total_departures_never_exceeds_total_arrivals_on_failed_depart_or_arrive() {
    let engine = VddEngine::new();
    let platform = FakeIddPlatform::new();

    platform.set_fail_arrive_monitor(true);
    let req = AddDisplayRequest::new(
        [0x44; 16],
        1920,
        1080,
        60,
        1,
        HdrProfile::Sdr,
        OwnershipMode::HandleBound,
    );
    assert!(engine.add_display(FileKey(1), &req, &platform).is_err());

    let status_after_arrive_fail = engine.get_status(&platform);
    assert_eq!(status_after_arrive_fail.total_arrivals, 0);
    assert_eq!(status_after_arrive_fail.total_departures, 0);

    platform.set_fail_arrive_monitor(false);
    engine.add_display(FileKey(1), &req, &platform).unwrap();
    platform.set_fail_depart_monitor(true);
    assert!(engine.on_file_cleanup(FileKey(1), &platform).is_err());

    let status_after_depart_fail = engine.get_status(&platform);
    assert_eq!(status_after_depart_fail.total_arrivals, 1);
    assert_eq!(status_after_depart_fail.total_departures, 0);
}
