//! H.265 RTP packetization per RFC 7798.
//!
//! H.265 NAL headers are 2 bytes (vs H.264's 1), type is bits 1–6 of
//! byte 0. Packetization modes:
//! - Single NAL (types 0–47)
//! - FU (49): fragmentation
//!
//! Aggregation Packets (AP, type 48) are intentionally unused: outbound
//! `Frame::Video` strips VPS/SPS/PPS at the translator boundary
//! (`stream_source::handle_iframe`) and the SDP `sprop-vps/sps/pps`
//! fmtp attribute carries them out-of-band. Emitting AP would re-aggregate
//! through HA's `ffmpeg:` re-publish wrapper and exit go2rtc's frame.jpeg
//! transcoder with status 183.

use crate::rtsp::rtp::{build_packet, RtpCounters};

/// Dynamic RTP payload type used by bairelay for H.265.
pub const H265_PAYLOAD_TYPE: u8 = 96;
/// RTP timestamp clock rate for H.265: 90 kHz (shared with H.264 / MPEG-TS).
pub const H265_CLOCK_HZ: u32 = 90_000;
/// Default MTU used to size FU-A fragments; conservative for typical Ethernet links.
pub const DEFAULT_MTU: usize = 1400;

const FU_TYPE: u8 = 49;

/// Packetize a single H.265 NAL unit into one RTP packet.
pub fn packetize_single(
	nal: &[u8],
	counters: &mut RtpCounters,
	timestamp_90khz: u32,
	marker: bool,
) -> Vec<u8> {
	let seq = counters.next_seq();
	build_packet(
		H265_PAYLOAD_TYPE,
		seq,
		timestamp_90khz,
		counters.ssrc,
		marker,
		nal,
	)
}

