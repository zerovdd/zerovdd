use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use zerovdd_edid::validate_custom_edid;
use zerovdd_engine::{FakeIddPlatform, FileKey, MonitorQueryKey, VddEngine};
use zerovdd_modes::VideoSignalRole;
use zerovdd_proto::types::{AddDisplayRequest, HdrProfile, OwnershipMode};

#[test]
fn synchronous_reentrancy_in_arrive_monitor_never_deadlocks_and_matches_target_modes() {
    let engine = Arc::new(VddEngine::new());
    let platform = FakeIddPlatform::new();
    let hook_calls = Arc::new(AtomicUsize::new(0));

    let hook_engine = Arc::clone(&engine);
    let hook_counter = Arc::clone(&hook_calls);
    platform.set_arrive_hook(move |ctx| {
        hook_counter.fetch_add(1, Ordering::SeqCst);

        let by_handle = hook_engine
            .on_parse_monitor_description(MonitorQueryKey::MonitorHandle(ctx.monitor_handle))
            .expect("lookup by monitor handle must succeed during arrival");
        let by_slot = hook_engine
            .on_parse_monitor_description(MonitorQueryKey::ConnectorSlot(ctx.connector_slot))
            .expect("lookup by connector slot must succeed during arrival");
        let by_guid = hook_engine
            .on_parse_monitor_description(MonitorQueryKey::MonitorGuid(ctx.monitor_guid))
            .expect("lookup by monitor GUID must succeed during arrival");
        let by_edid = hook_engine
            .on_parse_monitor_description(MonitorQueryKey::Edid(&ctx.edid))
            .expect("lookup by EDID bytes must succeed during arrival");

        assert_eq!(by_handle.edid, ctx.edid);
        assert_eq!(by_slot.edid, ctx.edid);
        assert_eq!(by_guid.edid, ctx.edid);
        assert_eq!(by_edid.edid, ctx.edid);

        let edid_summary = validate_custom_edid(&by_handle.edid).expect("synthesized EDID valid");
        assert_eq!(edid_summary.extension_blocks, 1);

        let target = hook_engine
            .on_query_target_modes(MonitorQueryKey::MonitorHandle(ctx.monitor_handle))
            .expect("target modes lookup must succeed during arrival");

        assert!(!by_handle.monitor_signals.is_empty());
        assert_eq!(by_handle.monitor_signals.len(), target.target_signals.len());
        assert!(target.planned_modes[0].is_preferred);
        assert_eq!(target.planned_modes[0].width, 2560);
        assert_eq!(target.planned_modes[0].height, 1600);

        for (mon_sig, tgt_sig) in by_handle
            .monitor_signals
            .iter()
            .zip(target.target_signals.iter())
        {
            assert_eq!(mon_sig.vsync_freq_divider, 0);
            assert_eq!(tgt_sig.vsync_freq_divider, 1);
            assert_eq!(
                *mon_sig,
                tgt_sig.with_role(VideoSignalRole::MonitorDescription)
            );
        }

        // WHY: Verify scaled resolution variants (e.g. 50% = 1280x800, 75% = 1920x1200) exist.
        assert!(target
            .planned_modes
            .iter()
            .any(|m| m.width == 1280 && m.height == 800));
        assert!(target
            .planned_modes
            .iter()
            .any(|m| m.width == 1920 && m.height == 1200));

        Ok(())
    });

    let mut req = AddDisplayRequest::new(
        [0x11; 16],
        2560,
        1600,
        90,
        1,
        HdrProfile::Hdr10,
        OwnershipMode::HandleBound,
    );
    req.set_client_name("Deck-OLED").unwrap();

    let resp = engine
        .add_display(FileKey(7), &req, &platform)
        .expect("add_display should succeed with synchronous re-entrant parse");
    assert_eq!(resp.created_new, 1);
    assert_eq!(hook_calls.load(Ordering::SeqCst), 1);
    assert_eq!(engine.active_display_count(), 1);
}
