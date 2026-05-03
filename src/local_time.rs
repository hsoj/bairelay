//! Local-time support for log timestamps and the `set-time` one-shot.
//!
//! `time::UtcOffset::current_local_offset()` is unsound to call once a
//! Unix process has worker threads (TZ env races with `setenv`), so the
//! `time` crate makes it return `Err` in that case. To get local time
//! anywhere in the program, capture the offset once via [`init`] from
//! `main()` *before* the tokio runtime is built, then read it from the
//! `OnceLock` everywhere else.

use std::sync::OnceLock;

use time::{OffsetDateTime, UtcOffset};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;

static LOCAL_OFFSET: OnceLock<UtcOffset> = OnceLock::new();

/// Capture the host's current local UTC offset. Falls back to `UTC` if
/// the platform refuses to compute one (e.g. called too late, after
/// worker threads exist). Idempotent — first call wins.
pub fn init() {
	let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
	let _ = LOCAL_OFFSET.set(offset);
}

/// Return the offset captured by [`init`], or `UTC` when [`init`] was
/// never called (typical in unit tests that don't go through `main()`).
pub fn offset() -> UtcOffset {
	LOCAL_OFFSET.get().copied().unwrap_or(UtcOffset::UTC)
}

/// Wall-clock now, anchored to the captured local offset.
pub fn now_local() -> OffsetDateTime {
	OffsetDateTime::now_utc().to_offset(offset())
}

/// `tracing_subscriber` timer that prints local time using the captured
/// offset. Format: `2026-04-28 20:27:46` (no trailing `Z`, no fractional
/// seconds — log-grep ergonomics over wire precision).
pub struct LocalTimer;

impl FormatTime for LocalTimer {
	fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
		let fmt =
			time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
		let s = now_local().format(&fmt).map_err(|_| std::fmt::Error)?;
		write!(w, "{}", s)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn offset_returns_utc_or_captured_value() {
		// Offset always returns *something*. Either the OnceLock holds
		// what `init` captured (any unit-test process that imported a
		// crate which called init() inherits it) or the UTC fallback
		// fires. Both paths are valid; we only pin that the call is
		// total.
		let _ = offset();
	}

	#[test]
	fn init_is_idempotent() {
		// `init()` uses `OnceLock::set` which returns Err on subsequent
		// calls; the binding swallows that. Calling twice must not panic.
		init();
		init();
	}

	#[test]
	fn now_local_does_not_panic_and_uses_captured_offset() {
		// `now_local()` is total — anchored on the captured offset (or
		// UTC fallback). Cross-check that the returned datetime's offset
		// matches `offset()` at the same instant.
		let dt = now_local();
		assert_eq!(dt.offset(), offset());
	}

	#[test]
	fn local_timer_writes_yyyy_mm_dd_hh_mm_ss() {
		let mut buf = String::new();
		let mut writer = Writer::new(&mut buf);
		LocalTimer.format_time(&mut writer).expect("format");
		// Loose shape match — exact wallclock is non-deterministic but
		// the format string is fixed: 19 chars, dashes at 5/8, colons
		// at 14/17, space at 11.
		assert_eq!(buf.len(), 19, "unexpected output: {buf:?}");
		let bytes = buf.as_bytes();
		assert_eq!(bytes[4], b'-');
		assert_eq!(bytes[7], b'-');
		assert_eq!(bytes[10], b' ');
		assert_eq!(bytes[13], b':');
		assert_eq!(bytes[16], b':');
	}
}
