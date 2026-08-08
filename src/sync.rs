//! Poison-recovering lock accessors.
//!
//! `expect("… poisoned")` on a shared lock cascades a single bug across
//! every other holder: one panic while the guard is held poisons the
//! lock, and every later `expect` turns a one-task failure into
//! process-wide collapse (in the RTSP server, one client's panic taking
//! down every session). The state these locks guard — registries,
//! cached frames, counters, codec verdicts — is always recoverable,
//! never structurally invalid: a panic mid-mutation can at worst leave
//! a stale or half-updated *entry*, which the daemon already tolerates,
//! so recovering the guard is strictly better than dying.
//!
//! Use the method forms everywhere a `std::sync` lock crosses a task
//! boundary; bare `.expect("poisoned")` and `if let Ok(guard)` are both
//! rejected in review (action-plan checklist).

use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Poison-recovering accessors for [`RwLock`]: drop any poisoning and
/// return the guard the holder would have produced if no panic had run
/// while holding.
pub trait RwLockPoisonRecover<T> {
	fn read_recover(&self) -> RwLockReadGuard<'_, T>;
	fn write_recover(&self) -> RwLockWriteGuard<'_, T>;
}

impl<T> RwLockPoisonRecover<T> for RwLock<T> {
	fn read_recover(&self) -> RwLockReadGuard<'_, T> {
		self.read().unwrap_or_else(|e| e.into_inner())
	}
	fn write_recover(&self) -> RwLockWriteGuard<'_, T> {
		self.write().unwrap_or_else(|e| e.into_inner())
	}
}

/// [`Mutex`] variant of [`RwLockPoisonRecover`].
pub trait MutexPoisonRecover<T> {
	fn lock_recover(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexPoisonRecover<T> for Mutex<T> {
	fn lock_recover(&self) -> MutexGuard<'_, T> {
		self.lock().unwrap_or_else(|e| e.into_inner())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::Arc;

	fn poison_mutex(m: &Arc<Mutex<u32>>) {
		let m = Arc::clone(m);
		let _ = std::thread::spawn(move || {
			let _guard = m.lock().unwrap();
			panic!("poison on purpose");
		})
		.join();
	}

	fn poison_rwlock(l: &Arc<RwLock<u32>>) {
		let l = Arc::clone(l);
		let _ = std::thread::spawn(move || {
			let _guard = l.write().unwrap();
			panic!("poison on purpose");
		})
		.join();
	}

	#[test]
	fn mutex_recovers_after_poison() {
		let m = Arc::new(Mutex::new(7u32));
		poison_mutex(&m);
		assert!(m.lock().is_err(), "lock must actually be poisoned");
		assert_eq!(*m.lock_recover(), 7);
		*m.lock_recover() = 8;
		assert_eq!(*m.lock_recover(), 8);
	}

	#[test]
	fn rwlock_recovers_after_poison_on_both_sides() {
		let l = Arc::new(RwLock::new(7u32));
		poison_rwlock(&l);
		assert!(l.read().is_err(), "lock must actually be poisoned");
		assert_eq!(*l.read_recover(), 7);
		*l.write_recover() = 8;
		assert_eq!(*l.read_recover(), 8);
	}
}
