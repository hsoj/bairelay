//! AAC ADTS parsing and MPEG-4 AudioSpecificConfig generation.

/// AAC sampling frequency index (per MPEG-4 audio).
pub fn sample_rate_index(hz: u32) -> Option<u8> {
	match hz {
		96000 => Some(0),
		88200 => Some(1),
		64000 => Some(2),
		48000 => Some(3),
		44100 => Some(4),
		32000 => Some(5),
		24000 => Some(6),
		22050 => Some(7),
		16000 => Some(8),
		12000 => Some(9),
		11025 => Some(10),
		8000 => Some(11),
		7350 => Some(12),
		_ => None,
	}
}

/// Build the 2-byte AudioSpecificConfig hex string used in SDP `fmtp config=`.
///
/// Format (13 bits total, packed into 2 bytes + trailing zeros):
/// - 5 bits audioObjectType (1..=29 valid; 30 reserved; 31 is the
///   escape sequence that signals a 6-bit extension — not supported
///   here because the wire shape becomes 14+ bits and our 16-bit
///   container overflows)
/// - 4 bits samplingFrequencyIndex
/// - 4 bits channelConfiguration (1..=7; 0 = PCE-specified, also
///   rejected: SDP can't carry the program config element in the
///   `config=` attribute, and downstream RTP players would render
///   "0 channels" which is a no-op stream)
///
/// Returns the hex-encoded string (e.g. "1190" for AAC-LC 48 kHz stereo).
pub fn build_audio_specific_config_hex(aot: u8, sample_rate: u32, channels: u8) -> Option<String> {
	if !(1..=29).contains(&aot) {
		return None;
	}
	let sr_idx = sample_rate_index(sample_rate)?;
	if channels == 0 || channels > 7 {
		return None;
	}
	let bits = ((aot as u32 & 0x1F) << 11)
		| ((sr_idx as u32 & 0x0F) << 7)
		| ((channels as u32 & 0x0F) << 3);
	let byte0 = (bits >> 8) as u8;
	let byte1 = bits as u8;
	Some(format!("{:02X}{:02X}", byte0, byte1))
}

/// Minimal ADTS header parse (7 bytes, no CRC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdtsHeader {
	/// Audio Object Type (e.g. 2 for AAC-LC).
	pub aot: u8,
	/// Sampling rate in Hz.
	pub sample_rate: u32,
	/// Channel configuration (1 = mono, 2 = stereo, ...).
	pub channels: u8,
	/// Total frame length in bytes, including the ADTS header.
	pub frame_length: usize,
	/// Number of raw AAC frames carried in this ADTS packet (1..=4).
	/// ADTS encodes `n - 1` in the low 2 bits of byte 6. Each AAC frame
	/// is exactly the codec's per-AU sample count (1024 for AAC-LC at
	/// 16 kHz; 2048 for HE-AAC). When the camera packs multiple frames
	/// in one ADTS packet, the RTP timestamp must advance by
	/// `aac_frames * samples_per_au` per packet, not by a single AU.
	pub aac_frames: u8,
}

/// Parse an ADTS header from the start of a buffer. Returns header + body offset.
pub fn parse_adts(buf: &[u8]) -> Option<AdtsHeader> {
	if buf.len() < 7 {
		return None;
	}
	// Sync word 0xFFF in first 12 bits
	if buf[0] != 0xFF || (buf[1] & 0xF0) != 0xF0 {
		return None;
	}
	let profile = (buf[2] >> 6) & 0x03;
	let aot = profile + 1; // ADTS profile = AOT - 1
	let sr_idx = (buf[2] >> 2) & 0x0F;
	let channels_high = (buf[2] & 0x01) << 2;
	let channels_low = (buf[3] >> 6) & 0x03;
	let channels = channels_high | channels_low;
	let frame_length = (((buf[3] as usize) & 0x03) << 11)
		| ((buf[4] as usize) << 3)
		| (((buf[5] as usize) >> 5) & 0x07);
	// frame_length carries header-included total length (ISO/IEC
	// 13818-7 §6.2). A value < 7 means the encoder is claiming the
	// header is shorter than the ADTS minimum — malformed input we
	// can't safely strip-and-forward downstream. handle_aac has its
	// own defensive check, but rejecting at parse time keeps every
	// ADTS-bearing call site (test, fuzz, future consumers) honest.
	if frame_length < ADTS_HEADER_LEN {
		return None;
	}
	let sample_rates = [
		96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
	];
	let sample_rate = *sample_rates.get(sr_idx as usize)?;
	// `number_of_raw_data_blocks_in_frame` is the low 2 bits of byte 6;
	// the actual count is that value + 1 per ISO/IEC 13818-7 §6.2.
	let aac_frames = (buf[6] & 0x03) + 1;
	Some(AdtsHeader {
		aot,
		sample_rate,
		channels,
		frame_length,
		aac_frames,
	})
}

