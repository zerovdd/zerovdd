//! Adapter power-state idempotency (`D0` / `D3`) and GPU-switch circuit breaker.

use spin::Mutex;

use crate::platform::{AdapterLuid, AdapterState, IddPlatform, PlatformError};
use crate::EngineError;

/// Default maximum number of swapchain abandonment cycles permitted per GPU switch.
pub const DEFAULT_MAX_GPU_SWITCH_ATTEMPTS: u32 = 2;

/// In-flight render adapter switch awaiting confirmation in `AssignSwapChain`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingGpuSwitch {
    /// Desired target render GPU adapter LUID.
    pub target_luid: AdapterLuid,
    /// Number of swapchain abandonments already issued for this switch request.
    pub attempts: u32,
    /// Maximum swapchain abandonments allowed before tripping the circuit breaker.
    pub max_attempts: u32,
}

/// Recorded cause of the most recent render GPU switch failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuSwitchError {
    /// Target LUID never materialized across `AssignSwapChain` within `max_attempts`.
    MaxAttemptsExceeded {
        /// Requested target adapter LUID.
        target_luid: AdapterLuid,
        /// Number of abandonment attempts issued before tripping.
        attempts: u32,
    },
    /// Platform call to `IddCxAdapterSetRenderAdapter` failed.
    SetRenderAdapterFailed(PlatformError),
}

/// Action returned to `EvtIddCxMonitorAssignSwapChain`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapchainAssignAction {
    /// Proceed with the swapchain on the current D3D11 render adapter.
    Proceed,
    /// Return `STATUS_GRAPHICS_INDIRECT_DISPLAY_ABANDON_SWAPCHAIN` to prompt re-assignment.
    AbandonSwapChain,
}

/// Point-in-time snapshot of adapter power state and render GPU telemetry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuSnapshot {
    /// Current `D0` / `D3` adapter power state.
    pub adapter_state: AdapterState,
    /// Monotonic timestamp (ms) when the adapter first entered `Ready`.
    pub init_time_ms: Option<u64>,
    /// Whether FP16 / HDR10 swapchain capability is enabled.
    pub hdr_capable: bool,
    /// Currently active render GPU adapter LUID.
    pub render_adapter_luid: AdapterLuid,
    /// Active pending GPU switch, if any.
    pub pending_gpu_switch: Option<PendingGpuSwitch>,
    /// `true` if the most recent GPU switch failed or tripped the circuit breaker.
    pub last_gpu_switch_failed: bool,
    /// Detailed error from the most recent failed GPU switch, if any.
    pub last_gpu_switch_error: Option<GpuSwitchError>,
}

struct GpuState {
    adapter_state: AdapterState,
    init_time_ms: Option<u64>,
    hdr_capable: bool,
    render_adapter_luid: AdapterLuid,
    pending_gpu_switch: Option<PendingGpuSwitch>,
    last_gpu_switch_failed: bool,
    last_gpu_switch_error: Option<GpuSwitchError>,
    max_attempts: u32,
    switch_gen: u64,
}

/// Manages adapter `D0` / `D3` idempotency and bounded GPU adapter switching.
pub struct GpuController {
    state: Mutex<GpuState>,
}

impl GpuController {
    /// Creates a new [`GpuController`] with default circuit-breaker limit (`2`).
    #[must_use]
    pub const fn new() -> Self {
        Self::with_max_attempts(DEFAULT_MAX_GPU_SWITCH_ATTEMPTS)
    }

    /// Creates a new [`GpuController`] with a custom `max_attempts` circuit-breaker bound.
    #[must_use]
    pub const fn with_max_attempts(max_attempts: u32) -> Self {
        Self {
            state: Mutex::new(GpuState {
                adapter_state: AdapterState::Uninitialized,
                init_time_ms: None,
                hdr_capable: false,
                render_adapter_luid: AdapterLuid::ZERO,
                pending_gpu_switch: None,
                last_gpu_switch_failed: false,
                last_gpu_switch_error: None,
                max_attempts,
                switch_gen: 0,
            }),
        }
    }

    /// Transitions `Uninitialized -> Initializing` and returns `true` strictly once.
    ///
    /// Subsequent calls while `Initializing`, `Ready`, or `Suspended` return `false`
    /// so sleep/resume (`D3 -> D0`) never re-invokes `IddCxAdapterInitAsync`.
    pub fn should_init_adapter(&self) -> bool {
        let mut guard = self.state.lock();
        match guard.adapter_state {
            AdapterState::Uninitialized => {
                guard.adapter_state = AdapterState::Initializing;
                true
            }
            AdapterState::Initializing | AdapterState::Ready | AdapterState::Suspended => false,
        }
    }

    /// Completes an adapter initialization begun via [`Self::should_init_adapter`],
    /// moving `Initializing -> Ready` on success or `Initializing -> Uninitialized` on failure.
    pub fn on_adapter_init_finished(&self, succeeded: bool) {
        let mut guard = self.state.lock();
        if guard.adapter_state == AdapterState::Initializing {
            guard.adapter_state = if succeeded {
                AdapterState::Ready
            } else {
                AdapterState::Uninitialized
            };
        }
    }

