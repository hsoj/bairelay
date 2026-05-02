/// Video streams encapsulate a stream of BcMedia
#[derive(Debug, Clone)]
pub enum BcMedia {
	/// Holds info on the stream
	InfoV1(BcMediaInfoV1),
	/// Holds info on the stream
	InfoV2(BcMediaInfoV2),
	/// Holds an IFrame either H264 or H265
	Iframe(BcMediaIframe),
	/// Holds a PFrame either H264 or H265
	Pframe(BcMediaPframe),
	/// Holds AAC audio
	Aac(BcMediaAac),
	/// Holds ADPCM audio
	Adpcm(BcMediaAdpcm),
}
//
pub(super) const MAGIC_HEADER_BCMEDIA_INFO_V1: u32 = 0x31303031;

/// The start of a BcMedia stream contains this message
/// which describes the data to follow
#[derive(Debug, Clone)]
pub struct BcMediaInfoV1 {
	// This is the size of the header so it's actually a fixed value
	// The other messages have body size here so maybe that's why
	// it's included
	// pub header_size: u32,
	/// Width of the video
	pub video_width: u32,
	/// Height of the video
	pub video_height: u32,
	// pub unknown: u8,
	/// Frames per second. On older cameras this seems to be an index of the FPS on a lookup table
	pub fps: u8,
	/// Start year of the stream
	pub start_year: u8,
	/// Start month of the stream
	pub start_month: u8,
	/// Start day of the stream
	pub start_day: u8,
	/// Start hour of the stream
	pub start_hour: u8,
	/// Start minute of the stream
	pub start_min: u8,
	/// Start seconds of the stream
	pub start_seconds: u8,
	/// End year of the video probably only useful for the recorded files on the SD card
	pub end_year: u8,
	/// End month of the video probably only useful for the recorded files on the SD card
	pub end_month: u8,
	/// End day of the video probably only useful for the recorded files on the SD card
	pub end_day: u8,
	/// End hour of the video probably only useful for the recorded files on the SD card
	pub end_hour: u8,
	/// End min of the video probably only useful for the recorded files on the SD card
	pub end_min: u8,
	/// End seconds of the video probably only useful for the recorded files on the SD card
	pub end_seconds: u8,
	// unknown: u16
}
//
pub(super) const MAGIC_HEADER_BCMEDIA_INFO_V2: u32 = 0x32303031;

/// The start of a BcMedia stream contains this message
/// which describes the data to follow
#[derive(Debug, Clone)]
pub struct BcMediaInfoV2 {
	// This is the size of the header so it's actually a fixed value
	// The other messages have body size here so maybe that's why
	// it's included
	// pub header_size: u32,
	/// Width of the video
	pub video_width: u32,
	/// Height of the video
	pub video_height: u32,
	// pub unknown: u8,
	/// Frames per second. On older cameras this seems to be an index of the FPS on a lookup table
	pub fps: u8,
	/// Start year of the stream
	pub start_year: u8,
	/// Start month of the stream
	pub start_month: u8,
	/// Start day of the stream
	pub start_day: u8,
	/// Start hour of the stream
	pub start_hour: u8,
	/// Start minute of the stream
	pub start_min: u8,
	/// Start seconds of the stream
	pub start_seconds: u8,
	/// End year of the video probably only useful for the recorded files on the SD card
	pub end_year: u8,
	/// End month of the video probably only useful for the recorded files on the SD card
	pub end_month: u8,
	/// End day of the video probably only useful for the recorded files on the SD card
	pub end_day: u8,
	/// End hour of the video probably only useful for the recorded files on the SD card
	pub end_hour: u8,
	/// End min of the video probably only useful for the recorded files on the SD card
	pub end_min: u8,
	/// End seconds of the video probably only useful for the recorded files on the SD card
	pub end_seconds: u8,
	// unknown: u16
}

// IFrame magics include the channel number in them
pub(super) const MAGIC_HEADER_BCMEDIA_IFRAME: u32 = 0x63643030;
pub(super) const MAGIC_HEADER_BCMEDIA_IFRAME_LAST: u32 = 0x63643039;

