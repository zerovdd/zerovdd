//! Lease expiration, renewal, and per-handle cleanup watchdog.

use zerovdd_proto::types::RenewLeaseRequest;
use zerovdd_proto::validation::Validate;

use crate::platform::{FileKey, IddPlatform};
use crate::registry::{MonitorRegistry, RenewLeaseOutcome};
use crate::EngineError;

/// Extends the TTL of an active leased virtual monitor using the monotonic clock.
pub fn renew_lease(
    registry: &MonitorRegistry,
    _file_key: FileKey,
    req: &RenewLeaseRequest,
    platform: &dyn IddPlatform,
) -> Result<(), EngineError> {
    req.validate()?;
    let now_ms = platform.now_monotonic_ms();
    match registry.renew_lease(req.monitor_guid, now_ms, req.lease_ttl_secs) {
        RenewLeaseOutcome::Renewed => Ok(()),
        RenewLeaseOutcome::NotFound | RenewLeaseOutcome::InFlight => {
            Err(EngineError::MonitorNotFound)
        }
        RenewLeaseOutcome::NotLeased => Err(EngineError::NotLeased),
        RenewLeaseOutcome::Expired => Err(EngineError::LeaseExpired),
    }
}

/// Departs all leased virtual monitors whose TTL has elapsed as of `now_monotonic_ms()`.
///
/// Never holds the registry mutex across [`IddPlatform::depart_monitor`].
pub fn reap_expired_leases(
    registry: &MonitorRegistry,
    platform: &dyn IddPlatform,
) -> Result<usize, EngineError> {
    let now_ms = platform.now_monotonic_ms();
    let expired = registry.collect_expired_leases(now_ms);
    let mut departed_count = 0usize;
    let mut first_error = None;

    for target in expired {
        let departed_ok = match platform.depart_monitor(target.handle) {
            Ok(()) => {
                departed_count += 1;
                true
            }
            Err(err) => {
                if first_error.is_none() {
                    first_error = Some(err);
                }
                false
            }
        };
        registry.remove_slot(target.slot, target.instance_id, departed_ok);
    }

    match first_error {
        Some(err) => Err(EngineError::Platform(err)),
        None => Ok(departed_count),
    }
}

/// Departs all `HandleBound` monitors owned by `file_key` when its handle closes.
///
/// Leaves other handles' monitors and all `Leased` monitors untouched. Never holds
/// the registry mutex across [`IddPlatform::depart_monitor`].
pub fn on_file_cleanup(
    registry: &MonitorRegistry,
    file_key: FileKey,
    platform: &dyn IddPlatform,
) -> Result<usize, EngineError> {
    let mut departed_count = 0usize;
    let mut first_error = None;

    loop {
        let batch = registry.begin_cleanup_for_file(file_key);
        if batch.to_depart.is_empty() && batch.wait_instances.is_empty() {
            break;
        }

        for target in batch.to_depart {
            let departed_ok = match platform.depart_monitor(target.handle) {
                Ok(()) => {
                    departed_count += 1;
                    true
                }
                Err(err) => {
                    if first_error.is_none() {
                        first_error = Some(err);
                    }
                    false
                }
            };
            registry.remove_slot(target.slot, target.instance_id, departed_ok);
        }

        for (slot, instance_id) in batch.wait_instances {
            while registry.is_instance_present(slot, instance_id) {
                core::hint::spin_loop();
            }
        }
    }

    match first_error {
        Some(err) => Err(EngineError::Platform(err)),
        None => Ok(departed_count),
    }
}
