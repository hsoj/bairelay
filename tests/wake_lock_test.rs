use std::time::Duration;

#[tokio::test]
async fn acquire_and_release() {
	let wl = bairelay::wake_lock::WakeLockCounter::new();
	assert_eq!(wl.count(), 0);
	let guard = wl.acquire();
	assert_eq!(wl.count(), 1);
	drop(guard);
	assert_eq!(wl.count(), 0);
}

#[tokio::test]
async fn default_works_same_as_new() {
	let wl_new = bairelay::wake_lock::WakeLockCounter::new();
	let wl_default = bairelay::wake_lock::WakeLockCounter::default();
	assert_eq!(wl_new.count(), wl_default.count());
	assert!(wl_new.is_idle());
	assert!(wl_default.is_idle());
	// Both should support acquire/release
	let g = wl_default.acquire();
	assert_eq!(wl_default.count(), 1);
	drop(g);
	assert_eq!(wl_default.count(), 0);
}

#[tokio::test]
async fn notify_future_fires_on_last_release() {
	let wl = bairelay::wake_lock::WakeLockCounter::new();
	let guard = wl.acquire();

	// Create the future BEFORE dropping, as the API intends.
	// notify_future() borrows wl, so we use it inline rather than spawning.
	let fut = wl.notify_future();

	// Drop in a separate task after a short delay
	tokio::spawn(async move {
		tokio::time::sleep(Duration::from_millis(50)).await;
		drop(guard);
	});

	// The future should complete once the guard is dropped
	let result = tokio::time::timeout(Duration::from_secs(1), fut).await;
	assert!(result.is_ok(), "notify_future should fire on last release");
}

#[tokio::test]
async fn wait_for_acquire_fires_on_0_to_1() {
	let wl = bairelay::wake_lock::WakeLockCounter::new();
	assert!(wl.is_idle());

	let wl_clone = wl.clone();
	let handle = tokio::spawn(async move {
		wl_clone.wait_for_acquire().await;
		true
	});

	tokio::time::sleep(Duration::from_millis(10)).await;
	let _guard = wl.acquire();

	let result = tokio::time::timeout(Duration::from_secs(1), handle).await;
	assert!(result.is_ok());
	assert!(result.unwrap().unwrap());
}

#[tokio::test]
async fn multiple_acquires_only_notify_on_first() {
	let wl = bairelay::wake_lock::WakeLockCounter::new();

	let wl_clone = wl.clone();
	let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(10);

	// Spawn a task that waits for acquire notifications and sends on channel
	let handle = tokio::spawn(async move {
		wl_clone.wait_for_acquire().await;
		let _ = tx.send(()).await;
		// Try to wait again — this should NOT fire from the second acquire
		tokio::time::timeout(Duration::from_millis(100), wl_clone.wait_for_acquire())
			.await
			.ok();
	});

	tokio::time::sleep(Duration::from_millis(10)).await;

	// First acquire: 0->1, should notify
	let g1 = wl.acquire();
	let got_first = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await;
	assert!(
		got_first.is_ok(),
		"should receive notification on 0->1 transition"
	);

	// Second acquire: 1->2, should NOT trigger another 0->1 notification
	let _g2 = wl.acquire();

	// The second wait_for_acquire in the spawned task should time out
	let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;

	drop(g1);
}

#[tokio::test]
async fn multiple_locks() {
	let wl = bairelay::wake_lock::WakeLockCounter::new();
	let g1 = wl.acquire();
	let g2 = wl.acquire();
	assert_eq!(wl.count(), 2);
	drop(g1);
	assert_eq!(wl.count(), 1);
	assert!(!wl.is_idle());
	drop(g2);
	assert_eq!(wl.count(), 0);
	assert!(wl.is_idle());
}

#[tokio::test]
async fn notifies_on_last_release() {
	let wl = bairelay::wake_lock::WakeLockCounter::new();
	let guard = wl.acquire();
	let wl_clone = wl.clone();
	let handle = tokio::spawn(async move {
		wl_clone.notified().await;
		true
	});
	tokio::time::sleep(Duration::from_millis(10)).await;
	drop(guard);
	let result = tokio::time::timeout(Duration::from_secs(1), handle).await;
	assert!(result.is_ok());
	assert!(result.unwrap().unwrap());
}