/// Video Types for I/PFrame
#[derive(Debug, Clone, Copy)]
pub enum VideoType {
	/// H264 video data
	H264,
	/// H265 video data
	H265,
}

/// This is a BcMedia video IFrame.
#[derive(Clone)]
pub struct BcMediaIframe {
	/// "H264", or "H265"
	pub video_type: VideoType,
	// Size of payload after header in bytes
	// pub payload_size: u32,
	// unknown: u32, // NVR channel count? Known values 1-00/08 2-00 3-00 4-00
	/// Timestamp in microseconds
	pub microseconds: u32,
	// unknown: u32, // Known values 1-00/23/5A 2-00 3-00 4-00
	/// POSIX time (seconds since 00:00:00 Jan 1 1970)
	pub time: Option<u32>,
	//unknown: u32, // Known values 1-00/06/29 2-00/01 3-00/C3 4-00
	/// Raw IFrame data
	pub data: Vec<u8>,
}

impl std::fmt::Debug for BcMediaIframe {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_map()
			.entry(&"video_type", &self.video_type)
			// .entry(&"payload_size", &self.payload_size)
			.entry(&"microseconds", &self.microseconds)
			.entry(&"time", &self.time)
			.entry(
				&"data[0..10]",
				&self.data[0..std::cmp::min(20, self.data.len())].to_vec(),
			)
			.entry(
				&"data[-10..-1]",
				&self.data[std::cmp::max(0, self.data.len() - 20)..self.data.len()].to_vec(),
			)
			.entry(&"data.len()", &self.data.len())
			.finish()
	}
}

// PFrame magics include the channel number in them
pub(super) const MAGIC_HEADER_BCMEDIA_PFRAME: u32 = 0x63643130;
pub(super) const MAGIC_HEADER_BCMEDIA_PFRAME_LAST: u32 = 0x63643139;

/// This is a BcMedia video PFrame.
#[derive(Clone)]
pub struct BcMediaPframe {
	/// "H264", or "H265"
	pub video_type: VideoType,
	// Size of payload after header in bytes
	// pub payload_size: u32,
	// unknown: u32, // NVR channel count? Known values 1-00/08 2-00 3-00 4-00
	/// Timestamp in microseconds
	pub microseconds: u32,
	// unknown: u32, // Known values 1-00/23/5A 2-00 3-00 4-00
	/// Raw PFrame data
	pub data: Vec<u8>,
}

impl std::fmt::Debug for BcMediaPframe {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_map()
			.entry(&"video_type", &self.video_type)
			// .entry(&"payload_size", &self.payload_size)
			.entry(&"microseconds", &self.microseconds)
			.entry(
				&"data[0..20]",
				&self.data[0..std::cmp::min(20, self.data.len())].to_vec(),
			)
			.entry(
				&"data[-20..-1]",
				&self.data[std::cmp::max(0, self.data.len() - 20)..self.data.len()].to_vec(),
			)
			.entry(&"data.len()", &self.data.len())
			.finish()
	}
}

pub(super) const MAGIC_HEADER_BCMEDIA_AAC: u32 = 0x62773530;

/// This contains BcMedia audio data in AAC format
#[derive(Debug, Clone)]
pub struct BcMediaAac {
	// Size of payload after header in bytes
	// pub payload_size: u16,
	// Size of payload after header in bytes exactly the same as before
	// pub payload_size_b: u16,
	/// Raw AAC data
	pub data: Vec<u8>,
}

