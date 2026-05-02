use std::time::Duration;

use crate::wake_lock::WakeLockCounter;

// ── GracePeriod ──────────────────────────────────────────────────────

pub struct GracePeriod {
	wake_lock: WakeLockCounter,
	duration: Duration,
}

impl GracePeriod {
	pub fn new(wake_lock: WakeLockCounter, duration: Duration) -> Self {
		Self {
			wake_lock,
			duration,
		}
	}

	/// Runs the grace period loop. Returns when the grace period expires
	/// (wake lock is observed idle at the deadline tick).
	///
	/// Semantics: this is a "check at deadline" countdown, not a
	/// "sustained idle" timer. A brief acquire+release pair that both
	/// happen inside the `duration` sleep window is invisible — by the
	/// time the sleep ends, the count is back to zero and we return.
	/// Realistic consumers (the motion listener holds for
	/// `motion_wake_hold_secs` ≈ 30 s, much longer than the typical
	/// `idle_disconnect_timeout_secs`) don't generate sub-window
	/// flicker, so the deadline-only check matches the operator's
	/// "camera was idle long enough to disconnect" intent.
	pub async fn run(self) {
		loop {
			// Register for notification BEFORE checking state
			// to avoid TOCTOU race with Notify
			let notification = self.wake_lock.notify_future();

			if !self.wake_lock.is_idle() {
				notification.await;
			}

			// Start countdown
			tokio::time::sleep(self.duration).await;

			// Check if still idle after sleep. A brief acquire+release
			// inside the sleep window is intentionally not visible here.
			if self.wake_lock.is_idle() {
				return; // Grace period expired — caller should disconnect
			}
			// Otherwise, lock is currently held — loop back to wait for
			// the next release before re-arming the countdown.
		}
	}
}
