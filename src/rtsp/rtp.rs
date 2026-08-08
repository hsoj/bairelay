//! RTP packet header builder.
//!
//! A thin wrapper on top of `rtp-types` that enforces bairelay's
//! conventions (version 2, no padding, no extensions, no CSRCs).

use rtp_types::RtpPacketBuilder;

/// Build a single RTP packet with the given fields.
///
/// Returns the fully serialized packet.
pub fn build_packet(
	payload_type: u8,
	sequence_number: u16,
	timestamp: u32,
	ssrc: u32,
	marker: bool,
	payload: &[u8],
) -> Vec<u8> {
	let builder = RtpPacketBuilder::new()
		.payload_type(payload_type)
		.sequence_number(sequence_number)
		.timestamp(timestamp)
		.ssrc(ssrc)
		.marker_bit(marker)
		.payload(payload);
	// Payload sizes are bounded well below the RTP maximum by the
	// fragmenters (MTU) and the ADTS frame-length field (8191), so the
	// builder cannot refuse them; if it ever does, emit nothing — the
	// receiver treats the missing packet as ordinary RTP loss, which
	// beats panicking inside the per-packet send path.
	let size = match builder.calculate_size() {
		Ok(s) => s,
		Err(e) => {
			tracing::error!(error = ?e, payload_len = payload.len(), "RTP packet build failed");
			return Vec::new();
		}
	};
	let mut buf = vec![0u8; size];
	if let Err(e) = builder.write_into(&mut buf) {
		tracing::error!(error = ?e, payload_len = payload.len(), "RTP packet write failed");
		return Vec::new();
	}
	buf
}

/// Per-session RTP counters.
///
/// Owned by a packetizer; increments on every packet sent. Use
/// `next()` to obtain (seq, ts) for the next RTP packet given the
/// media timestamp in the codec's clock.
pub struct RtpCounters {
	/// Synchronization source identifier carried on every RTP packet.
	/// `pub(crate)` so the codec / packetizer / session-task pipeline can
	/// inspect it without exposing the SSRC value to downstream binaries
	/// that have no use for it.
	pub(crate) ssrc: u32,
	/// Next sequence number to emit; increments on every packet with wrap-around.
	pub seq: u16,
}

impl RtpCounters {
	/// Construct a fresh counter pair with cryptographically random `ssrc`
	/// and an unpredictable starting `seq`, per RFC 3550 recommendations.
	pub fn random() -> Self {
		use rand::Rng;
		let mut rng = rand::thread_rng();
		Self {
			ssrc: rng.gen(),
			seq: rng.gen(),
		}
	}

	/// Construct a counter pair with caller-chosen `ssrc` and `seq`.
	/// Used by benchmarks and integration tests that need deterministic
	/// values; production code path uses [`Self::random`].
	pub fn fixed(ssrc: u32, seq: u16) -> Self {
		Self { ssrc, seq }
	}

	/// Return the current sequence number and advance it, wrapping at `u16::MAX`.
	pub fn next_seq(&mut self) -> u16 {
		let s = self.seq;
		self.seq = self.seq.wrapping_add(1);
		s
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn builds_parseable_rtp_packet() {
		let pkt = build_packet(96, 1234, 90000, 0xdeadbeef, true, &[0xaa, 0xbb, 0xcc]);
		// RTP header is 12 bytes, payload 3 bytes
		assert_eq!(pkt.len(), 15);
		// Version 2, no padding, no extension, no CC
		assert_eq!(pkt[0], 0x80);
		// Marker + PT 96
		assert_eq!(pkt[1], 0x80 | 96);
		// Sequence
		assert_eq!(u16::from_be_bytes([pkt[2], pkt[3]]), 1234);
		// Timestamp
		assert_eq!(u32::from_be_bytes([pkt[4], pkt[5], pkt[6], pkt[7]]), 90000);
		// SSRC
		assert_eq!(
			u32::from_be_bytes([pkt[8], pkt[9], pkt[10], pkt[11]]),
			0xdeadbeef
		);
		// Payload
		assert_eq!(&pkt[12..], &[0xaa, 0xbb, 0xcc]);
	}

	#[test]
	fn counters_wrap_correctly() {
		let mut c = RtpCounters {
			ssrc: 0,
			seq: 65534,
		};
		assert_eq!(c.next_seq(), 65534);
		assert_eq!(c.next_seq(), 65535);
		assert_eq!(c.next_seq(), 0);
		assert_eq!(c.next_seq(), 1);
	}

	#[test]
	fn random_counters_produce_distinct_values() {
		// Smoke test: `random()` must not panic and must yield *some* value
		// for both fields. We also check that repeated calls don't collapse
		// to a single deterministic output (extremely improbable at u32+u16).
		let a = RtpCounters::random();
		let b = RtpCounters::random();
		assert!(a.ssrc != b.ssrc || a.seq != b.seq);
	}

	#[test]
	fn build_packet_without_marker_and_empty_payload() {
		let pkt = build_packet(0, 0, 0, 0, false, &[]);
		// 12-byte RTP header, no payload.
		assert_eq!(pkt.len(), 12);
		// Marker bit cleared; PT=0.
		assert_eq!(pkt[1], 0);
	}
}