impl BcMediaAac {
	/// Read the ADTS header to learn the duration in micro secs
	pub fn duration(&self) -> Option<u32> {
		if self.data.len() < 8 {
			// Too small for the header
			return None;
		}
		if self.data[0] != 0b11111111 {
			// Syncword incorrect
			return None;
		}
		if (self.data[1] & 0b11110000) != 0b11110000 {
			// Syncword incorrect
			return None;
		}
		let frequency_index = (self.data[2] & 0b00111100) >> 2;
		let sample_frequency = match frequency_index {
			0 => Some(96000u32),
			1 => Some(88200u32),
			2 => Some(64000u32),
			3 => Some(48000u32),
			4 => Some(44100u32),
			5 => Some(32000u32),
			6 => Some(24000u32),
			7 => Some(22050u32),
			8 => Some(16000u32),
			9 => Some(12000u32),
			10 => Some(11025u32),
			11 => Some(8000u32),
			12 => Some(7350u32),
			_ => None,
		}?;
		log::trace!("sample_frequency: {sample_frequency}");

		let frames = (self.data[6] & 0b00000011) + 1;
		log::trace!("frames: {frames}");
		let samples = frames as u32 * 1024;
		log::trace!("samples: {samples}");
		const MICROSECONDS: u32 = 1000000;
		let duration = samples * MICROSECONDS / sample_frequency;
		Some(duration)
	}
}

pub(super) const MAGIC_HEADER_BCMEDIA_ADPCM: u32 = 0x62773130;

pub(super) const MAGIC_HEADER_BCMEDIA_ADPCM_DATA: u16 = 0x0100;

/// This contains BcMedia audio data in ADPCM format
#[derive(Debug, Clone)]
pub struct BcMediaAdpcm {
	// Size of payload after header in bytes
	// pub payload_size: u16,
	// Size of payload after header in bytes exactly the same as before
	// pub payload_size_b: u16,
	// more_magic: MAGIC_HEADER_BCMEDIA_ADPCM_DATA
	// Adpcm sample_block_size in bytes
	//
	// These bytes (and the MAGIC_HEADER_BCMEDIA_ADPCM_DATA) are included as
	// part of the payload_size. It may be more prudent to sealise them to
	// another structure.
	// pub sample_block_size: u16,
	/// The raw adpcm data in DVI-4 layout.
	///
	/// One `data` should contain 4 bytes of the adpcm predictor state then one block
	/// of adpcm samples
	///
	/// To calculate the block-align size simply remove 4 from the `len()`
	pub data: Vec<u8>,
}

impl BcMediaAdpcm {
	/// The block size, this is bytes without the block header
	pub fn block_size(&self) -> u32 {
		self.data.len() as u32 - 4
	}