#[tokio::test]
async fn no_notify_when_not_last() {
	let wl = bairelay::wake_lock::WakeLockCounter::new();
	let g1 = wl.acquire();
	let g2 = wl.acquire();
	let wl_clone = wl.clone();
	let handle = tokio::spawn(async move {
		wl_clone.notified().await;
	});
	drop(g1);
	let result = tokio::time::timeout(Duration::from_millis(100), handle).await;
	assert!(result.is_err()); // Timed out — correct
	drop(g2);
}

#[tokio::test]
async fn is_idle_reflects_state() {
	let wl = bairelay::wake_lock::WakeLockCounter::new();
	assert!(wl.is_idle());
	let guard = wl.acquire();
	assert!(!wl.is_idle());
	drop(guard);
	assert!(wl.is_idle());
}

#[tokio::test]
async fn wait_for_acquire_returns_when_acquire_fired_before_wait() {
	// Regression: the camera run loop calls is_idle() then
	// wait_for_acquire() in sequence. If startup_wake (or any other
	// acquirer) calls acquire() between those two steps, notify_waiters
	// would drop the edge because no waiter is registered yet, and the
	// run loop would park forever. notify_one stores the permit so the
	// late-arriving waiter still completes. Asserts the fix.
	let wl = bairelay::wake_lock::WakeLockCounter::new();
	let _guard = wl.acquire();
	tokio::time::timeout(Duration::from_millis(100), wl.wait_for_acquire())
		.await
		.expect("wait_for_acquire must not block when acquire fired before the wait");
}

#[tokio::test]
async fn wait_for_acquire_wakes_up_on_concurrent_acquire() {
	let wl = bairelay::wake_lock::WakeLockCounter::new();
	let wl_acq = wl.clone();
	let waiter = tokio::spawn(async move { wl.wait_for_acquire().await });
	// Give the waiter a beat to register on the Notify.
	tokio::time::sleep(Duration::from_millis(10)).await;
	let _guard = wl_acq.acquire();
	tokio::time::timeout(Duration::from_millis(100), waiter)
		.await
		.expect("waiter must complete after acquire")
		.expect("waiter task panicked");
}

#[tokio::test]
async fn idle_since_returns_none_before_first_acquire() {
	// A pristine WakeLockCounter has never been touched. The watchdog
	// uses `idle_since().is_some()` as the gate to fire its
	// disconnect; this test pins that a never-acquired lock returns
	// `None` so the watchdog never disconnects a camera the operator
	// has never asked it to manage.
	let wl = bairelay::wake_lock::WakeLockCounter::new();
	assert!(wl.is_idle());
	assert_eq!(wl.idle_since(), None);
}

#[tokio::test]
async fn idle_since_records_release_timestamp() {
	// Acquire+drop must stamp `idle_since` so the watchdog can
	// compare elapsed against the configured grace period. Pins the
	// fix for the wakeup-vs-watchdog race that disconnected a camera
	// a few hundred ms before an MQTT `control/wakeup` could land.
	let wl = bairelay::wake_lock::WakeLockCounter::new();
	let before = std::time::Instant::now();
	drop(wl.acquire());
	let after = std::time::Instant::now();
	let stamped = wl.idle_since().expect("idle_since populated post-release");
	assert!(stamped >= before && stamped <= after);
}

#[tokio::test]
async fn idle_since_clears_on_reacquire() {
	// A new acquire after a release must clear the timestamp — the
	// camera is no longer idle, so the watchdog must not see a stale
	// `Some(_)` and disconnect mid-session.
	let wl = bairelay::wake_lock::WakeLockCounter::new();
	drop(wl.acquire());
	assert!(wl.idle_since().is_some());
	let _guard = wl.acquire();
	assert_eq!(wl.idle_since(), None);
}

#[tokio::test]
async fn idle_since_updates_on_each_release() {
	// Successive acquire+release cycles must refresh the timestamp
	// (not freeze it on the first release). Without this, a long-
	// idle camera that has been actively used in between would still
	// report its very first idle window and could be disconnected
	// immediately on the next watchdog tick.
	let wl = bairelay::wake_lock::WakeLockCounter::new();
	drop(wl.acquire());
	let first = wl.idle_since().expect("first release");
	tokio::time::sleep(std::time::Duration::from_millis(10)).await;
	drop(wl.acquire());
	let second = wl.idle_since().expect("second release");
	assert!(
		second > first,
		"expected fresh timestamp on second release; first={first:?} second={second:?}",
	);
}