/// FU (fragmentation unit) packetizer for H.265.
///
/// Packet structure:
/// - Replacement 2-byte NAL header with type=49
/// - 1-byte FU header: S(1) | E(1) | FuType(6) — FuType copies original NAL type
/// - Fragment payload
pub fn packetize_fu(
	nal: &[u8],
	counters: &mut RtpCounters,
	timestamp_90khz: u32,
	marker_on_last: bool,
	mtu: usize,
) -> Vec<Vec<u8>> {
	// `nal.len() >= 2` is a hard precondition (2-byte H.265 header):
	// no graceful path — callers must filter empty NALs upstream.
	assert!(nal.len() >= 2, "H.265 NAL must have 2-byte header");
	// MTU smaller than RTP (12) + NAL (2) + FU (1) = 15 bytes can't
	// emit a valid fragment. Earlier code panicked here; now we
	// surface the misconfiguration via a one-shot error log + empty
	// output. Production callers pass a fixed `DEFAULT_MTU = 1400`,
	// so this is purely defensive against a future refactor that
	// pipes operator-supplied MTU values into the function.
	if mtu <= 15 {
		tracing::error!(
			"packetize_fu: mtu={mtu} too small for RTP+NAL+FU headers (need >15); emitting nothing"
		);
		return Vec::new();
	}

	let orig_type = (nal[0] >> 1) & 0x3F;
	// H.265 NAL header: byte 0 = F | type<<1 | layer_id_msb; byte 1 = layer_id_lsb<<3 | tid_plus1.
	// For FU we swap only the type, keeping F and layer/TID identical to the original.
	let fu_payload_header_byte0 = (nal[0] & 0x81) | (FU_TYPE << 1);
	let fu_payload_header_byte1 = nal[1];

	let body = &nal[2..];
	let max_fragment_payload = mtu - 12 /* RTP */ - 2 /* NAL header */ - 1 /* FU header */;
	let mut packets = Vec::new();
	let mut offset = 0;
	let total = body.len();

	while offset < total {
		let end = (offset + max_fragment_payload).min(total);
		let is_first = offset == 0;
		let is_last = end == total;

		let mut fu_header = orig_type;
		if is_first {
			fu_header |= 0x80;
		}
		if is_last {
			fu_header |= 0x40;
		}

		let mut payload = Vec::with_capacity(3 + (end - offset));
		payload.push(fu_payload_header_byte0);
		payload.push(fu_payload_header_byte1);
		payload.push(fu_header);
		payload.extend_from_slice(&body[offset..end]);

		let seq = counters.next_seq();
		let marker = is_last && marker_on_last;
		packets.push(build_packet(
			H265_PAYLOAD_TYPE,
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
	fn packetize_fu_emits_exact_count_at_mtu_boundary() {
		// max_fragment_payload = mtu - 12 (RTP) - 2 (NAL) - 1 (FU) = mtu - 15.
		let mtu = 64usize;
		let max_payload = mtu - 15;
		let mut c = RtpCounters { ssrc: 1, seq: 0 };
		let mut nal = vec![0x26u8, 0x01]; // 2-byte H.265 header
		nal.extend(vec![0xCC; max_payload * 3]);
		let pkts = packetize_fu(&nal, &mut c, 0, true, mtu);
		assert_eq!(pkts.len(), 3, "exactly three fragments at the boundary");
	}

	#[test]
	fn packetize_fu_sequence_numbers_are_monotonic() {
		let mut c = RtpCounters {
			ssrc: 1,
			seq: 50_000,
		};
		let mut nal = vec![0x26u8, 0x01];
		nal.extend(vec![0xCC; 5000]);
		let pkts = packetize_fu(&nal, &mut c, 0, true, 1400);
		assert!(pkts.len() >= 4);
		let seqs: Vec<u16> = pkts
			.iter()
			.map(|p| u16::from_be_bytes([p[2], p[3]]))
			.collect();
		for window in seqs.windows(2) {
			assert_eq!(window[1].wrapping_sub(window[0]), 1);
		}
	}

	#[test]
	fn packetize_fu_zero_body_emits_no_packets() {
		// 2-byte H.265 NAL = header only, body = 0 bytes. The inner
		// `while offset < total` loop runs zero times, so no fragments
		// are emitted and the seq counter stays put. Documents the
		// silent-no-op contract for callers that pass header-only NALs.
		let mut c = RtpCounters { ssrc: 1, seq: 999 };
		let nal = &[0x26u8, 0x01]; // header only
		let pkts = packetize_fu(nal, &mut c, 0, true, 1400);
		assert!(pkts.is_empty());
		assert_eq!(c.seq, 999);
	}

	#[test]
	fn packetize_fu_with_too_small_mtu_emits_nothing() {
		// Used to panic via assert!(mtu > 15, ...). Now logs and
		// returns empty Vec; counter must remain unadvanced.
		let mut c = RtpCounters { ssrc: 1, seq: 50 };
		let nal = &[0x26, 0x01, 0xaa, 0xbb];
		let pkts = packetize_fu(nal, &mut c, 0, true, 15);
		assert!(pkts.is_empty());
		assert_eq!(c.seq, 50);
	}

	#[test]
	fn single_nal_packet() {
		let mut c = RtpCounters { ssrc: 1, seq: 0 };
		let nal = &[0x26, 0x01, 0xaa, 0xbb]; // IDR
		let pkt = packetize_single(nal, &mut c, 0, true);
		assert_eq!(pkt[1], 0x80 | H265_PAYLOAD_TYPE);
		assert_eq!(&pkt[12..], nal);
	}

	#[test]
	fn fu_fragments_large_idr() {
		let mut c = RtpCounters { ssrc: 1, seq: 0 };
		let mut nal = vec![0x26, 0x01]; // IDR_W_RADL, layer=0, tid=0, plus1=1
		nal.extend(vec![0xcc; 5000]);
		let pkts = packetize_fu(&nal, &mut c, 0, true, 1400);
		assert!(pkts.len() >= 4);
		// FU type in payload byte 0 (bits 1-6) is 49
		assert_eq!((pkts[0][12] >> 1) & 0x3F, FU_TYPE);
		// Start bit on first
		assert_eq!(pkts[0][14] & 0x80, 0x80);
		// End bit on last
		let last = pkts.len() - 1;
		assert_eq!(pkts[last][14] & 0x40, 0x40);
		// Marker bit only on last
		assert_eq!(pkts[0][1] & 0x80, 0);
		assert_eq!(pkts[last][1] & 0x80, 0x80);
	}

	#[test]
	fn fu_middle_fragment_has_neither_start_nor_end() {
		let mut c = RtpCounters { ssrc: 1, seq: 0 };
		let mut nal = vec![0x26, 0x01];
		nal.extend(vec![0xcc; 5000]);
		let pkts = packetize_fu(&nal, &mut c, 0, false, 1400);
		assert!(pkts.len() >= 3, "expected at least 3 fragments");
		// Middle fragments: S=0, E=0
		for pkt in &pkts[1..pkts.len() - 1] {
			assert_eq!(
				pkt[14] & 0x80,
				0,
				"middle fragment should not have Start bit"
			);
			assert_eq!(pkt[14] & 0x40, 0, "middle fragment should not have End bit");
		}
	}
}
