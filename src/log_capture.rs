//! Test-only `tracing` capture.
//!
//! Exists to pin the log lines that `tests/scripts/manual-verify.sh`
//! greps for. That script is the live-hardware gate for the RTSP path,
//! and it drives the daemon entirely through `target/release/bairelay`'s
//! stdout: it blocks on `RTSP server listening`, blocks again on
//! `Startup wake cycle complete`, and decides the battery-sleep stage
//! by counting `Grace period expired` / `Disconnected` hits. Reword any
//! of those and the script doesn't fail loudly — it stalls for its full
//! 30 s / 60 s poll window and then reports a misleading FAIL (or, for
//! the battery stage, a FAIL on working code). Nothing else in the
//! suite asserts on rendered log output, so these strings were free to
//! drift.
//!
//! The capture is a global subscriber because the emitters run on
//! spawned tasks across the runtime's worker threads — a scoped
//! (thread-local) subscriber would miss them. It is installed once per
//! test binary and shared, so assertions must disambiguate by a field
//! value unique to the calling test (a camera name, a bound port)
//! rather than by the marker alone. `assert_marker` does that.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex, OnceLock};

use tracing::field::{Field, Visit};
use tracing::Event;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;

type Buffer = Arc<Mutex<Vec<String>>>;

fn buffer() -> &'static Buffer {
	static BUFFER: OnceLock<Buffer> = OnceLock::new();
	BUFFER.get_or_init(Buffer::default)
}

/// Renders an event as `message field=value ...` — the message text
/// unquoted (it arrives as `format_args!`, whose `Debug` is the
/// formatted string) and every other field appended so callers can
/// disambiguate on `camera=` / `bind=`.
struct Rendered(String);

impl Visit for Rendered {
	fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
		let _ = if field.name() == "message" {
			write!(self.0, "{value:?}")
		} else {
			write!(self.0, " {}={:?}", field.name(), value)
		};
	}
}

struct CaptureLayer;

impl<S: tracing::Subscriber> Layer<S> for CaptureLayer {
	fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
		let mut rendered = Rendered(String::new());
		event.record(&mut rendered);
		buffer()
			.lock()
			.unwrap_or_else(|p| p.into_inner())
			.push(rendered.0);
	}
}

/// Install the capturing subscriber. Idempotent and safe to call from
/// every test — only the first call wins, and `try_init` swallows the
/// "already set" error if something else got there first.
///
/// Filtered to INFO so the trace-level packet dumps from the
/// `stream_source` tests sharing this binary don't pile into the buffer.
/// All four live-verify markers are `info!`.
pub(crate) fn install() {
	static INSTALLED: OnceLock<()> = OnceLock::new();
	INSTALLED.get_or_init(|| {
		let _ = tracing_subscriber::registry()
			.with(CaptureLayer.with_filter(LevelFilter::INFO))
			.try_init();
	});
}

/// Every captured line containing `needle`.
pub(crate) fn lines_containing(needle: &str) -> Vec<String> {
	buffer()
		.lock()
		.unwrap_or_else(|p| p.into_inner())
		.iter()
		.filter(|line| line.contains(needle))
		.cloned()
		.collect()
}

/// Assert that some single captured line carries both `marker` and
/// `discriminator`. Both on one line is the load-bearing part: the
/// buffer is shared with every other test in the binary, so matching
/// them independently would pass on two unrelated events.
#[track_caller]
pub(crate) fn assert_marker(marker: &str, discriminator: &str) {
	let hits = lines_containing(discriminator);
	assert!(
		hits.iter().any(|line| line.contains(marker)),
		"live-verify marker {marker:?} not emitted for {discriminator:?}.\n\
		 manual-verify.sh greps this string; see src/log_capture.rs.\n\
		 lines matching {discriminator:?}: {hits:#?}"
	);
}

/// Poll until [`assert_marker`]'s condition holds, or panic after
/// `attempts` yields. For markers emitted from a task the test does not
/// await — the RTSP accept loop logs before it is cancellable.
pub(crate) async fn await_marker(marker: &str, discriminator: &str, attempts: u32) {
	for _ in 0..attempts {
		if lines_containing(discriminator)
			.iter()
			.any(|line| line.contains(marker))
		{
			return;
		}
		tokio::task::yield_now().await;
		tokio::time::sleep(std::time::Duration::from_millis(10)).await;
	}
	assert_marker(marker, discriminator);
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The capture layer must render the message text unquoted and keep
	/// structured fields on the same line — everything `assert_marker`
	/// relies on.
	#[test]
	fn captures_message_and_fields_on_one_line() {
		install();
		tracing::info!(camera = %"cam-selftest", "sentinel message");
		let hits = lines_containing("cam-selftest");
		assert_eq!(hits.len(), 1, "expected exactly one hit, got {hits:#?}");
		assert!(
			hits[0].contains("sentinel message"),
			"message text must render unquoted: {:?}",
			hits[0]
		);
		assert_marker("sentinel message", "cam-selftest");
	}

	/// Below-INFO events are dropped, so unrelated trace-heavy tests in
	/// this binary can't crowd the buffer.
	#[test]
	fn debug_events_are_filtered_out() {
		install();
		tracing::debug!(camera = %"cam-debugfilter", "should not be captured");
		assert!(lines_containing("cam-debugfilter").is_empty());
	}
}
