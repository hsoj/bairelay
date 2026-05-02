//! Video and audio codec support.

pub mod aac;
pub mod g711;
pub mod h264;
pub mod h265;
pub mod nal;

/// Video codec identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
	/// H.264 / AVC.
	H264,
	/// H.265 / HEVC.
	H265,
}

/// Audio codec identifier (post-transcode for ADPCM → G.711).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
	/// MPEG-4 AAC-LC.
	Aac,
	/// G.711 µ-law (PCMU), 8 kHz mono.
	G711Ulaw,
}
