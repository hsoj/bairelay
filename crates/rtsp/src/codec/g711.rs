//! G.711 µ-law (PCMU) encoding and decoding per ITU-T G.711.
//!
//! Used for the ADPCM → G.711 transcoding path. RTP payload type 0
//! (static), sample rate 8 kHz, mono.

/// Static RTP payload type for G.711 µ-law (PCMU), per RFC 3551.
pub const G711_PAYLOAD_TYPE: u8 = 0;
/// RTP timestamp clock rate for G.711: 8 kHz.
pub const G711_CLOCK_HZ: u32 = 8_000;
/// Number of samples per 20 ms G.711 RTP frame at 8 kHz.
pub const G711_SAMPLES_PER_FRAME: usize = 160; // 20 ms at 8 kHz

const BIAS: i16 = 0x84;
const CLIP: i16 = 32635;

/// Encode a single PCM16 sample to µ-law per ITU-T G.711.
pub fn encode_sample(pcm: i16) -> u8 {
	let mut sample = pcm;
	let sign = if sample < 0 {
		sample = -sample.max(-CLIP);
		0x80u8
	} else {
		sample = sample.min(CLIP);
		0x00u8
	};
	let biased = sample.saturating_add(BIAS);
	let exponent = log2_exp(biased as u16);
	let mantissa = ((biased >> (exponent + 3)) & 0x0F) as u8;
	let ulaw = sign | (exponent << 4) | mantissa;
	!ulaw
}

fn log2_exp(biased: u16) -> u8 {
	let mut exp = 7u8;
	let mut val = biased;
	while exp > 0 && val & 0x4000 == 0 {
		val <<= 1;
		exp -= 1;
	}
	exp
}

/// Decode a µ-law byte to PCM16.
pub fn decode_sample(ulaw: u8) -> i16 {
	let u = !ulaw;
	let sign = u & 0x80;
	let exponent = (u >> 4) & 0x07;
	let mantissa = u & 0x0F;
	let magnitude =
		((mantissa as i32) << (exponent + 3)) + (BIAS as i32) * (1 << exponent) - BIAS as i32;
	let s = magnitude.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
	if sign != 0 {
		-s
	} else {
		s
	}
}

/// Encode a PCM16 slice to µ-law.
///
/// Allocates a fresh `Vec<u8>` per call. Hot-path callers should prefer
/// [`encode_into`] which appends into a caller-managed buffer.
pub fn encode(pcm: &[i16]) -> Vec<u8> {
	let mut out = Vec::with_capacity(pcm.len());
	encode_into(pcm, &mut out);
	out
}

/// Encode a PCM16 slice to µ-law, appending into `out`. Reserves the
/// exact additional capacity required so the call grows the backing
/// buffer at most once. Caller-managed buffer means a per-frame loop
/// can reuse a single `Vec` across iterations and drop allocator
/// pressure to zero — the original `encode` is one such loop's hot
/// inner step on the ADPCM → G.711 transcode path.
pub fn encode_into(pcm: &[i16], out: &mut Vec<u8>) {
	out.reserve(pcm.len());
	for &s in pcm {
		out.push(encode_sample(s));
	}
}

/// Decode a µ-law slice to PCM16.
///
/// Allocates a fresh `Vec<i16>` per call. Hot-path callers should
/// prefer [`decode_into`].
pub fn decode(ulaw: &[u8]) -> Vec<i16> {
	let mut out = Vec::with_capacity(ulaw.len());
	decode_into(ulaw, &mut out);
	out
}

/// Decode a µ-law slice to PCM16, appending into `out`. See
/// [`encode_into`] for the allocator-pressure rationale.
pub fn decode_into(ulaw: &[u8], out: &mut Vec<i16>) {
	out.reserve(ulaw.len());
	for &b in ulaw {
		out.push(decode_sample(b));
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn silence_roundtrip() {
		assert!(decode_sample(encode_sample(0)).abs() < 256);
	}

	#[test]
	fn full_scale_positive_roundtrip() {
		let encoded = encode_sample(32000);
		let decoded = decode_sample(encoded);
		assert!((decoded - 32000).abs() < 800, "decoded={decoded}");
	}

	#[test]
	fn full_scale_negative_roundtrip() {
		let encoded = encode_sample(-32000);
		let decoded = decode_sample(encoded);
		assert!((decoded + 32000).abs() < 800, "decoded={decoded}");
	}

	#[test]
	fn known_ulaw_zero_is_0xff() {
		// PCM 0 → µ-law 0xFF (per G.711 spec — silence)
		assert_eq!(encode_sample(0), 0xFF);
	}

	#[test]
	fn encode_slice_matches_per_sample_encoder() {
		let pcm = [0i16, 1000, -1000, 32000, -32000];
		let out = encode(&pcm);
		let expected: Vec<u8> = pcm.iter().copied().map(encode_sample).collect();
		assert_eq!(out, expected);
	}

	#[test]
	fn decode_slice_matches_per_sample_decoder() {
		let ulaw = [0u8, 0x7F, 0xFF, 0x55, 0xAA];
		let out = decode(&ulaw);
		let expected: Vec<i16> = ulaw.iter().copied().map(decode_sample).collect();
		assert_eq!(out, expected);
	}

	#[test]
	fn encode_into_appends_to_existing_buffer() {
		let mut buf = vec![0xDEu8, 0xAD];
		encode_into(&[0i16, 1000, -1000], &mut buf);
		// Sentinel bytes preserved; new encoded bytes appended.
		assert_eq!(buf.len(), 2 + 3);
		assert_eq!(buf[0], 0xDE);
		assert_eq!(buf[1], 0xAD);
		assert_eq!(buf[2..], encode(&[0i16, 1000, -1000])[..]);
	}

	#[test]
	fn decode_into_appends_to_existing_buffer() {
		let mut buf = vec![123i16, -456];
		decode_into(&[0xFF, 0x80], &mut buf);
		assert_eq!(buf.len(), 2 + 2);
		assert_eq!(buf[0], 123);
		assert_eq!(buf[1], -456);
		assert_eq!(buf[2..], decode(&[0xFF, 0x80])[..]);
	}

	#[test]
	fn encode_into_reserves_capacity_in_one_shot() {
		// Reservation contract: a single `reserve(len)` call before the
		// inner loop, so a fresh empty Vec ends up with capacity ≥ len
		// after the call regardless of internal grow strategy.
		let mut buf = Vec::new();
		encode_into(&vec![0i16; 1024], &mut buf);
		assert_eq!(buf.len(), 1024);
		assert!(buf.capacity() >= 1024);
	}

	#[test]
	fn encode_matches_decode_for_all_ulaw_bytes() {
		// For every µ-law byte, decoding then re-encoding gives back the same byte.
		// Exception: per ITU-T G.711, both 0x7F (+0) and 0xFF (-0) decode to PCM 0,
		// so re-encoding the shared PCM value collapses to the canonical silence
		// code 0xFF. This is a spec-mandated ambiguity at the zero crossing.
		for ulaw in 0u8..=255 {
			let pcm = decode_sample(ulaw);
			let reenc = encode_sample(pcm);
			if ulaw == 0x7F {
				assert_eq!(reenc, 0xFF, "±0 collision: 0x7F should fold to 0xFF");
				continue;
			}
			assert_eq!(reenc, ulaw, "ulaw {:#x} → pcm {} → {:#x}", ulaw, pcm, reenc);
		}
	}
}
