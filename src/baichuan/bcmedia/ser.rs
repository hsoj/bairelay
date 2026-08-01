use super::model::*;
use crate::baichuan::Error;
use cookie_factory::bytes::*;
use cookie_factory::sequence::tuple;
use cookie_factory::SerializeFn;
use cookie_factory::{combinator::*, gen};
use std::io::Write;

// PAD_SIZE: Media packets use 8 byte padding
const PAD_SIZE: u32 = 8;

impl BcMedia {
	/// Serialize a single [`BcMedia`] variant to a writer.
	///
	/// Emits the on-wire Baichuan media frame layout: a little-endian
	/// 32-bit magic header, the per-variant fixed header, the payload
	/// bytes, then zero-byte padding up to an 8-byte boundary of the
	/// payload length where applicable. Each variant of [`BcMedia`]
	/// has its own magic header; consult the variant's description
	/// for the wire-format details. Exactly one `BcMedia` variant is
	/// written per call.
	///
	/// Following the `cookie-factory` convention, the same writer `W`
	/// that was passed in is returned on success so callers can chain
	/// writes.
	///
	/// # Known wire-format caveat (ADPCM)
	///
	/// [`BcMedia::Adpcm`] does not currently round-trip byte-exact
	/// through [`BcMedia::deserialize`] because the serializer pads
	/// based on `data.len() % 8` while the deserializer pads based on
	/// `(data.len() + 4) % 8` (the sub-header is counted in the wire
	/// `payload_size`). A fixture replay that concatenates serialized
	/// ADPCM frames and calls `deserialize` in a loop will desync; a
	/// replay that feeds the original camera bytes directly to
	/// `deserialize` is fine. Tracked for future upstream alignment.
	pub fn serialize<W: Write>(&self, buf: W) -> Result<W, Error> {
		let (buf, _) = match &self {
			BcMedia::InfoV1(payload) => gen(bcmedia_info_v1(payload), buf)?,
			BcMedia::InfoV2(payload) => gen(bcmedia_info_v2(payload), buf)?,
			BcMedia::Iframe(payload) => {
				let pad_size = match payload.data.len() as u32 % PAD_SIZE {
					0 => 0,
					n => PAD_SIZE - n,
				};
				gen(
					tuple((
						bcmedia_iframe(payload),
						slice(&payload.data),
						slice(&vec![0; pad_size as usize]),
					)),
					buf,
				)?
			}
			BcMedia::Pframe(payload) => {
				let pad_size = match payload.data.len() as u32 % PAD_SIZE {
					0 => 0,
					n => PAD_SIZE - n,
				};
				gen(
					tuple((
						bcmedia_pframe(payload),
						slice(&payload.data),
						slice(&vec![0; pad_size as usize]),
					)),
					buf,
				)?
			}
			BcMedia::Aac(payload) => {
				let pad_size = match payload.data.len() as u32 % PAD_SIZE {
					0 => 0,
					n => PAD_SIZE - n,
				};
				gen(
					tuple((
						bcmedia_aac(payload),
						slice(&payload.data),
						slice(&vec![0; pad_size as usize]),
					)),
					buf,
				)?
			}
			BcMedia::Adpcm(payload) => {
				let pad_size = match payload.data.len() as u32 % PAD_SIZE {
					0 => 0,
					n => PAD_SIZE - n,
				};
				gen(
					tuple((
						bcmedia_adpcm(payload),
						slice(&payload.data),
						slice(&vec![0; pad_size as usize]),
					)),
					buf,
				)?
			}
		};

		Ok(buf)
	}
}

fn bcmedia_info_v1<W: Write>(payload: &BcMediaInfoV1) -> impl SerializeFn<W> {
	tuple((
		le_u32(MAGIC_HEADER_BCMEDIA_INFO_V1),
		le_u32(32),
		le_u32(payload.video_width),
		le_u32(payload.video_height),
		le_u8(0), // unknown. Known values 00/01
		le_u8(payload.fps),
		le_u8(payload.start_year),
		le_u8(payload.start_month),
		le_u8(payload.start_day),
		le_u8(payload.start_hour),
		le_u8(payload.start_min),
		le_u8(payload.start_seconds),
		le_u8(payload.end_year),
		le_u8(payload.end_month),
		le_u8(payload.end_day),
		le_u8(payload.end_hour),
		le_u8(payload.end_min),
		le_u8(payload.end_seconds),
		le_u8(0),
		le_u8(0),
	))
}

