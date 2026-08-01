pub(crate) mod codex;
/// Deserlizer for BCMedia
pub mod de;
/// Structure model for BCMedia
pub mod model;
/// Serlizer for BCMedia
pub mod ser;

#[cfg(test)]
mod roundtrip_tests {
	use super::model::*;
	use bytes::BytesMut;

	fn roundtrip(input: &BcMedia) -> BcMedia {
		let bytes = input.serialize(Vec::<u8>::new()).expect("serialize");
		let mut buf = BytesMut::from(bytes.as_slice());
		let out = BcMedia::deserialize(&mut buf).expect("deserialize");
		assert!(
			buf.is_empty(),
			"buffer not fully consumed after deserialize; {} bytes remain",
			buf.len()
		);
		out
	}

	#[test]
	fn roundtrip_iframe_h265() {
		let data = (0..37u8).collect::<Vec<u8>>();
		let input = BcMedia::Iframe(BcMediaIframe {
			video_type: VideoType::H265,
			microseconds: 100_000,
			time: Some(1_700_000_000),
			data: data.clone(),
		});
		match roundtrip(&input) {
			BcMedia::Iframe(out) => {
				assert!(matches!(out.video_type, VideoType::H265));
				assert_eq!(out.microseconds, 100_000);
				assert_eq!(out.time, Some(1_700_000_000));
				assert_eq!(out.data, data);
			}
			other => panic!("expected Iframe, got {other:?}"),
		}
	}

	#[test]
	fn roundtrip_iframe_h264_no_time() {
		let data = vec![0xAAu8; 16];
		let input = BcMedia::Iframe(BcMediaIframe {
			video_type: VideoType::H264,
			microseconds: 42,
			time: None,
			data: data.clone(),
		});
		match roundtrip(&input) {
			BcMedia::Iframe(out) => {
				assert!(matches!(out.video_type, VideoType::H264));
				assert_eq!(out.microseconds, 42);
				assert_eq!(out.time, None);
				assert_eq!(out.data, data);
			}
			other => panic!("expected Iframe, got {other:?}"),
		}
	}

	#[test]
	fn roundtrip_pframe_h265() {
		let data = (0..51u8).collect::<Vec<u8>>();
		let input = BcMedia::Pframe(BcMediaPframe {
			video_type: VideoType::H265,
			microseconds: 200_000,
			data: data.clone(),
		});
		match roundtrip(&input) {
			BcMedia::Pframe(out) => {
				assert!(matches!(out.video_type, VideoType::H265));
				assert_eq!(out.microseconds, 200_000);
				assert_eq!(out.data, data);
			}
			other => panic!("expected Pframe, got {other:?}"),
		}
	}

	#[test]
	fn roundtrip_aac() {
		let data = vec![0x12, 0x34, 0x56, 0x78];
		let input = BcMedia::Aac(BcMediaAac { data: data.clone() });
		match roundtrip(&input) {
			BcMedia::Aac(out) => assert_eq!(out.data, data),
			other => panic!("expected Aac, got {other:?}"),
		}
	}

	#[test]
	fn deserialize_adpcm_from_real_wire_shape() {
		// BcMedia::Adpcm does not round-trip byte-exact through our
		// own serialize: ser pads on `data.len() % 8` while de pads on
		// `(data.len() + 4) % 8` because the wire `payload_size`
		// counts the 4-byte sub-header. We therefore build the raw
		// wire bytes by hand instead of going through serialize.

		// 4 bytes of predictor state + 1024 bytes of block samples.
		const DATA_LEN: usize = 1028;
		// payload_size on the wire includes the 4-byte sub-header
		// (magic + half_block_size).
		let payload_size: u16 = (DATA_LEN as u16) + 4;
		// Padding is computed off the wire payload_size.
		let pad = match (payload_size as u32) % 8 {
			0 => 0,
			n => 8 - n,
		} as usize;
		// Half of the raw data block (sans sub-header), per the
		// camera convention.
		let half_block: u16 = ((DATA_LEN as u16) - 4) / 2;

		let mut wire = Vec::with_capacity(4 + 8 + DATA_LEN + pad);
		// MAGIC_HEADER_BCMEDIA_ADPCM, little-endian u32.
		wire.extend_from_slice(&0x62773130u32.to_le_bytes());
		// payload_size (le_u16) x2.
		wire.extend_from_slice(&payload_size.to_le_bytes());
		wire.extend_from_slice(&payload_size.to_le_bytes());
		// MAGIC_HEADER_BCMEDIA_ADPCM_DATA, little-endian u16.
		wire.extend_from_slice(&0x0100u16.to_le_bytes());
		// half-block size (le_u16).
		wire.extend_from_slice(&half_block.to_le_bytes());
		// The block itself — payload_size - 4 bytes of zeros.
		wire.extend_from_slice(&vec![0u8; DATA_LEN]);
		// Zero padding up to the 8-byte boundary.
		wire.extend_from_slice(&vec![0u8; pad]);

		let mut buf = BytesMut::from(wire.as_slice());
		match BcMedia::deserialize(&mut buf).expect("deserialize") {
			BcMedia::Adpcm(out) => {
				assert_eq!(out.data.len(), DATA_LEN);
				assert_eq!(out.block_size(), 1024);
				assert!(
					buf.is_empty(),
					"buffer not fully consumed; {} bytes remain",
					buf.len()
				);
			}
			other => panic!("expected Adpcm, got {other:?}"),
		}
	}

