//! Frame → RTP dispatch for session send loops.

use crate::codec::h264::{self, DEFAULT_MTU};
use crate::codec::h265;
use crate::codec::VideoCodec;
use crate::provider::{AudioPayload, Frame};
use crate::rtp::RtpCounters;

/// Dispatch one access unit to a vector of RTP packets.
///
/// The caller picks the right counter before calling — video frames go
/// through the video track's counter; audio frames through the audio
/// track's. `session_task::dispatch_frame` already does this selection
/// based on frame kind.
///
/// For video, emits one or more packets (FU when needed, marker on last).
/// For audio, emits one packet per frame (AAC via RFC 3640, G.711 as
/// raw µ-law payload in an RTP packet with static PT 0).
pub fn dispatch(frame: &Frame, counters: &mut RtpCounters) -> Vec<Vec<u8>> {
	match frame {
		Frame::Video {
			codec,
			nals,
			pts_90khz,
			access_unit_end,
			..
		} => {
			let mut packets = Vec::new();
			let last_index = nals.len().saturating_sub(1);
			for (i, nal) in nals.iter().enumerate() {
				let marker = *access_unit_end && i == last_index;
				let nal_bytes: &[u8] = nal.as_ref();
				match codec {
					VideoCodec::H264 => {
						if nal_bytes.len() + 12 <= DEFAULT_MTU {
							packets.push(h264::packetize_single(
								nal_bytes, counters, *pts_90khz, marker,
							));
						} else {
							packets.extend(h264::packetize_fu_a(
								nal_bytes,
								counters,
								*pts_90khz,
								marker,
								DEFAULT_MTU,
							));
						}
					}
					VideoCodec::H265 => {
						if nal_bytes.len() + 12 <= DEFAULT_MTU {
							packets.push(h265::packetize_single(
								nal_bytes, counters, *pts_90khz, marker,
							));
						} else {
							packets.extend(h265::packetize_fu(
								nal_bytes,
								counters,
								*pts_90khz,
								marker,
								DEFAULT_MTU,
							));
						}
					}
				}
			}
			packets
		}
		Frame::Audio { payload, pts } => dispatch_audio(payload, *pts, counters),
	}
}

fn dispatch_audio(payload: &AudioPayload, pts: u32, counters: &mut RtpCounters) -> Vec<Vec<u8>> {
	use crate::codec::aac::{build_au_hbr_payload, AAC_PAYLOAD_TYPE};
	use crate::codec::g711::G711_PAYLOAD_TYPE;
	use crate::rtp::build_packet;

	match payload {
		AudioPayload::Aac { au_data, .. } => {
			let body = build_au_hbr_payload(au_data);
			let seq = counters.next_seq();
			vec![build_packet(
				AAC_PAYLOAD_TYPE,
				seq,
				pts,
				counters.ssrc,
				true,
				&body,
			)]
		}
		AudioPayload::G711Ulaw { samples } => {
			let seq = counters.next_seq();
			vec![build_packet(
				G711_PAYLOAD_TYPE,
				seq,
				pts,
				counters.ssrc,
				true,
				samples,
			)]
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::Bytes;

	#[test]
	fn dispatch_h264_single_nal_produces_one_packet() {
		let mut counters = RtpCounters { ssrc: 1, seq: 0 };
		let frame = Frame::Video {
			codec: VideoCodec::H264,
			nals: vec![Bytes::from_static(&[0x67, 0x42, 0x00, 0x1F])],
			pts_90khz: 9000,
			keyframe: true,
			access_unit_end: true,
		};
		let packets = dispatch(&frame, &mut counters);
		assert_eq!(packets.len(), 1);
		// Marker bit on single NAL that ends the access unit
		assert_eq!(packets[0][1] & 0x80, 0x80);
	}

	#[test]
	fn dispatch_h264_large_nal_produces_fu_a_fragments() {
		let mut counters = RtpCounters { ssrc: 1, seq: 0 };
		let mut big = vec![0x41];
		big.extend(vec![0xAA; 3000]);
		let frame = Frame::Video {
			codec: VideoCodec::H264,
			nals: vec![Bytes::from(big)],
			pts_90khz: 9000,
			keyframe: false,
			access_unit_end: true,
		};
		let packets = dispatch(&frame, &mut counters);
		assert!(packets.len() >= 2);
		// Last fragment has marker
		assert_eq!(packets.last().unwrap()[1] & 0x80, 0x80);
		// Earlier fragments do not
		assert_eq!(packets[0][1] & 0x80, 0);
	}

	#[test]
	fn dispatch_g711_produces_one_packet_with_static_pt0() {
		let mut counters = RtpCounters { ssrc: 2, seq: 0 };
		let frame = Frame::Audio {
			payload: AudioPayload::G711Ulaw {
				samples: Bytes::from_static(&[0xFF; 160]),
			},
			pts: 1600,
		};
		let packets = dispatch(&frame, &mut counters);
		assert_eq!(packets.len(), 1);
		assert_eq!(packets[0][1] & 0x7F, 0); // PT=0 (PCMU), marker masked off
	}

	#[test]
	fn dispatch_aac_wraps_au_in_hbr_payload() {
		let mut counters = RtpCounters { ssrc: 2, seq: 0 };
		let frame = Frame::Audio {
			payload: AudioPayload::Aac {
				au_data: Bytes::from_static(&[0xAA; 50]),
				sample_rate: 48_000,
				channels: 2,
			},
			pts: 48_000,
		};
		let packets = dispatch(&frame, &mut counters);
		assert_eq!(packets.len(), 1);
		// AU-headers-length = 16 bits at offset 12 after RTP header
		assert_eq!(packets[0][12..14], [0x00, 0x10]);
	}

	#[test]
	fn dispatch_video_without_access_unit_end_omits_marker() {
		let mut counters = RtpCounters { ssrc: 1, seq: 0 };
		let frame = Frame::Video {
			codec: VideoCodec::H264,
			nals: vec![Bytes::from_static(&[0x41, 0xAA, 0xBB])],
			pts_90khz: 9000,
			keyframe: false,
			access_unit_end: false,
		};
		let packets = dispatch(&frame, &mut counters);
		assert_eq!(packets.len(), 1);
		assert_eq!(packets[0][1] & 0x80, 0, "marker must be clear");
	}

	#[test]
	fn dispatch_h265_large_nal_fragments_via_fu() {
		// Larger than DEFAULT_MTU forces the h265::packetize_fu branch.
		let mut nal = vec![0x26, 0x01]; // IDR_W_RADL header
		nal.extend(std::iter::repeat_n(0xAB, DEFAULT_MTU * 2));
		let mut counters = RtpCounters { ssrc: 3, seq: 0 };
		let frame = Frame::Video {
			codec: VideoCodec::H265,
			nals: vec![Bytes::from(nal)],
			pts_90khz: 9000,
			keyframe: true,
			access_unit_end: true,
		};
		let packets = dispatch(&frame, &mut counters);
		assert!(
			packets.len() >= 2,
			"large H.265 NAL must fragment, got {}",
			packets.len()
		);
		// Only the last packet has the marker bit set (access_unit_end=true).
		let last = packets.last().unwrap();
		assert_eq!(last[1] & 0x80, 0x80, "final FU packet carries the marker");
		assert_eq!(
			packets[0][1] & 0x80,
			0,
			"intermediate FU packets clear the marker"
		);
	}
}