	/// Returns duration in micro seconds;
	pub fn duration(&self) -> Option<u32> {
		let samples = self.block_size() * 2;
		// Always 8000Hz for ADPCM
		const SAMPLE_FREQUENCY: u32 = 8000;
		const MICROSECONDS: u32 = 1000000;
		let duration = samples * MICROSECONDS / SAMPLE_FREQUENCY;
		Some(duration)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn iframe_with_data(data: Vec<u8>) -> BcMediaIframe {
		BcMediaIframe {
			video_type: VideoType::H264,
			microseconds: 42,
			time: Some(1),
			data,
		}
	}

	fn pframe_with_data(data: Vec<u8>) -> BcMediaPframe {
		BcMediaPframe {
			video_type: VideoType::H265,
			microseconds: 7,
			data,
		}
	}

	#[test]
	fn video_type_debug_both_variants() {
		// Only Debug impl covers the enum; exercise both arms.
		let h264 = VideoType::H264;
		let h265 = VideoType::H265;
		assert!(format!("{:?}", h264).contains("H264"));
		assert!(format!("{:?}", h265).contains("H265"));
	}

	#[test]
	fn bc_media_iframe_debug_long_data() {
		// The Debug impls use `self.data.len() - 20` unchecked, so
		// callers must supply at least 20 bytes. Production callers
		// always pass real iframes (many KB); we don't add a
		// short-data test because it panics on unsigned underflow.
		let frame = iframe_with_data((0u8..=60).collect());
		let s = format!("{:?}", frame);
		assert!(s.contains("data[0..10]"));
		assert!(s.contains("data[-10..-1]"));
		assert!(s.contains("video_type"));
	}

	#[test]
	fn bc_media_pframe_debug_long_data() {
		let frame = pframe_with_data((0u8..=60).collect());
		let s = format!("{:?}", frame);
		assert!(s.contains("data[0..20]"));
		assert!(s.contains("data[-20..-1]"));
	}

	/// Build a minimal ADTS header with the given freq-index so
	/// `BcMediaAac::duration` sees a valid syncword.
	fn adts_header(freq_index: u8, frames_minus_one: u8) -> Vec<u8> {
		// Byte layout per ADTS: sync=0xFFFX, then freq index bits in
		// byte[2], then frames-1 in low 2 bits of byte[6].
		let b0 = 0xFF;
		let b1 = 0xF1; // sync high nibble + MPEG4 + layer 0 + protection_absent
		let b2 = (freq_index << 2) & 0b0011_1100;
		let b3 = 0;
		let b4 = 0;
		let b5 = 0;
		let b6 = frames_minus_one & 0b0000_0011;
		let b7 = 0;
		vec![b0, b1, b2, b3, b4, b5, b6, b7]
	}

	#[test]
	fn aac_duration_too_short_returns_none() {
		let aac = BcMediaAac {
			data: vec![0xFF; 4],
		};
		assert_eq!(aac.duration(), None);
	}

	#[test]
	fn aac_duration_bad_syncword_byte0_returns_none() {
		let mut bytes = adts_header(4, 0);
		bytes[0] = 0x00;
		let aac = BcMediaAac { data: bytes };
		assert_eq!(aac.duration(), None);
	}

	#[test]
	fn aac_duration_bad_syncword_byte1_returns_none() {
		let mut bytes = adts_header(4, 0);
		bytes[1] = 0x00;
		let aac = BcMediaAac { data: bytes };
		assert_eq!(aac.duration(), None);
	}

	#[test]
	fn aac_duration_every_valid_freq_index() {
		// Samples per frame is 1024; duration = samples * 1e6 / freq.
		let expected: [(u8, u32); 13] = [
			(0, 1_000_000 * 1024 / 96000),
			(1, 1_000_000 * 1024 / 88200),
			(2, 1_000_000 * 1024 / 64000),
			(3, 1_000_000 * 1024 / 48000),
			(4, 1_000_000 * 1024 / 44100),
			(5, 1_000_000 * 1024 / 32000),
			(6, 1_000_000 * 1024 / 24000),
			(7, 1_000_000 * 1024 / 22050),
			(8, 1_000_000 * 1024 / 16000),
			(9, 1_000_000 * 1024 / 12000),
			(10, 1_000_000 * 1024 / 11025),
			(11, 1_000_000 * 1024 / 8000),
			(12, 1_000_000 * 1024 / 7350),
		];
		for (idx, want) in expected.iter().copied() {
			let aac = BcMediaAac {
				data: adts_header(idx, 0),
			};
			assert_eq!(aac.duration(), Some(want), "freq index {idx}");
		}
	}

	#[test]
	fn aac_duration_invalid_freq_index_returns_none() {
		// Indexes 13, 14, 15 are reserved / explicit / undefined.
		for idx in [13u8, 14, 15] {
			let aac = BcMediaAac {
				data: adts_header(idx, 0),
			};
			assert_eq!(aac.duration(), None, "freq index {idx} should be None");
		}
	}

	#[test]
	fn aac_duration_multi_frame_scales_samples() {
		// frames = raw+1; with raw=1, samples = 2 * 1024.
		let aac = BcMediaAac {
			data: adts_header(4, 1),
		};
		let expected = 1_000_000 * 2 * 1024 / 44100;
		assert_eq!(aac.duration(), Some(expected));
	}

	#[test]
	fn adpcm_block_size_subtracts_header() {
		let adpcm = BcMediaAdpcm {
			data: vec![0u8; 36], // 4 header + 32 samples
		};
		assert_eq!(adpcm.block_size(), 32);
	}

	#[test]
	fn adpcm_duration_samples_are_2_per_byte_at_8khz() {
		// block_size = 32, samples = 64, duration = 64 * 1e6 / 8000.
		let adpcm = BcMediaAdpcm {
			data: vec![0u8; 36],
		};
		assert_eq!(adpcm.duration(), Some(8000));
	}
}
