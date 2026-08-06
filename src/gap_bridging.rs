//! Bridging gaps in a camera's video stream.
//!
//! Battery cameras stall their upstream for seconds at a time. Rather
//! than let RTSP subscribers see a dead stream, the source flips
//! `Live → Bridging` and re-broadcasts the cached key frame with a
//! synthesised presentation timestamp so the wire cadence never stops.
//! The operator-facing knobs are `pause.bridge_gaps` and
//! `pause.gap_threshold_secs`.
//!
//! This module is the decision half only: no channels, no locks, no
//! clock — time arrives as an [`Instant`] parameter, and the caller
//! performs the I/O the returned decisions describe. `stream_source.rs`
//! is the driver. Keeping the split means the PTS arithmetic, where
//! A/V-desync bugs actually live, is testable as plain values.

use std::time::{Duration, Instant};

/// Whether a source is forwarding upstream frames or filling a gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GapState {
	/// Upstream frames are flowing; broadcast them as they arrive.
	Live,
	/// Upstream has stalled past the threshold; the cached key frame is
	/// being re-broadcast to keep subscribers' timelines advancing.
	Bridging,
}

/// RTP clock rate for video timestamps.
const VIDEO_CLOCK_HZ: u64 = 90_000;

/// Gap-bridging state for one stream source.
///
/// Construct with [`BridgingPolicy::new`], then feed it the three
/// things that happen to a source: upstream packets arrive
/// ([`Self::on_upstream_packet`]), frames get broadcast
/// ([`Self::on_broadcast`]), and the gap-detection timer ticks
/// ([`Self::on_tick`]).
#[derive(Debug)]
pub struct BridgingPolicy {
	/// Silence tolerated before declaring a gap. [`Duration::MAX`]
	/// disables bridging entirely (`pause.bridge_gaps = false`), since
	/// no elapsed time can ever exceed it.
	gap_threshold: Duration,
	state: GapState,
	/// When upstream last delivered a video packet. Seeded at
	/// construction so a source that never receives anything still
	/// starts its gap countdown from birth rather than from the epoch.
	last_upstream_at: Instant,
	/// PTS of the last frame actually broadcast, live or replayed.
	/// `None` until the first broadcast.
	last_emitted_pts_90khz: Option<u32>,
	/// Wall-clock of that same broadcast, used to derive the synthetic
	/// PTS advance for the next replay.
	last_emit_wallclock_at: Option<Instant>,
}

impl BridgingPolicy {
	/// Start in [`GapState::Live`] with the gap countdown running from
	/// `now`.
	pub fn new(gap_threshold: Duration, now: Instant) -> Self {
		Self {
			gap_threshold,
			state: GapState::Live,
			last_upstream_at: now,
			last_emitted_pts_90khz: None,
			last_emit_wallclock_at: None,
		}
	}

	pub fn state(&self) -> GapState {
		self.state
	}

	/// PTS of the last frame broadcast on this source, live or
	/// replayed. `None` until the first broadcast.
	pub fn last_emitted_pts_90khz(&self) -> Option<u32> {
		self.last_emitted_pts_90khz
	}

	/// Force the state without waiting for a tick. Test-only: lets
	/// callers observe `Bridging`-dependent behaviour in sub-millisecond
	/// time instead of driving the real 200 ms ticker.
	#[cfg(any(test, feature = "test-util"))]
	pub fn set_state_for_test(&mut self, state: GapState) {
		self.state = state;
	}

	/// `true` while a gap is being bridged. Callers drop live audio in
	/// this state: video is frozen on a replayed key frame, so
	/// forwarding audio would present subscribers with nonsensical A/V
	/// correlation. Audio PTS counters still advance (the caller's job)
	/// so the two clocks realign on resume.
	pub fn is_bridging(&self) -> bool {
		self.state == GapState::Bridging
	}

	/// Record that upstream delivered a video packet.
	///
	/// This is deliberately driven by *arrival*, not by broadcast: a
	/// packet whose NALs are all filtered out (Reolink's UNSPEC62
	/// metadata, for one) still proves the Baichuan stream is alive, and
	/// must not let a gap fire spuriously.
	pub fn on_upstream_packet(&mut self, now: Instant) {
		self.last_upstream_at = now;
	}

	/// Record that a frame reached subscribers at `pts_90khz`.
	///
	/// Any successful broadcast ends a gap: real frames are flowing
	/// again, so the next tick has nothing to bridge.
	pub fn on_broadcast(&mut self, pts_90khz: u32, now: Instant) {
		self.state = GapState::Live;
		self.last_emitted_pts_90khz = Some(pts_90khz);
		self.last_emit_wallclock_at = Some(now);
	}

