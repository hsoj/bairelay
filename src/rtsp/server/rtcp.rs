//! RTCP Sender Report builder per RFC 3550 §6.4.1.
//!
//! Sender Reports carry an NTP timestamp + RTP timestamp correspondence
//! that lets receivers synchronise media and compute drift. **Periodic
//! SR is intentionally disabled in production** (Phase 3D — every SR
//! receipt re-anchored mpv/ffmpeg's decode clock and surfaced as a
//! brief A-V hitch every `SR_INTERVAL`). Receivers fall back to
//! RTP-arrival-time sync, which at our sub-second live-camera latency
//! is indistinguishable. The helpers below remain for any future
//! SR-emitting context (e.g. a recording sink that needs precise
//! NTP↔RTP). The disabled `sr_ticker` arm in
//! `crate::rtsp::server::session_task::run` is the single point of toggle.
//!
//! **Why NTP and RTP are both caller-supplied.** RFC 3550 §6.4.1 is
//! explicit: the NTP and RTP timestamps in an SR MUST correspond to the
//! same instant ("Rather, it MUST be calculated from the corresponding
//! NTP timestamp using the relationship between the RTP timestamp
//! counter and real time as maintained by periodically checking the
//! wallclock time at a sampling instant.").
//!
//! Earlier revisions sampled `ntp_now()` inside this function while the
//! caller passed `rtp_timestamp = last_rtp_ts` — the RTP timestamp of
//! the last dispatched packet, which lagged the SR fire instant by up to
//! one frame period (≤ 64 ms for AAC-LC at 16 kHz). ffmpeg uses the
//! SR's (NTP, RTP) pair to calibrate its decode clock; inconsistent
//! pairs surface as `non monotonically increasing dts` warnings every SR
//! interval. The post-Phase-2E fix takes a different tack: the session
//! task records `last_rtp_ts` alongside a monotonic `Instant`, and at
//! SR fire time extrapolates forward to "now" using the track's clock
//! rate — so the SR reports the (NTP, RTP) pair that would correspond
//! to the same "now" a next-arriving frame would extend. See
//! `docs/implementation.md` for the investigation log.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// RTCP packet type: Sender Report.
pub const PT_SENDER_REPORT: u8 = 200;

/// Build a 28-byte minimal Sender Report (no RR blocks).
///
/// - `ssrc` — the stream's RTP SSRC.
/// - `ntp_sec` / `ntp_frac` — the NTP wall-clock instant that corresponds
///   to `rtp_timestamp`. Callers MUST sample both at the same instant
///   (see module docs).
/// - `rtp_timestamp` — the RTP timestamp corresponding to that instant
///   (in the media's clock rate).
/// - `packet_count` / `octet_count` — totals since stream start.
pub fn build_sender_report(
	ssrc: u32,
	ntp_sec: u32,
	ntp_frac: u32,
	rtp_timestamp: u32,
	packet_count: u32,
	octet_count: u32,
) -> [u8; 28] {
	let mut buf = [0u8; 28];
	// Byte 0: V=2, P=0, RC=0 → 0x80
	buf[0] = 0x80;
	// Byte 1: PT = 200
	buf[1] = PT_SENDER_REPORT;
	// Bytes 2-3: length in 32-bit words minus 1 → (28/4)-1 = 6
	buf[2..4].copy_from_slice(&6u16.to_be_bytes());
	// Bytes 4-7: SSRC
	buf[4..8].copy_from_slice(&ssrc.to_be_bytes());
	// Bytes 8-11: NTP seconds
	buf[8..12].copy_from_slice(&ntp_sec.to_be_bytes());
	// Bytes 12-15: NTP fraction
	buf[12..16].copy_from_slice(&ntp_frac.to_be_bytes());
	// Bytes 16-19: RTP timestamp
	buf[16..20].copy_from_slice(&rtp_timestamp.to_be_bytes());
	// Bytes 20-23: packet count
	buf[20..24].copy_from_slice(&packet_count.to_be_bytes());
	// Bytes 24-27: octet count
	buf[24..28].copy_from_slice(&octet_count.to_be_bytes());
	buf
}

/// Recommended interval between Sender Reports.
pub const SR_INTERVAL: Duration = Duration::from_secs(5);

/// Subtract a wall-clock `Duration` from an NTP `(sec, frac)` pair.
///
/// Used by the session task to translate "NTP at now" into "NTP at the
/// instant we last dispatched a packet" without re-sampling
/// `SystemTime::now()`. Pairing the SR's NTP with the actual dispatch
/// instant (instead of "now" + an extrapolated RTP) keeps (NTP, RTP)
/// referring to the *same* instant per RFC 3550 §6.4.1, with zero
/// extrapolation error — the receiver's slope between successive SRs
/// is then exactly the per-packet emission slope, which the audio
/// pacer holds at `clock_rate`.
pub fn ntp_minus(ntp_sec: u32, ntp_frac: u32, dt: Duration) -> (u32, u32) {
	let dt_sec = dt.as_secs() as u32;
	// Convert dt's sub-second part to NTP fraction units (Q32 of a second).
	let dt_frac = ((dt.subsec_nanos() as u64 * (1u64 << 32)) / 1_000_000_000) as u32;
	let (frac, borrow) = ntp_frac.overflowing_sub(dt_frac);
	let sec = ntp_sec.wrapping_sub(dt_sec);
	let sec = if borrow { sec.wrapping_sub(1) } else { sec };
	(sec, frac)
}

