//! H.264 RTP packetization per RFC 6184.
//!
//! Supports:
//! - Single NAL unit (types 1–23)
//! - FU-A (type 28): fragment NALs larger than MTU
//!
//! Parameter-set aggregation (STAP-A) is intentionally unused: outbound
//! `Frame::Video` strips SPS/PPS at the translator boundary
//! (`stream_source::handle_iframe`) and the SDP `sprop-parameter-sets`
//! fmtp attribute carries them out-of-band.

use crate::rtsp::rtp::{build_packet, RtpCounters};

/// RTP payload type for H.264 (dynamic; we use 96 by convention).
pub const H264_PAYLOAD_TYPE: u8 = 96;

/// RTP clock rate for H.264 (always 90 kHz).
pub const H264_CLOCK_HZ: u32 = 90_000;

/// Maximum MTU we target for RTP payloads over UDP/TCP-interleaved.
pub const DEFAULT_MTU: usize = 1400;

/// Packetize a single NAL unit smaller than the MTU into one RTP packet.
///
/// Returns the serialized RTP packet.
pub fn packetize_single(
	nal: &[u8],
	counters: &mut RtpCounters,
	timestamp_90khz: u32,
	marker: bool,
) -> Vec<u8> {
	let seq = counters.next_seq();
	build_packet(
		H264_PAYLOAD_TYPE,
		seq,
		timestamp_90khz,
		counters.ssrc,
		marker,
		nal,
	)
}

/// FU-A NAL unit type (28).
const FU_A_TYPE: u8 = 28;

/// Packetize a NAL larger than `mtu` into FU-A fragments per RFC 6184 §5.8.
///
/// Each fragment has:
/// - 1-byte FU indicator: F(1) | NRI(2) | Type=28(5) — F and NRI copied from original NAL header
/// - 1-byte FU header: S(1) | E(1) | R(1)=0 | Type(5) — S on first, E on last
/// - Fragment payload
///
/// Returns each fragment as a separate RTP packet; marker bit is set on the
/// last fragment only if `marker_on_last` is true (i.e. this is the last NAL
/// of an access unit).
pub fn packetize_fu_a(
	nal: &[u8],
	counters: &mut RtpCounters,
	timestamp_90khz: u32,
	marker_on_last: bool,
	mtu: usize,
) -> Vec<Vec<u8>> {
	// `nal.len() >= 2` (header + ≥1 body byte) is required: a header-only
	// NAL has nothing to fragment and would silently emit an empty Vec,
	// violating RFC 6184 §5.8 (every fragmented NAL must produce ≥1
	// fragment with S=1, E=1). Mirror H.265 `packetize_fu`'s precondition.
	assert!(nal.len() >= 2, "H.264 NAL must have header + ≥1 body byte");
	// MTU smaller than RTP (12) + FU (2) = 14 bytes can't emit a
	// valid fragment. Earlier code panicked; now logged + emit
	// empty output. Production callers pass `DEFAULT_MTU = 1400`,
	// so this guards a future refactor (operator-supplied MTU)
	// rather than any current code path. See sibling H.265 path.
	if mtu <= 14 {
		tracing::error!(
			"packetize_fu_a: mtu={mtu} too small for RTP+FU headers (need >14); emitting nothing"
		);
		return Vec::new();
	}

	let nal_header = nal[0];
	let nri = nal_header & 0x60;
	let nal_type = nal_header & 0x1F;
	let body = &nal[1..];

	let max_fragment_payload = mtu - 12 /* RTP */ - 2 /* FU indicator + header */;
	let mut packets = Vec::new();
	let mut offset = 0;
	let total = body.len();

	while offset < total {
		let end = (offset + max_fragment_payload).min(total);
		let is_first = offset == 0;
		let is_last = end == total;

		let fu_indicator = nri | FU_A_TYPE;
		let mut fu_header = nal_type;
		if is_first {
			fu_header |= 0x80;
		}
		if is_last {
			fu_header |= 0x40;
		}

		let mut payload = Vec::with_capacity(2 + (end - offset));
		payload.push(fu_indicator);
		payload.push(fu_header);
		payload.extend_from_slice(&body[offset..end]);

		let seq = counters.next_seq();
		let marker = is_last && marker_on_last;
		packets.push(build_packet(
			H264_PAYLOAD_TYPE,
			seq,
			timestamp_90khz,
			counters.ssrc,
			marker,
			&payload,
		));
		offset = end;
	}
	packets
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn packetize_fu_a_emits_exact_count_at_mtu_boundary() {
		// max_fragment_payload = mtu - 12 (RTP) - 2 (FU) = mtu - 14.
		// Pick mtu so two fragments fit exactly: payload size = 2*M.
		let mtu = 64usize;
		let max_payload = mtu - 14;
		let mut c = RtpCounters {
			ssrc: 0xAAAA,
			seq: 0,
		};
		let mut nal = vec![0x65u8]; // header
		nal.extend(vec![0xCC; max_payload * 2]);
		let pkts = packetize_fu_a(&nal, &mut c, 100, true, mtu);
		assert_eq!(pkts.len(), 2, "exactly two fragments at the boundary");
	}

	#[test]
	fn packetize_fu_a_sequence_numbers_are_monotonic() {
		// Multi-fragment NAL must produce strictly +1 sequence numbers
		// across the emitted RTP packets — gaps or reuse would break
		// the receiver's reorder/dedup buffer.
		let mut c = RtpCounters { ssrc: 1, seq: 1000 };
		let mut nal = vec![0x65u8];
		nal.extend(vec![0xDD; 5000]);
		let pkts = packetize_fu_a(&nal, &mut c, 0, true, 1400);
		assert!(pkts.len() >= 4);
		// RTP seq sits at bytes 2-3, big-endian.
		let seqs: Vec<u16> = pkts
			.iter()
			.map(|p| u16::from_be_bytes([p[2], p[3]]))
			.collect();
		for window in seqs.windows(2) {
			assert_eq!(
				window[1].wrapping_sub(window[0]),
				1,
				"seq jumped: {} -> {}",
				window[0],
				window[1]
			);
		}
	}

	#[test]
	fn packetize_fu_a_with_too_small_mtu_emits_nothing() {
		// Used to panic via assert!(mtu > 14, ...). Now logs an error
		// and returns an empty Vec — caller observes no packets, no
		// crash, no implicit truncated output.
		let mut c = RtpCounters {
			ssrc: 0x1234,
			seq: 100,
		};
		let nal = &[0x41, 0xaa, 0xbb, 0xcc];
		let pkts = packetize_fu_a(nal, &mut c, 9000, true, 14);
		assert!(pkts.is_empty());
		// And the seq counter must NOT have been advanced — that would
		// drift the next valid call's RTP sequence numbering.
		assert_eq!(c.seq, 100);
	}

	#[test]
	fn single_nal_produces_one_packet_with_marker() {
		let mut c = RtpCounters {
			ssrc: 0x1234,
			seq: 100,
		};
		let nal = &[0x41, 0xaa, 0xbb];
		let pkt = packetize_single(nal, &mut c, 9000, true);
		assert_eq!(pkt.len(), 12 + 3);
		assert_eq!(pkt[1], 0x80 | H264_PAYLOAD_TYPE);
		assert_eq!(&pkt[12..], nal);
		assert_eq!(c.seq, 101);
	}

	#[test]
	fn single_nal_without_marker() {
		let mut c = RtpCounters {
			ssrc: 0x1234,
			seq: 100,
		};
		let pkt = packetize_single(&[0x41, 0xaa], &mut c, 9000, false);
		assert_eq!(pkt[1], H264_PAYLOAD_TYPE); // no marker bit
	}
}