    /// Handles `EvtDeviceD0Entry` idempotently.
    ///
    /// Returns `Ok(true)` if [`IddPlatform::init_adapter`] was invoked on first boot,
    /// or `Ok(false)` if resuming from `Suspended` (`D3 -> D0`) or already `Ready`.
    pub fn on_d0_entry(&self, platform: &dyn IddPlatform) -> Result<bool, EngineError> {
        let needs_init = {
            let mut guard = self.state.lock();
            match guard.adapter_state {
                AdapterState::Uninitialized => {
                    guard.adapter_state = AdapterState::Initializing;
                    true
                }
                AdapterState::Suspended => {
                    guard.adapter_state = AdapterState::Ready;
                    false
                }
                AdapterState::Initializing | AdapterState::Ready => false,
            }
        };

        if !needs_init {
            return Ok(false);
        }

        match platform.init_adapter() {
            Ok(()) => {
                let now_ms = platform.now_monotonic_ms();
                let mut guard = self.state.lock();
                guard.adapter_state = AdapterState::Ready;
                if guard.init_time_ms.is_none() {
                    guard.init_time_ms = Some(now_ms);
                }
                Ok(true)
            }
            Err(err) => {
                let mut guard = self.state.lock();
                guard.adapter_state = AdapterState::Uninitialized;
                Err(EngineError::Platform(err))
            }
        }
    }

    /// Handles `EvtDeviceD0Exit` (`D0 -> D3`), transitioning `Ready -> Suspended`.
    pub fn on_d0_exit(&self) -> AdapterState {
        let mut guard = self.state.lock();
        if guard.adapter_state == AdapterState::Ready {
            guard.adapter_state = AdapterState::Suspended;
        }
        guard.adapter_state
    }

    /// Updates whether the adapter advertises FP16 / HDR10 swapchain support.
    pub fn set_hdr_capable(&self, capable: bool) {
        self.state.lock().hdr_capable = capable;
    }

    /// Records a pending GPU switch and calls [`IddPlatform::set_render_adapter`].
    pub fn request_gpu_switch(
        &self,
        target_luid: AdapterLuid,
        platform: &dyn IddPlatform,
    ) -> Result<(), EngineError> {
        let my_gen = {
            let mut guard = self.state.lock();
            guard.switch_gen = guard.switch_gen.saturating_add(1);
            let max_attempts = guard.max_attempts;
            guard.pending_gpu_switch = Some(PendingGpuSwitch {
                target_luid,
                attempts: 0,
                max_attempts,
            });
            guard.last_gpu_switch_failed = false;
            guard.last_gpu_switch_error = None;
            guard.switch_gen
        };

        if let Err(err) = platform.set_render_adapter(target_luid) {
            let mut guard = self.state.lock();
            // WHY: Only record failure if no newer GPU switch superseded this in-flight request.
            if guard.switch_gen == my_gen {
                guard.pending_gpu_switch = None;
                guard.last_gpu_switch_failed = true;
                guard.last_gpu_switch_error =
                    Some(GpuSwitchError::SetRenderAdapterFailed(err.clone()));
            }
            return Err(EngineError::Platform(err));
        }

        Ok(())
    }

    /// Evaluates whether `AssignSwapChain` must abandon the current swapchain to complete
    /// a pending render GPU switch, or trip the circuit breaker once `attempts > max_attempts`.
    pub fn on_assign_swapchain(&self, current_adapter_luid: AdapterLuid) -> SwapchainAssignAction {
        let mut guard = self.state.lock();
        let Some(mut pending) = guard.pending_gpu_switch else {
            if current_adapter_luid != AdapterLuid::ZERO {
                guard.render_adapter_luid = current_adapter_luid;
            }
            return SwapchainAssignAction::Proceed;
        };

        if pending.target_luid == AdapterLuid::ZERO || current_adapter_luid == pending.target_luid {
            guard.render_adapter_luid = current_adapter_luid;
            guard.pending_gpu_switch = None;
            guard.last_gpu_switch_failed = false;
            guard.last_gpu_switch_error = None;
            return SwapchainAssignAction::Proceed;
        }

        pending.attempts = pending.attempts.saturating_add(1);
        if pending.attempts > pending.max_attempts {
            guard.pending_gpu_switch = None;
            guard.last_gpu_switch_failed = true;
            guard.last_gpu_switch_error = Some(GpuSwitchError::MaxAttemptsExceeded {
                target_luid: pending.target_luid,
                attempts: pending.max_attempts,
            });
            if current_adapter_luid != AdapterLuid::ZERO {
                guard.render_adapter_luid = current_adapter_luid;
            }
            return SwapchainAssignAction::Proceed;
        }

        guard.pending_gpu_switch = Some(pending);
        SwapchainAssignAction::AbandonSwapChain
    }

    /// Returns a consistent point-in-time snapshot of GPU and adapter state.
    #[must_use]
    pub fn snapshot(&self) -> GpuSnapshot {
        let guard = self.state.lock();
        GpuSnapshot {
            adapter_state: guard.adapter_state,
            init_time_ms: guard.init_time_ms,
            hdr_capable: guard.hdr_capable,
            render_adapter_luid: guard.render_adapter_luid,
            pending_gpu_switch: guard.pending_gpu_switch,
            last_gpu_switch_failed: guard.last_gpu_switch_failed,
            last_gpu_switch_error: guard.last_gpu_switch_error.clone(),
        }
    }

    /// Returns the current [`AdapterState`].
    #[must_use]
    pub fn adapter_state(&self) -> AdapterState {
        self.state.lock().adapter_state
    }

    /// Returns the currently pending GPU switch, if one is active.
    #[must_use]
    pub fn pending_gpu_switch(&self) -> Option<PendingGpuSwitch> {
        self.state.lock().pending_gpu_switch
    }

    /// Returns the most recent GPU switch error, if the last switch failed.
    #[must_use]
    pub fn last_gpu_switch_error(&self) -> Option<GpuSwitchError> {
        self.state.lock().last_gpu_switch_error.clone()
    }
}

impl Default for GpuController {
    fn default() -> Self {
        Self::new()
    }
}