	#[test]
	fn roundtrip_pframe_h264_exercises_h264_ser_arm() {
		// ser.rs's bcmedia_pframe matches VideoType::H264 on one arm
		// and H265 on another; previous roundtrips only hit H265.
		let data = (0..40u8).collect::<Vec<u8>>();
		let input = BcMedia::Pframe(BcMediaPframe {
			video_type: VideoType::H264,
			microseconds: 12_345,
			data: data.clone(),
		});
		match roundtrip(&input) {
			BcMedia::Pframe(out) => {
				assert!(matches!(out.video_type, VideoType::H264));
				assert_eq!(out.microseconds, 12_345);
				assert_eq!(out.data, data);
			}
			other => panic!("expected Pframe, got {other:?}"),
		}
	}

	#[test]
	fn serialize_adpcm_zero_pad_branch() {
		// ser.rs pads based on `data.len() % 8`. When len is divisible
		// by 8 the zero-pad branch (line 85) fires. We cannot feed
		// the resulting bytes back through our own `deserialize` loop
		// (see the caveat doc on `BcMedia::serialize`), but we can
		// observe that serialization succeeds and emits the expected
		// minimum frame length: 4 magic + 8 fixed header + data bytes.
		let data = vec![0u8; 16]; // 16 % 8 == 0 — zero pad.
		let input = BcMedia::Adpcm(BcMediaAdpcm { data: data.clone() });
		let bytes = input.serialize(Vec::<u8>::new()).expect("serialize");
		assert_eq!(bytes.len(), 4 + 8 + data.len());
		// Magic.
		assert_eq!(
			u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
			0x62773130
		);
	}

	#[test]
	fn deserialize_adpcm_non_zero_pad_branch() {
		// de.rs also pads based on `payload_size % 8`, where
		// `payload_size = data.len() + 4`. Choosing data.len() so
		// that (len+4) % 8 != 0 exercises the `n => PAD_SIZE - n`
		// branch. 4 bytes of state + 4 bytes of block = 8; plus the
		// 4-byte sub-header → payload_size=12 → pad = 4.
		const DATA_LEN: usize = 8;
		let payload_size: u16 = (DATA_LEN as u16) + 4;
		let pad = match (payload_size as u32) % 8 {
			0 => 0,
			n => 8 - n,
		} as usize;
		assert!(pad > 0, "test should exercise the non-zero pad branch");
		let half_block: u16 = ((DATA_LEN as u16) - 4) / 2;

		let mut wire = Vec::with_capacity(4 + 8 + DATA_LEN + pad);
		wire.extend_from_slice(&0x62773130u32.to_le_bytes());
		wire.extend_from_slice(&payload_size.to_le_bytes());
		wire.extend_from_slice(&payload_size.to_le_bytes());
		wire.extend_from_slice(&0x0100u16.to_le_bytes());
		wire.extend_from_slice(&half_block.to_le_bytes());
		wire.extend_from_slice(&[0u8; DATA_LEN]);
		wire.extend_from_slice(&vec![0u8; pad]);

		let mut buf = BytesMut::from(wire.as_slice());
		match BcMedia::deserialize(&mut buf).expect("deserialize") {
			BcMedia::Adpcm(out) => {
				assert_eq!(out.data.len(), DATA_LEN);
				assert!(buf.is_empty());
			}
			other => panic!("expected Adpcm, got {other:?}"),
		}
	}

	#[test]
	fn roundtrip_info_v2() {
		let input = BcMedia::InfoV2(BcMediaInfoV2 {
			video_width: 2560,
			video_height: 1440,
			fps: 30,
			start_year: 121,
			start_month: 8,
			start_day: 4,
			start_hour: 23,
			start_min: 23,
			start_seconds: 52,
			end_year: 121,
			end_month: 8,
			end_day: 4,
			end_hour: 23,
			end_min: 23,
			end_seconds: 52,
		});
		match roundtrip(&input) {
			BcMedia::InfoV2(out) => {
				assert_eq!(out.video_width, 2560);
				assert_eq!(out.video_height, 1440);
				assert_eq!(out.fps, 30);
				assert_eq!(out.start_year, 121);
				assert_eq!(out.start_month, 8);
				assert_eq!(out.start_day, 4);
				assert_eq!(out.start_hour, 23);
				assert_eq!(out.start_min, 23);
				assert_eq!(out.start_seconds, 52);
				assert_eq!(out.end_year, 121);
				assert_eq!(out.end_month, 8);
				assert_eq!(out.end_day, 4);
				assert_eq!(out.end_hour, 23);
				assert_eq!(out.end_min, 23);
				assert_eq!(out.end_seconds, 52);
			}
			other => panic!("expected InfoV2, got {other:?}"),
		}
	}
}