fn bcmedia_info_v2<W: Write>(payload: &BcMediaInfoV2) -> impl SerializeFn<W> {
	tuple((
		le_u32(MAGIC_HEADER_BCMEDIA_INFO_V2),
		le_u32(32),
		le_u32(payload.video_width),
		le_u32(payload.video_height),
		le_u8(0), // unknown. Known values 00/01
		le_u8(payload.fps),
		le_u8(payload.start_year),
		le_u8(payload.start_month),
		le_u8(payload.start_day),
		le_u8(payload.start_hour),
		le_u8(payload.start_min),
		le_u8(payload.start_seconds),
		le_u8(payload.end_year),
		le_u8(payload.end_month),
		le_u8(payload.end_day),
		le_u8(payload.end_hour),
		le_u8(payload.end_min),
		le_u8(payload.end_seconds),
		le_u8(0),
		le_u8(0),
	))
}

fn bcmedia_iframe<W: Write>(payload: &BcMediaIframe) -> impl SerializeFn<W> {
	// Cookie String needs a static lifetime
	let vid_string = match payload.video_type {
		VideoType::H264 => "H264",
		VideoType::H265 => "H265",
	};
	let (extra_header, extra_header_size) = if let Some(payload_time) = payload.time {
		// `gen` writes two u32 LE values into a `Vec<u8>`; `Vec<u8>`'s
		// `Write` impl is infallible, so the only way this can fail is
		// a cookie-factory internal-state error that does not occur for
		// fixed-size primitive serializers. `expect` documents the
		// reasoning rather than `unwrap`'s silent-trust.
		let extra_header = slice(
			gen(tuple((le_u32(payload_time), le_u32(0))), vec![])
				.expect("infallible: gen of fixed le_u32 tuple into Vec<u8> cannot fail")
				.0,
		);
		let extra_header_size = 8;
		(extra_header, extra_header_size)
	} else {
		let extra_header = slice(vec![]);
		let extra_header_size = 0;
		(extra_header, extra_header_size)
	};
	tuple((
		le_u32(MAGIC_HEADER_BCMEDIA_IFRAME),
		string(vid_string),
		le_u32(payload.data.len() as u32),
		le_u32(extra_header_size), //  unknown. NVR channel count? Known values 1-00/08 2-00 3-00 4-00
		le_u32(payload.microseconds),
		le_u32(0), // unknown. Known values 1-00/23/5A 2-00 3-00 4-00
		extra_header,
	))
}

fn bcmedia_pframe<W: Write>(payload: &BcMediaPframe) -> impl SerializeFn<W> {
	// Cookie String needs a static lifetime
	let vid_string = match payload.video_type {
		VideoType::H264 => "H264",
		VideoType::H265 => "H265",
	};
	tuple((
		le_u32(MAGIC_HEADER_BCMEDIA_PFRAME),
		string(vid_string),
		le_u32(payload.data.len() as u32),
		le_u32(0), //  unknown. NVR channel count? Known values 1-00/08 2-00 3-00 4-00
		le_u32(payload.microseconds),
		le_u32(0), // unknown. Known values 1-00/23/5A 2-00 3-00 4-00
	))
}

fn bcmedia_aac<W: Write>(payload: &BcMediaAac) -> impl SerializeFn<W> {
	tuple((
		le_u32(MAGIC_HEADER_BCMEDIA_AAC),
		le_u16(payload.data.len() as u16),
		le_u16(payload.data.len() as u16),
	))
}

fn bcmedia_adpcm<W: Write>(payload: &BcMediaAdpcm) -> impl SerializeFn<W> {
	tuple((
		le_u32(MAGIC_HEADER_BCMEDIA_ADPCM),
		le_u16((payload.data.len() + 4) as u16), // Payload + 2 byte magic + 2byte block size
		le_u16((payload.data.len() + 4) as u16), // Payload + 2 byte magic + 2byte block size
		le_u16(MAGIC_HEADER_BCMEDIA_ADPCM_DATA), // magic
		le_u16(((payload.data.len() - 4) / 2) as u16), // Block size without the header halved
	))
}