	/// Advance the gap-detection timer.
	///
	/// Flips to [`GapState::Bridging`] once upstream has been silent
	/// longer than the threshold. `replay_anchor` is the cached key
	/// frame's captured PTS, or `None` when the caller has nothing
	/// replayable (no burst captured yet, or every NAL filtered out).
	///
	/// Returns `Some(pts)` when the caller should re-broadcast the
	/// cached key frame at that timestamp. Returns `None` — leaving all
	/// counters untouched — when there is nothing to replay, so a source
	/// that stalls before its first key frame doesn't silently consume
	/// PTS space.
	pub fn on_tick(&mut self, now: Instant, replay_anchor: Option<u32>) -> Option<u32> {
		if now.saturating_duration_since(self.last_upstream_at) > self.gap_threshold {
			self.state = GapState::Bridging;
		}
		if self.state != GapState::Bridging {
			return None;
		}
		let anchor = replay_anchor?;
		Some(self.advance_replay_pts(anchor, now))
	}

	/// Synthesise the next replay PTS: last emitted + the wall-clock
	/// delta since that emission, converted to 90 kHz ticks.
	///
	/// On the first replay of a source's life there is no prior emission
	/// to measure from, so the delta is zero and the PTS anchors on the
	/// burst's own captured timestamp. Seeding the delta from
	/// construction time instead would inject process uptime into the
	/// subscriber's RTP timeline.
	///
	/// Wrapping arithmetic is intentional — RTP timestamps are `u32` and
	/// wrap at 2^32 by design.
	fn advance_replay_pts(&mut self, anchor: u32, now: Instant) -> u32 {
		let delta_90khz = match self.last_emit_wallclock_at {
			Some(last_at) => {
				let delta = now.saturating_duration_since(last_at);
				(delta.as_nanos().saturating_mul(VIDEO_CLOCK_HZ as u128) / 1_000_000_000) as u32
			}
			None => 0,
		};
		let synth = self
			.last_emitted_pts_90khz
			.unwrap_or(anchor)
			.wrapping_add(delta_90khz);
		self.last_emitted_pts_90khz = Some(synth);
		self.last_emit_wallclock_at = Some(now);
		synth
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const THRESHOLD: Duration = Duration::from_secs(2);

	fn policy(now: Instant) -> BridgingPolicy {
		BridgingPolicy::new(THRESHOLD, now)
	}

	#[test]
	fn starts_live_and_not_bridging() {
		let p = policy(Instant::now());
		assert_eq!(p.state(), GapState::Live);
		assert!(!p.is_bridging());
	}

	#[test]
	fn tick_within_threshold_stays_live() {
		let t0 = Instant::now();
		let mut p = policy(t0);
		assert_eq!(
			p.on_tick(t0 + Duration::from_millis(1_999), Some(100)),
			None
		);
		assert_eq!(p.state(), GapState::Live);
	}

	#[test]
	fn threshold_is_exclusive_exactly_at_boundary_stays_live() {
		// `>` not `>=`: a tick landing exactly on the threshold is not
		// yet a gap. Pinned because the ticker period and the threshold
		// are both operator-configurable and can coincide.
		let t0 = Instant::now();
		let mut p = policy(t0);
		assert_eq!(p.on_tick(t0 + THRESHOLD, Some(100)), None);
		assert_eq!(p.state(), GapState::Live);
	}

	#[test]
	fn tick_past_threshold_enters_bridging_and_replays_from_anchor() {
		let t0 = Instant::now();
		let mut p = policy(t0);
		let pts = p
			.on_tick(t0 + Duration::from_secs(3), Some(7_000))
			.expect("gap exceeded → replay");
		assert_eq!(p.state(), GapState::Bridging);
		// First replay of this source's life: no prior emission, so the
		// delta is zero and the PTS is the burst anchor itself.
		assert_eq!(pts, 7_000);
	}

	#[test]
	fn successive_replays_advance_by_wallclock_delta() {
		let t0 = Instant::now();
		let mut p = policy(t0);
		let first = p.on_tick(t0 + Duration::from_secs(3), Some(7_000)).unwrap();
		assert_eq!(first, 7_000);
		// 200 ms later → 200 ms × 90 kHz = 18 000 ticks.
		let second = p
			.on_tick(t0 + Duration::from_millis(3_200), Some(7_000))
			.unwrap();
		assert_eq!(second, 7_000 + 18_000);
		// Another 200 ms → another 18 000, measured from the previous
		// emission, not from the anchor.
		let third = p
			.on_tick(t0 + Duration::from_millis(3_400), Some(7_000))
			.unwrap();
		assert_eq!(third, 7_000 + 36_000);
	}

	#[test]
	fn replay_pts_is_monotonic_across_a_long_gap() {
		let t0 = Instant::now();
		let mut p = policy(t0);
		let mut last = 0u32;
		for step in 0..50 {
			let now = t0 + Duration::from_secs(3) + Duration::from_millis(200 * step);
			let pts = p.on_tick(now, Some(1_000)).expect("bridging → replay");
			if step > 0 {
				assert!(pts > last, "replay PTS must advance every tick");
			}
			last = pts;
		}
	}

	#[test]
	fn broadcast_during_gap_resumes_live_and_reanchors() {
		let t0 = Instant::now();
		let mut p = policy(t0);
		p.on_tick(t0 + Duration::from_secs(3), Some(7_000)).unwrap();
		assert!(p.is_bridging());

		// A real frame lands: back to Live, and the live PTS becomes the
		// base for any future replay.
		p.on_broadcast(50_000, t0 + Duration::from_secs(4));
		assert_eq!(p.state(), GapState::Live);
		assert!(!p.is_bridging());

		// Upstream goes quiet again; the next replay continues from the
		// live frame's PTS, not from the stale burst anchor.
		let pts = p
			.on_tick(t0 + Duration::from_millis(6_100), Some(7_000))
			.unwrap();
		assert_eq!(pts, 50_000 + (2_100 * 90));
	}

	#[test]
	fn upstream_packet_refreshes_liveness_and_prevents_the_gap() {
		let t0 = Instant::now();
		let mut p = policy(t0);
		// A packet at t+1.5s pushes the deadline out; the tick at t+3s
		// is only 1.5s past it, so no gap.
		p.on_upstream_packet(t0 + Duration::from_millis(1_500));
		assert_eq!(p.on_tick(t0 + Duration::from_secs(3), Some(1)), None);
		assert_eq!(p.state(), GapState::Live);
	}

	#[test]
	fn upstream_packet_alone_does_not_leave_bridging() {
		// Arrival refreshes the countdown but only a real broadcast
		// proves subscribers saw something, so the state stays Bridging
		// until then. Guards the metadata-only-packet case: those never
		// broadcast, and flipping to Live on arrival would stop the
		// replay stream while the screen is still frozen.
		let t0 = Instant::now();
		let mut p = policy(t0);
		p.on_tick(t0 + Duration::from_secs(3), Some(7_000)).unwrap();
		assert!(p.is_bridging());

		p.on_upstream_packet(t0 + Duration::from_secs(4));
		assert!(p.is_bridging(), "arrival alone must not resume Live");

		p.on_broadcast(9_000, t0 + Duration::from_secs(4));
		assert!(!p.is_bridging());
	}

	#[test]
	fn no_replay_anchor_enters_bridging_but_emits_nothing() {
		let t0 = Instant::now();
		let mut p = policy(t0);
		assert_eq!(p.on_tick(t0 + Duration::from_secs(3), None), None);
		assert_eq!(
			p.state(),
			GapState::Bridging,
			"state still flips — the gap is real even with nothing to send"
		);

		// Counters were untouched, so the first real replay still
		// anchors on the burst rather than on a phantom emission.
		let pts = p.on_tick(t0 + Duration::from_secs(4), Some(2_500)).unwrap();
		assert_eq!(pts, 2_500);
	}

	#[test]
	fn max_threshold_disables_bridging_entirely() {
		// `pause.bridge_gaps = false` folds to Duration::MAX so the
		// ticker can run without ever declaring a gap.
		let t0 = Instant::now();
		let mut p = BridgingPolicy::new(Duration::MAX, t0);
		assert_eq!(p.on_tick(t0 + Duration::from_secs(86_400), Some(1)), None);
		assert_eq!(p.state(), GapState::Live);
	}

	#[test]
	fn replay_pts_wraps_at_u32_boundary() {
		// RTP timestamps wrap by design; the synth must wrap with them
		// rather than saturate (which would stall the timeline) or panic
		// in debug builds.
		let t0 = Instant::now();
		let mut p = policy(t0);
		p.on_broadcast(u32::MAX - 10, t0);
		let pts = p
			.on_tick(t0 + Duration::from_secs(3), Some(0))
			.expect("bridging → replay");
		// 3 s × 90 kHz = 270 000 ticks past u32::MAX - 10.
		assert_eq!(pts, (u32::MAX - 10).wrapping_add(270_000));
	}

	#[test]
	fn sub_tick_delta_rounds_down_without_stalling_the_timeline() {
		// A delta shorter than one 90 kHz tick (~11 µs) contributes
		// zero. Documented rather than fixed: the ticker runs at 200 ms,
		// so this is unreachable in production and rounding up would be
		// the more surprising behaviour.
		let t0 = Instant::now();
		let mut p = policy(t0);
		p.on_broadcast(1_000, t0 + Duration::from_secs(3));
		let pts = p
			.on_tick(
				t0 + Duration::from_secs(3) + Duration::from_micros(5),
				Some(0),
			)
			.unwrap();
		assert_eq!(pts, 1_000);
	}
}
