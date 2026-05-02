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