/// Wall-clock sampled as `(ntp_seconds, ntp_fraction)`. Callers sample
/// this at the same instant they capture a frame's RTP timestamp so the
/// pair can be passed into [`build_sender_report`] preserving the RFC
/// 3550 §6.4.1 "same instant" invariant.
pub fn ntp_now() -> (u32, u32) {
	let now = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default();
	// NTP epoch = 1900-01-01; Unix epoch = 1970-01-01. Offset = 2_208_988_800 s.
	const NTP_OFFSET: u64 = 2_208_988_800;
	let secs = now.as_secs() + NTP_OFFSET;
	let frac = ((now.subsec_nanos() as u64 * (1u64 << 32)) / 1_000_000_000) as u32;
	(secs as u32, frac)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn sender_report_is_28_bytes_version_2_pt_200() {
		let sr = build_sender_report(0xDEADBEEF, 0x1234_5678, 0x9ABC_DEF0, 90_000, 100, 14_400);
		assert_eq!(sr.len(), 28);
		assert_eq!(sr[0], 0x80);
		assert_eq!(sr[1], 200);
		// Length = 6 words
		assert_eq!(u16::from_be_bytes([sr[2], sr[3]]), 6);
	}

	#[test]
	fn sender_report_fields_roundtrip() {
		let sr = build_sender_report(0xAABBCCDD, 0x1111_2222, 0x3333_4444, 123_456, 42, 8_400);
		assert_eq!(u32::from_be_bytes([sr[4], sr[5], sr[6], sr[7]]), 0xAABBCCDD);
		// NTP seconds and fraction must be the caller-supplied values,
		// not sampled inside build_sender_report — this is the whole
		// point of the signature change. See module docs.
		assert_eq!(
			u32::from_be_bytes([sr[8], sr[9], sr[10], sr[11]]),
			0x1111_2222,
			"NTP seconds must come from caller, not from now()"
		);
		assert_eq!(
			u32::from_be_bytes([sr[12], sr[13], sr[14], sr[15]]),
			0x3333_4444,
			"NTP fraction must come from caller, not from now()"
		);
		assert_eq!(
			u32::from_be_bytes([sr[16], sr[17], sr[18], sr[19]]),
			123_456
		);
		assert_eq!(u32::from_be_bytes([sr[20], sr[21], sr[22], sr[23]]), 42);
		assert_eq!(u32::from_be_bytes([sr[24], sr[25], sr[26], sr[27]]), 8_400);
	}

	#[test]
	fn ntp_now_seconds_are_after_ntp_epoch() {
		let (secs, _) = ntp_now();
		// Sanity: 2026 is well past 1900+2_208_988_800 = 2070 NTP-sec ≈ 2^31.
		assert!(secs > 2_208_988_800);
	}

	#[test]
	fn ntp_minus_subtracts_whole_seconds() {
		// Whole-second delta with no sub-second component → sec drops by
		// the duration, frac unchanged.
		let (sec, frac) = ntp_minus(1000, 0x8000_0000, Duration::from_secs(3));
		assert_eq!(sec, 997);
		assert_eq!(frac, 0x8000_0000);
	}

	#[test]
	fn ntp_minus_subtracts_subsecond_without_borrow() {
		// 500 ms = 0.5 in NTP fraction = 0x8000_0000. Subtracting from
		// 0xC000_0000 leaves 0x4000_0000, no borrow into seconds.
		let (sec, frac) = ntp_minus(1000, 0xC000_0000, Duration::from_millis(500));
		assert_eq!(sec, 1000);
		assert_eq!(frac, 0x4000_0000);
	}

	#[test]
	fn ntp_minus_borrows_into_seconds_when_frac_underflows() {
		// 500 ms subtracted from 0x4000_0000 (0.25) underflows → frac
		// wraps high (0xC000_0000) and sec borrows one extra.
		let (sec, frac) = ntp_minus(1000, 0x4000_0000, Duration::from_millis(500));
		assert_eq!(sec, 999);
		assert_eq!(frac, 0xC000_0000);
	}

	#[test]
	fn ntp_minus_handles_combined_seconds_and_subsecond() {
		// 2.75 s = 2 s + 0xC000_0000 frac. Subtract from
		// (1000, 0x4000_0000) → frac borrows once (becomes 0x8000_0000),
		// seconds decrement by 2 + 1 = 3 → 997.
		let (sec, frac) = ntp_minus(1000, 0x4000_0000, Duration::from_millis(2750));
		assert_eq!(sec, 997);
		assert_eq!(frac, 0x8000_0000);
	}
}