#[cfg(test)]
mod fu_a_tests {
	use super::*;

	#[test]
	fn fu_a_two_fragments() {
		let mut c = RtpCounters {
			ssrc: 0x4242,
			seq: 0,
		};
		// 1-byte header (NRI=3, type=5=IDR) + 2760 bytes body → needs 2 fragments at MTU 1400
		let mut nal = vec![0x65]; // NRI=3, type=5
		nal.extend(vec![0xaa; 2760]);
		let pkts = packetize_fu_a(&nal, &mut c, 9000, true, 1400);
		assert_eq!(pkts.len(), 2);

		// First fragment: Start bit set, End bit clear, marker clear
		assert_eq!(pkts[0][12] & 0xE0, 0x60, "NRI preserved");
		assert_eq!(pkts[0][12] & 0x1F, FU_A_TYPE);
		assert_eq!(pkts[0][13] & 0x80, 0x80, "Start bit");
		assert_eq!(pkts[0][13] & 0x40, 0, "End clear");
		assert_eq!(pkts[0][13] & 0x1F, 5, "NAL type preserved");
		assert_eq!(pkts[0][1] & 0x80, 0, "no marker on first");

		// Second fragment: End bit set, marker set
		assert_eq!(pkts[1][13] & 0x80, 0, "Start clear on last");
		assert_eq!(pkts[1][13] & 0x40, 0x40, "End bit");
		assert_eq!(pkts[1][1] & 0x80, 0x80, "marker on last");
	}

	#[test]
	fn fu_a_three_fragments_middle_has_neither_bit() {
		let mut c = RtpCounters { ssrc: 1, seq: 0 };
		let mut nal = vec![0x65];
		nal.extend(vec![0xcc; 5000]);
		let pkts = packetize_fu_a(&nal, &mut c, 9000, false, 1400);
		assert_eq!(pkts.len(), 4);
		// Middle fragments: S=0, E=0
		assert_eq!(pkts[1][13] & 0xC0, 0);
		assert_eq!(pkts[2][13] & 0xC0, 0);
	}
}
