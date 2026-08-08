//! Camera login credentials. Carries the heap-plaintext password so it
//! gets actively zeroed on drop — `String::drop` only frees the heap
//! buffer, leaving the plaintext bytes recoverable until the allocator
//! reuses the page. `Credentials::drop` runs `Zeroize::zeroize` first so
//! the plaintext doesn't outlive the value.
//!
//! No `Default` impl: a baked-in factory password (`"123456"`) was a
//! footgun — accidental `Credentials::default()` in a future call site
//! would silently authenticate against the camera with the documented
//! Reolink default credentials. Construct via [`Credentials::new`].

use zeroize::{Zeroize, Zeroizing};

/// Camera login pair. Username is non-secret; password is wiped on
/// drop. The `Drop` impl is the load-bearing piece — never replace
/// `Credentials` with a derive that bypasses it.
#[derive(Clone)]
pub struct Credentials {
	/// The username to login to the camera with
	pub username: String,
	/// The password to use for login. Some cameras allow this to be
	/// omitted (anonymous login). Zeroed on drop.
	pub password: Option<String>,
}

impl std::fmt::Debug for Credentials {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_map()
			.entry(&"username", &self.username)
			.entry(&"password", &"******")
			.finish()
	}
}

impl Drop for Credentials {
	fn drop(&mut self) {
		if let Some(pw) = self.password.as_mut() {
			pw.zeroize();
		}
	}
}

impl Credentials {
	pub(crate) fn new<T: Into<String>, U: Into<String>>(username: T, password: Option<U>) -> Self {
		Self {
			username: username.into(),
			password: password.map(|t| t.into()),
		}
	}

	/// This is a convience function to make an AES key from the login
	/// password and the NONCE negotiated during login.
	///
	/// Intermediate plaintext (`key_phrase`, the cloned password) is
	/// held in `Zeroizing` so it wipes on every early-return / scope
	/// exit. Without this, the `format!("{}-{}", ...)` and the
	/// `clone().unwrap_or_default()` would each spawn a heap String
	/// that lingers until the allocator reuses the page.
	pub(crate) fn make_aeskey<T: AsRef<str>>(&self, nonce: T) -> [u8; 16] {
		let password = Zeroizing::new(self.password.clone().unwrap_or_default());
		let key_phrase = Zeroizing::new(format!("{}-{}", nonce.as_ref(), *password));
		// `{:X}` already emits uppercase hex; `.to_uppercase()` was a
		// no-op carried over from a lowercase variant in upstream code.
		let key_phrase_hash = format!("{:X}\0", md5::compute(&*key_phrase)).into_bytes();
		// 32 hex chars + NUL, always ≥16 bytes; the zero-key fallback is
		// unreachable but beats a panic inside the login path.
		key_phrase_hash
			.first_chunk::<16>()
			.copied()
			.unwrap_or_default()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn debug_redacts_password() {
		let c = Credentials::new("admin", Some("hunter2"));
		let dbg = format!("{:?}", c);
		assert!(dbg.contains("admin"));
		assert!(!dbg.contains("hunter2"));
		assert!(dbg.contains("******"));
	}

	#[test]
	fn make_aeskey_is_deterministic_and_16_bytes() {
		let c = Credentials::new("admin", Some("password"));
		let k1 = c.make_aeskey("nonce-abc");
		let k2 = c.make_aeskey("nonce-abc");
		assert_eq!(k1, k2);
		// Different nonce -> different key.
		let k3 = c.make_aeskey("nonce-xyz");
		assert_ne!(k1, k3);
	}

	#[test]
	fn zeroize_clears_the_inner_string_in_place() {
		// Verifies the contract Drop relies on: `String::zeroize()`
		// overwrites the heap buffer in place before the String drops.
		// We can't observe the buffer after `drop(Credentials)` without
		// reading freed memory (UB and allocator-dependent), so instead
		// hold a `String`, capture its body, run zeroize, then read it
		// back through the still-live `String` reference.
		let mut pw = String::from("hunter22hunter22hunter22");
		let len = pw.len();
		pw.zeroize();
		// `String::zeroize` truncates length to 0 (per its docs); the
		// capacity buffer is left zeroed but unobservable from safe
		// Rust. Length-0 + capacity-preserved is the wire contract we
		// rely on inside Drop.
		assert_eq!(pw.len(), 0);
		assert!(pw.capacity() >= len);
	}
}
