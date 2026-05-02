//! Lightweight service supervisor for `main.rs`.
//!
//! Centralises the spawn → cancel → join-with-timeout pattern that
//! every long-running task in the binary uses. Each registered
//! service:
//!
//! 1. Receives a clone of the supervisor's [`CancellationToken`].
//! 2. Runs until that token fires (or it returns on its own).
//! 3. Is named, so shutdown logs identify which service exited.
//!
//! This does **not** attempt to be a generic actor framework. The
//! MQTT event loop lives outside the supervisor because it needs a
//! distinct cancellation token (the camera teardown publishes its
//! final `disconnected` status onto MQTT, so MQTT must outlive the
//! orchestrator). Everything that shares the global token is fair
//! game.

use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// A named service and its join handle. Internal-only — the supervisor
/// owns the lifecycle and exposes only the high-level methods below.
struct Service {
	name: String,
	handle: JoinHandle<()>,
}

/// Owns a CancellationToken and the JoinHandles of every spawned
/// service. Drop-safe: the cancellation token doesn't fire on drop,
/// callers MUST call [`Supervisor::shutdown`] explicitly so the
/// services see a graceful cancel signal.
pub struct Supervisor {
	cancel: CancellationToken,
	services: Vec<Service>,
}

impl Supervisor {
	pub fn new(cancel: CancellationToken) -> Self {
		Self {
			cancel,
			services: Vec::new(),
		}
	}

	/// Borrow the supervisor's cancellation token. Useful for
	/// services that need to share it with their own subsystems
	/// (e.g. the orchestrator's child tasks).
	pub fn cancel_token(&self) -> &CancellationToken {
		&self.cancel
	}

	/// Spawn a service that gets a clone of the supervisor's cancel
	/// token. The service body runs until the token fires or it
	/// returns naturally.
	pub fn spawn<F, Fut>(&mut self, name: impl Into<String>, body: F)
	where
		F: FnOnce(CancellationToken) -> Fut + Send + 'static,
		Fut: std::future::Future<Output = ()> + Send + 'static,
	{
		let name = name.into();
		let cancel = self.cancel.clone();
		let handle = tokio::spawn(async move {
			body(cancel).await;
		});
		tracing::debug!(service = %name, "service spawned");
		self.services.push(Service { name, handle });
	}

	/// Number of currently-tracked services. Test seam.
	#[cfg(test)]
	pub(crate) fn len(&self) -> usize {
		self.services.len()
	}

	/// Test seam — returns true when no services are tracked.
	#[cfg(test)]
	pub(crate) fn is_empty(&self) -> bool {
		self.services.is_empty()
	}

	/// Cancel the token (if not already cancelled), then join every
	/// service with a per-service timeout. Logs the name + exit
	/// status of each service so a wedged subsystem is visible
	/// without needing strace.
	pub async fn shutdown(mut self, per_service_timeout: Duration) {
		self.cancel.cancel();
		for Service { name, handle } in self.services.drain(..) {
			match tokio::time::timeout(per_service_timeout, handle).await {
				Ok(Ok(())) => tracing::debug!(service = %name, "service exited cleanly"),
				Ok(Err(e)) => tracing::warn!(service = %name, error = %e, "service panicked"),
				Err(_) => tracing::warn!(
					service = %name,
					timeout_secs = per_service_timeout.as_secs(),
					"service shutdown timed out; abandoning"
				),
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::atomic::{AtomicUsize, Ordering};
	use std::sync::Arc;

	#[tokio::test]
	async fn spawn_increments_len_and_runs_body() {
		let counter = Arc::new(AtomicUsize::new(0));
		let mut sup = Supervisor::new(CancellationToken::new());
		let c = Arc::clone(&counter);
		sup.spawn("worker", move |_cancel| async move {
			c.fetch_add(1, Ordering::SeqCst);
		});
		assert_eq!(sup.len(), 1);
		sup.shutdown(Duration::from_secs(1)).await;
		assert_eq!(counter.load(Ordering::SeqCst), 1);
	}

	#[tokio::test]
	async fn shutdown_cancels_long_running_service() {
		let mut sup = Supervisor::new(CancellationToken::new());
		sup.spawn("idler", move |cancel| async move {
			cancel.cancelled().await;
		});
		// shutdown() fires the cancel and joins.
		sup.shutdown(Duration::from_secs(1)).await;
	}

	#[tokio::test(start_paused = true)]
	async fn shutdown_times_out_on_wedged_service() {
		let mut sup = Supervisor::new(CancellationToken::new());
		sup.spawn("wedged", move |_cancel| async move {
			// Ignore cancel and sleep forever (well, virtual forever).
			tokio::time::sleep(Duration::from_secs(86_400)).await;
		});
		// Shutdown with a short budget — should not block forever.
		let start = tokio::time::Instant::now();
		sup.shutdown(Duration::from_millis(50)).await;
		let elapsed = start.elapsed();
		assert!(
			elapsed < Duration::from_secs(1),
			"shutdown must not block past the timeout, took {elapsed:?}"
		);
	}

	#[tokio::test]
	async fn empty_supervisor_shuts_down_immediately() {
		let sup = Supervisor::new(CancellationToken::new());
		assert!(sup.is_empty());
		sup.shutdown(Duration::from_secs(1)).await;
	}

	#[test]
	fn cancel_token_returns_underlying_token() {
		// `cancel_token()` is the seam orchestrator code uses to share
		// the supervisor's token with its own per-camera task tree.
		let outer = CancellationToken::new();
		let sup = Supervisor::new(outer.clone());
		// Cancelling via the borrowed reference fires on the original.
		sup.cancel_token().cancel();
		assert!(outer.is_cancelled());
	}

	#[tokio::test]
	async fn shutdown_logs_panicking_service_via_join_error() {
		// A service that panics surfaces as `Ok(Err(JoinError))` from
		// the timeout. Cover the warn branch so the supervisor's
		// log-and-continue contract is locked.
		let mut sup = Supervisor::new(CancellationToken::new());
		sup.spawn("panicker", move |_cancel| async move {
			panic!("intentional");
		});
		// shutdown must NOT propagate the panic.
		sup.shutdown(Duration::from_secs(1)).await;
	}
}
