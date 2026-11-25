use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use zerovdd_engine::{
    AdapterLuid, ConnectorSlot, EngineError, FakeIddPlatform, FileKey, IddPlatform,
    MonitorArrivalResult, MonitorHandle, MonitorQueryKey, PlatformError, VddEngine, MAX_MONITORS,
};
use zerovdd_proto::types::{AddDisplayRequest, HdrProfile, OwnershipMode, RemoveDisplayRequest};

#[test]
fn eight_thread_barrier_soak_across_thousand_plus_operations_leaks_zero_slots() {
    const THREADS: usize = 8;
    const ITERS_PER_THREAD: usize = 160;

    let engine = Arc::new(VddEngine::new());
    let platform = Arc::new(FakeIddPlatform::new());
    let total_ops = Arc::new(AtomicUsize::new(0));

    let hook_engine = Arc::clone(&engine);
    platform.set_arrive_hook(move |ctx| {
        let _ = hook_engine
            .on_parse_monitor_description(MonitorQueryKey::MonitorHandle(ctx.monitor_handle));
        let _ =
            hook_engine.on_query_target_modes(MonitorQueryKey::ConnectorSlot(ctx.connector_slot));
        Ok(())
    });

    let start_barrier = Arc::new(Barrier::new(THREADS));
    let end_barrier = Arc::new(Barrier::new(THREADS));

    let mut handles = Vec::with_capacity(THREADS);
    for tid in 0..THREADS {
        let eng = Arc::clone(&engine);
        let plat = Arc::clone(&platform);
        let start = Arc::clone(&start_barrier);
        let end = Arc::clone(&end_barrier);
        let ops = Arc::clone(&total_ops);

        handles.push(thread::spawn(move || {
            let file_key = FileKey((tid as u64) + 1);
            start.wait();

            for iter in 0..ITERS_PER_THREAD {
                let mut guid = [0u8; 16];
                guid[0] = (tid as u8) + 1;
                // WHY: Alternate between per-thread unique GUIDs and shared GUIDs to exercise
                // both slot turnover and cross-thread contention.
                guid[1] = if iter % 5 == 0 {
                    0xEE
                } else {
                    (iter % 4) as u8
                };

                let req = AddDisplayRequest::new(
                    guid,
                    1920,
                    1080,
                    60,
                    1,
                    HdrProfile::Sdr,
                    OwnershipMode::HandleBound,
                );

                match eng.add_display(file_key, &req, plat.as_ref()) {
                    Ok(resp) => {
                        let _ =
                            eng.on_parse_monitor_description(MonitorQueryKey::MonitorGuid(guid));
                        let _ = eng.on_query_target_modes(MonitorQueryKey::MonitorGuid(guid));
                        let _ = eng.list_displays(plat.as_ref());
                        if iter % 3 == 0 {
                            let _ = eng.remove_display(
                                file_key,
                                &RemoveDisplayRequest::by_target_id(resp.target_id),
                                plat.as_ref(),
                            );
                        } else if iter % 7 == 0 {
                            let _ = eng.on_file_cleanup(file_key, plat.as_ref());
                        } else {
                            let _ = eng.remove_display(
                                file_key,
                                &RemoveDisplayRequest::by_guid(guid),
                                plat.as_ref(),
                            );
                        }
                    }
                    Err(
                        EngineError::MonitorConflict
                        | EngineError::NoFreeConnectorSlots { .. }
                        | EngineError::CancelledDuringArrival,
                    ) => {}
                    Err(other) => panic!("unexpected engine error during soak: {other:?}"),
                }

                ops.fetch_add(1, Ordering::Relaxed);
            }

            eng.on_file_cleanup(file_key, plat.as_ref())
                .expect("final file cleanup must succeed");
            end.wait();
        }));
    }

    for handle in handles {
        handle.join().expect("soak worker thread must not panic");
    }

    assert_eq!(
        total_ops.load(Ordering::Relaxed),
        THREADS * ITERS_PER_THREAD
    );
    assert_eq!(engine.active_display_count(), 0);
    assert_eq!(engine.free_slot_count(), MAX_MONITORS);
    assert_eq!(engine.list_displays(platform.as_ref()).count, 0);
    assert_eq!(platform.created_monitor_count(), 0);
    assert_eq!(platform.arrived_monitor_count(), 0);
}

#[test]
fn concurrent_cleanup_during_pending_arrival_cancels_cleanly_without_leak() {
    let engine = Arc::new(VddEngine::new());
    let platform = Arc::new(FakeIddPlatform::new());
    let file_key = FileKey(55);
    let guid = [0x99; 16];

    let in_arrive = Arc::new(Barrier::new(2));
    let release_arrive = Arc::new(Barrier::new(2));

    let in_arrive_hook = Arc::clone(&in_arrive);
    let release_arrive_hook = Arc::clone(&release_arrive);
    platform.set_arrive_hook(move |_| {
        in_arrive_hook.wait();
        release_arrive_hook.wait();
        Ok(())
    });

    let eng_add = Arc::clone(&engine);
    let plat_add = Arc::clone(&platform);
    let add_thread = thread::spawn(move || {
        let req = AddDisplayRequest::new(
            guid,
            1920,
            1080,
            60,
            1,
            HdrProfile::Sdr,
            OwnershipMode::HandleBound,
        );
        eng_add.add_display(file_key, &req, plat_add.as_ref())
    });

    in_arrive.wait();
    let eng_cleanup = Arc::clone(&engine);
    let plat_cleanup = Arc::clone(&platform);
    let cleanup_thread =
        thread::spawn(move || eng_cleanup.on_file_cleanup(file_key, plat_cleanup.as_ref()));

    // WHY: Unblock arrive_monitor so the pending monitor promotes/cancels and departs cleanly.
    release_arrive.wait();

    let add_res = add_thread.join().unwrap();
    let cleanup_res = cleanup_thread.join().unwrap();

    assert!(cleanup_res.is_ok());
    assert!(matches!(
        add_res,
        Err(EngineError::CancelledDuringArrival) | Ok(_)
    ));

    // WHY: If cleanup ran before Pending promotion completed, CancelledDuringArrival tore it down;
    // ensure calling cleanup after join leaves zero active monitors either way.
    engine.on_file_cleanup(file_key, platform.as_ref()).unwrap();
    assert_eq!(engine.active_display_count(), 0);
    assert_eq!(engine.free_slot_count(), MAX_MONITORS);
    assert_eq!(platform.created_monitor_count(), 0);
    assert_eq!(platform.arrived_monitor_count(), 0);
}

