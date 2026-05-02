use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::Notify;

struct WakeLockInner {
	count: AtomicUsize,
	notify_release: Notify,
	notify_acquire: Notify,
	/// Wallclock instant of the most recent 1→0 transition. `None`
	/// means the lock is either currently held (count > 0) or has
	/// never been acquired in this process. Used by the watchdog to
	/// distinguish "just-released, grace-period covers it" from
	/// "idle long enough that grace_period.rs didn't fire". Replaces
	/// the older check that disconnected on instantaneous `count == 0`
	/// — that races wakeup MQTT publishes and tears down a camera the
	/// operator was about to start using.
	last_release_at: Mutex<Option<Instant>>,
}

#[derive(Clone)]
pub struct WakeLockCounter {
	inner: Arc<WakeLockInner>,
}

pub struct WakeLockGuard {
	inner: Arc<WakeLockInner>,
}

impl WakeLockCounter {
	pub fn new() -> Self {
		Self {
			inner: Arc::new(WakeLockInner {
				count: AtomicUsize::new(0),
				notify_release: Notify::new(),
				notify_acquire: Notify::new(),
				last_release_at: Mutex::new(None),
			}),
		}
	}

	pub fn acquire(&self) -> WakeLockGuard {
		let prev = self.inner.count.fetch_add(1, Ordering::AcqRel);
		if prev == 0 {
			// Transition 0→1. Clear the last-release timestamp — the
			// camera is no longer idle, so any grace window the
			// watchdog was tracking is now invalidated.
			if let Ok(mut last) = self.inner.last_release_at.lock() {
				*last = None;
			}
			// Use notify_one (not notify_waiters) so a permit is stored
			// if the camera run loop has not yet reached its
			// wait_for_acquire().await. notify_waiters drops the edge
			// on the floor when there are zero registered waiters,
			// which would leave an idle-disconnect camera parked
			// forever when acquire races the run loop's is_idle() check.
			self.inner.notify_acquire.notify_one();
		}
		WakeLockGuard {
			inner: Arc::clone(&self.inner),
		}
	}

	pub fn count(&self) -> usize {
		self.inner.count.load(Ordering::Acquire)
	}

	pub fn is_idle(&self) -> bool {
		self.count() == 0
	}

	/// Returns the `Instant` at which the last 1→0 release happened,
	/// or `None` if the lock is currently held / has never been
	/// acquired. The watchdog uses this to gate its disconnect on
	/// "idle for at least grace_period_secs"; without this gate it
	/// races every freshly-arrived MQTT wakeup that lands a few ms
	/// after a probe finishes.
	pub fn idle_since(&self) -> Option<Instant> {
		// Belt-and-braces: only return a timestamp when count is
		// observably 0 right now. A racing acquire would have cleared
		// the timestamp first; we re-check count to avoid handing back
		// a stale `Some(_)` from a prior idle window.
		if !self.is_idle() {
			return None;
		}
		self.inner.last_release_at.lock().ok().and_then(|g| *g)
	}

	/// Wait for the last lock to be released (count transitions 1→0).
	pub async fn notified(&self) {
		self.inner.notify_release.notified().await;
	}

	/// Returns a future that completes when the last lock is released.
	/// The future must be created BEFORE checking is_idle() to avoid
	/// missing notifications (TOCTOU race with Notify).
	pub fn notify_future(&self) -> tokio::sync::futures::Notified<'_> {
		self.inner.notify_release.notified()
	}

	/// Wait for a lock to be acquired (count transitions 0→1).
	/// Used by idle-disconnect cameras to wake up when needed.
	///
	/// Spurious-permit guard: `notify_one` stores a permit if nobody
	/// is waiting at the time of the 0→1 transition (intentional —
	/// closes the TOCTOU race against the run loop's `is_idle` check).
	/// But stored permits from a prior session linger until consumed,
	/// so a freshly-disconnected camera entering this `await` would
	/// otherwise be woken immediately by the stale permit and
	/// reconnect with `count == 0`. The new session would then have
	/// nobody holding it, the watchdog would catch the empty session
	/// on its next tick, and the camera would flap. Re-check
	/// `is_idle` after each notification and park again on a stale
	/// permit; only return when the count is actually held.
	pub async fn wait_for_acquire(&self) {
		loop {
			let notified = self.inner.notify_acquire.notified();
			if !self.is_idle() {
				return;
			}
			notified.await;
			if !self.is_idle() {
				return;
			}
			// Stored permit from a prior session — drop it and park again.
		}
	}
}

impl Default for WakeLockCounter {
	fn default() -> Self {
		Self::new()
	}
}

impl Drop for WakeLockGuard {
	fn drop(&mut self) {
		let prev = self.inner.count.fetch_sub(1, Ordering::AcqRel);
		if prev == 1 {
			// Stamp the release time BEFORE the Notify so a watchdog
			// tick that fires immediately after the release sees the
			// fresh timestamp. Reverse order would let the watchdog
			// observe `is_idle() == true` with `idle_since == None`
			// (or a stale prior timestamp), defeating the grace-window
			// gate.
			if let Ok(mut last) = self.inner.last_release_at.lock() {
				*last = Some(Instant::now());
			}
			// Same TOCTOU concern as acquire: notify_one stores a permit
			// if the grace-period timer has not yet registered, so a
			// release that races a just-spawned grace task still fires.
			self.inner.notify_release.notify_one();
		}
	}
}