/// Length of a minimal (no-CRC) ADTS header.
pub const ADTS_HEADER_LEN: usize = 7;

/// Build a single AAC-hbr RTP payload (RFC 3640): 4-byte AU-header section
/// (sizelength=13 + indexlength=3) + AU data.
pub fn build_au_hbr_payload(au: &[u8]) -> Vec<u8> {
	let au_header = ((au.len() as u16) << 3).to_be_bytes();
	let mut out = Vec::with_capacity(4 + au.len());
	out.extend_from_slice(&[0x00, 0x10]);
	out.extend_from_slice(&au_header);
	out.extend_from_slice(au);
	out
}

/// RTP payload type for AAC (dynamic; we pick 97 by convention).
pub const AAC_PAYLOAD_TYPE: u8 = 97;

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn config_hex_aac_lc_48k_stereo() {
		assert_eq!(
			build_audio_specific_config_hex(2, 48000, 2),
			Some("1190".to_string())
		);
	}

	#[test]
	fn config_hex_aac_lc_16k_mono() {
		// AOT=2, SR_idx=8 (16000), channels=1
		assert_eq!(
			build_audio_specific_config_hex(2, 16000, 1),
			Some("1408".to_string())
		);
	}

	#[test]
	fn config_hex_invalid_sample_rate() {
		assert_eq!(build_audio_specific_config_hex(2, 9000, 2), None);
	}

	#[test]
	fn parses_known_adts_header() {
		// ADTS: profile=1 (AOT=2 AAC-LC), sr_idx=4 (44100), channels=2, frame_len=100
		// byte0=0xFF, byte1=0xF1 (MPEG-4, no CRC), byte2=0x50 (profile 1, sr_idx 4, ch_high=0)
		// byte3=0x80 | (ch_low=2<<6=0x80) | ((frame_len>>11)&3)=0 → 0x80
		// byte4=(frame_len>>3)=0x0C, byte5=((frame_len&7)<<5)=0x80, byte6=0x00
		let buf = &[0xFF, 0xF1, 0x50, 0x80, 0x0C, 0x80, 0x00];
		let h = parse_adts(buf).unwrap();
		assert_eq!(h.aot, 2);
		assert_eq!(h.sample_rate, 44100);
		assert_eq!(h.channels, 2);
		assert_eq!(h.frame_length, 100);
	}

	#[test]
	fn parse_adts_rejects_bad_sync() {
		assert_eq!(parse_adts(&[0xFE, 0xF1, 0, 0, 0, 0, 0]), None);
	}

	#[test]
	fn build_au_hbr_payload_correct_shape() {
		let au = &[0xAA; 100];
		let payload = build_au_hbr_payload(au);
		// 2 bytes headers-length + 2 bytes AU-header + 100 bytes AU
		assert_eq!(payload.len(), 4 + 100);
		// AU-headers-length = 16
		assert_eq!(u16::from_be_bytes([payload[0], payload[1]]), 16);
		// AU-size = 100 in top 13 bits
		let au_header = u16::from_be_bytes([payload[2], payload[3]]);
		assert_eq!(au_header >> 3, 100);
	}

	#[test]
	fn sample_rate_index_covers_all_known_rates() {
		// Each MPEG-4 sample rate must map to its canonical index.
		let table: &[(u32, u8)] = &[
			(96000, 0),
			(88200, 1),
			(64000, 2),
			(48000, 3),
			(44100, 4),
			(32000, 5),
			(24000, 6),
			(22050, 7),
			(16000, 8),
			(12000, 9),
			(11025, 10),
			(8000, 11),
			(7350, 12),
		];
		for (hz, idx) in table {
			assert_eq!(sample_rate_index(*hz), Some(*idx), "rate {hz}");
		}
	}

	#[test]
	fn sample_rate_index_rejects_unknown_rate() {
		assert_eq!(sample_rate_index(1234), None);
	}

	#[test]
	fn config_hex_rejects_too_many_channels() {
		// channels > 7 is outside the 4-bit channelConfiguration field we emit.
		assert_eq!(build_audio_specific_config_hex(2, 48000, 8), None);
	}

	#[test]
	fn config_hex_rejects_zero_channels_pce_specified() {
		// channels=0 = PCE-specified config; we can't render it to SDP
		// `config=` (the PCE lives inside the AAC body, not here).
		assert_eq!(build_audio_specific_config_hex(2, 48000, 0), None);
	}

	#[test]
	fn config_hex_rejects_aot_zero_and_above_29() {
		// MPEG-4 valid AOTs are 1..=29; 30 is reserved, 31 is the
		// 6-bit-extension escape that overflows our 16-bit container.
		assert_eq!(build_audio_specific_config_hex(0, 48000, 2), None);
		assert_eq!(build_audio_specific_config_hex(30, 48000, 2), None);
		assert_eq!(build_audio_specific_config_hex(31, 48000, 2), None);
		// Sanity: every legal AOT still resolves.
		for aot in 1u8..=29 {
			assert!(build_audio_specific_config_hex(aot, 48000, 2).is_some());
		}
	}

	#[test]
	fn parse_adts_rejects_frame_length_below_header() {
		// Frame_length is encoded across bytes 3-5: 2 bits (3) + 8 bits
		// (4) + 3 bits (5). Construct a header with frame_length=5 (less
		// than ADTS_HEADER_LEN=7) and confirm parse rejects it. byte3=0,
		// byte4=0, byte5=(5&7)<<5 = 0xA0 → frame_length = 0|0|5 = 5.
		let buf = &[0xFF, 0xF1, 0x50, 0x80, 0x00, 0xA0, 0x00];
		assert_eq!(parse_adts(buf), None);
	}

	#[test]
	fn parse_adts_rejects_short_buffer() {
		assert_eq!(parse_adts(&[0xFF, 0xF1, 0x50]), None);
	}

	#[test]
	fn build_au_hbr_payload_handles_empty_au() {
		let payload = build_au_hbr_payload(&[]);
		// Still emits the 4-byte AU-header section; AU body empty.
		assert_eq!(payload.len(), 4);
		assert_eq!(u16::from_be_bytes([payload[0], payload[1]]), 16);
		assert_eq!(u16::from_be_bytes([payload[2], payload[3]]), 0);
	}

	// Property tests: parse_adts must safely return None for any byte
	// input it can't recognise as a valid ADTS header. build_au_hbr_payload
	// accepts arbitrary bytes (it's a serialiser, not a parser).
	use proptest::prelude::*;

	proptest! {
		#![proptest_config(ProptestConfig {
			cases: 256,
			..ProptestConfig::default()
		})]

		#[test]
		fn parse_adts_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..128)) {
			let _ = parse_adts(&bytes);
		}

		#[test]
		fn parse_adts_with_valid_sync_never_panics(
			bits in proptest::collection::vec(any::<u8>(), 6..32),
		) {
			// Force the 12-bit sync word `0xFFF` at the front so the
			// parser walks past the early-reject path and into the
			// per-field validation branches.
			let mut buf = vec![0xFFu8, 0xF0u8 | (bits[0] & 0x0F)];
			buf.extend_from_slice(&bits[1..]);
			let _ = parse_adts(&buf);
		}

		#[test]
		fn build_au_hbr_payload_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
			let _ = build_au_hbr_payload(&bytes);
		}
	}
}