struct ArriveFailureDepartBarrierPlatform {
    fail_slot: ConnectorSlot,
    next_handle: AtomicU64,
    next_target: AtomicU64,
    depart_entered: Arc<Barrier>,
    depart_release: Arc<Barrier>,
}

impl IddPlatform for ArriveFailureDepartBarrierPlatform {
    fn create_monitor(
        &self,
        _slot: ConnectorSlot,
        _monitor_guid: [u8; 16],
        _edid: &[u8],
    ) -> Result<MonitorHandle, PlatformError> {
        Ok(MonitorHandle(
            self.next_handle.fetch_add(1, Ordering::SeqCst),
        ))
    }

    fn arrive_monitor(
        &self,
        _handle: MonitorHandle,
        slot: ConnectorSlot,
        _monitor_guid: [u8; 16],
    ) -> Result<MonitorArrivalResult, PlatformError> {
        if slot == self.fail_slot {
            return Err(PlatformError::ArriveMonitorFailed {
                slot: slot.0,
                code: -1,
            });
        }
        let target_id = self.next_target.fetch_add(1, Ordering::SeqCst) as u32;
        Ok(MonitorArrivalResult {
            adapter_luid: AdapterLuid::new(0x1000, 0),
            target_id,
        })
    }

    fn depart_monitor(&self, handle: MonitorHandle) -> Result<(), PlatformError> {
        if handle == MonitorHandle(1) {
            self.depart_entered.wait();
            self.depart_release.wait();
        }
        Ok(())
    }

    fn set_render_adapter(&self, _luid: AdapterLuid) -> Result<(), PlatformError> {
        Ok(())
    }

    fn now_monotonic_ms(&self) -> u64 {
        0
    }
}

#[test]
fn arrive_failure_keeps_slot_departing_until_depart_monitor_completes() {
    let engine = Arc::new(VddEngine::new());
    let depart_entered = Arc::new(Barrier::new(2));
    let depart_release = Arc::new(Barrier::new(2));
    let platform = Arc::new(ArriveFailureDepartBarrierPlatform {
        fail_slot: ConnectorSlot(0),
        next_handle: AtomicU64::new(1),
        next_target: AtomicU64::new(100),
        depart_entered: Arc::clone(&depart_entered),
        depart_release: Arc::clone(&depart_release),
    });

    let eng1 = Arc::clone(&engine);
    let plat1 = Arc::clone(&platform);
    let failing_add = thread::spawn(move || {
        let req = AddDisplayRequest::new(
            [0xA1; 16],
            1920,
            1080,
            60,
            1,
            HdrProfile::Sdr,
            OwnershipMode::HandleBound,
        );
        eng1.add_display(FileKey(1), &req, plat1.as_ref())
    });

    depart_entered.wait();

    // WHY: While depart_monitor is blocked for failing slot 0, a concurrent add_display must
    // allocate slot 1 instead of reusing slot 0, and on_file_cleanup must wait until teardown ends.
    let req2 = AddDisplayRequest::new(
        [0xB2; 16],
        1920,
        1080,
        60,
        1,
        HdrProfile::Sdr,
        OwnershipMode::HandleBound,
    );
    let resp2 = engine
        .add_display(FileKey(2), &req2, platform.as_ref())
        .expect("concurrent add on healthy slot must succeed");
    assert_eq!(resp2.connector_index, 1);

    let cleanup_done = Arc::new(AtomicBool::new(false));
    let cleanup_started = Arc::new(Barrier::new(2));
    let eng_clean = Arc::clone(&engine);
    let plat_clean = Arc::clone(&platform);
    let done_flag = Arc::clone(&cleanup_done);
    let started_barrier = Arc::clone(&cleanup_started);
    let cleanup_thread = thread::spawn(move || {
        started_barrier.wait();
        let res = eng_clean.on_file_cleanup(FileKey(1), plat_clean.as_ref());
        done_flag.store(true, Ordering::SeqCst);
        res
    });

    cleanup_started.wait();
    assert!(!cleanup_done.load(Ordering::SeqCst));

    depart_release.wait();

    assert!(matches!(
        failing_add.join().unwrap(),
        Err(EngineError::Platform(_))
    ));
    assert_eq!(cleanup_thread.join().unwrap().unwrap(), 0);
    assert!(cleanup_done.load(Ordering::SeqCst));

    let status = engine.get_status(platform.as_ref());
    assert_eq!(status.total_arrivals, 1);
    assert_eq!(status.total_departures, 0);
    assert_eq!(engine.active_display_count(), 1);
    assert_eq!(engine.free_slot_count(), MAX_MONITORS - 1);
}
