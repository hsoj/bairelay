//! SDP body generation per RFC 4566 for bairelay's codec combinations.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

use crate::rtsp::codec::{AudioCodec, VideoCodec};

/// Parameters needed to render the SDP body. Populated from the
/// `StreamSource` once codec detection completes.
#[derive(Clone)]
pub struct SdpParams {
	/// Server IP that will be advertised in the `o=` origin line.
	pub server_ip: String,
	/// Numeric session identifier for the `o=` line.
	pub session_id: String,
	/// Human-readable session name for the `s=` line.
	pub session_name: String,
	/// Optional video media description.
	pub video: Option<VideoParams>,
	/// Optional audio media description.
	pub audio: Option<AudioParams>,
}

/// Video media parameters used to build an `m=video` section.
#[derive(Clone, Debug)]
pub struct VideoParams {
	/// Codec identifier (H.264 or H.265).
	pub codec: VideoCodec,
	/// Dynamic RTP payload type (typically 96).
	pub payload_type: u8,
	/// Sequence Parameter Set (raw NAL payload without start code).
	pub sps: Vec<u8>,
	/// Picture Parameter Set (raw NAL payload without start code).
	pub pps: Vec<u8>,
	/// Video Parameter Set — H.265 only.
	pub vps: Option<Vec<u8>>,
	/// H.264 profile/level triplet rendered as a hex string in fmtp.
	pub profile_level_id: [u8; 3],
}

/// Audio media parameters used to build an `m=audio` section.
#[derive(Clone, Debug)]
pub struct AudioParams {
	/// Codec identifier (AAC or G.711 µ-law).
	pub codec: AudioCodec,
	/// RTP payload type (dynamic for AAC, static 0 for G.711 µ-law).
	pub payload_type: u8,
	/// Sample rate in Hz.
	pub sample_rate: u32,
	/// Channel count.
	pub channels: u8,
	/// AudioSpecificConfig as a hex string — AAC only.
	pub asc_hex: Option<String>,
}

/// Render the full SDP body for the given parameters.
///
/// Produces an RFC 4566-compatible description with session-level lines,
/// followed by optional video and audio media sections. Track IDs are
/// assigned sequentially starting at 0 in the order `video`, `audio`.
pub fn build(params: &SdpParams) -> String {
	let mut s = String::new();
	s.push_str("v=0\r\n");
	s.push_str(&format!(
		"o=- {} 1 IN IP4 {}\r\n",
		params.session_id, params.server_ip
	));
	s.push_str(&format!("s={}\r\n", params.session_name));
	s.push_str(&format!("c=IN IP4 {}\r\n", params.server_ip));
	s.push_str("t=0 0\r\n");
	s.push_str("a=control:*\r\n");
	s.push_str("a=range:npt=now-\r\n");

	let mut track_id = 0;

	if let Some(v) = &params.video {
		append_video(&mut s, v, track_id);
		track_id += 1;
	}
	if let Some(a) = &params.audio {
		append_audio(&mut s, a, track_id);
	}

	s
}

fn append_video(s: &mut String, v: &VideoParams, track_id: u32) {
	s.push_str(&format!("m=video 0 RTP/AVP {}\r\n", v.payload_type));
	match v.codec {
		VideoCodec::H264 => {
			s.push_str(&format!("a=rtpmap:{} H264/90000\r\n", v.payload_type));
			let sps_b64 = BASE64.encode(&v.sps);
			let pps_b64 = BASE64.encode(&v.pps);
			let profile = format!(
				"{:02x}{:02x}{:02x}",
				v.profile_level_id[0], v.profile_level_id[1], v.profile_level_id[2]
			);
			s.push_str(&format!(
				"a=fmtp:{} packetization-mode=1;profile-level-id={};sprop-parameter-sets={},{}\r\n",
				v.payload_type, profile, sps_b64, pps_b64
			));
		}
		VideoCodec::H265 => {
			s.push_str(&format!("a=rtpmap:{} H265/90000\r\n", v.payload_type));
			let sps_b64 = BASE64.encode(&v.sps);
			let pps_b64 = BASE64.encode(&v.pps);
			let vps_b64 = v.vps.as_ref().map(|b| BASE64.encode(b)).unwrap_or_default();
			s.push_str(&format!(
				"a=fmtp:{} sprop-vps={};sprop-sps={};sprop-pps={}\r\n",
				v.payload_type, vps_b64, sps_b64, pps_b64
			));
		}
	}
	s.push_str(&format!("a=control:trackID={}\r\n", track_id));
}

fn append_audio(s: &mut String, a: &AudioParams, track_id: u32) {
	s.push_str(&format!("m=audio 0 RTP/AVP {}\r\n", a.payload_type));
	match a.codec {
		AudioCodec::Aac => {
			s.push_str(&format!(
				"a=rtpmap:{} mpeg4-generic/{}/{}\r\n",
				a.payload_type, a.sample_rate, a.channels
			));
			let asc = a.asc_hex.as_deref().unwrap_or("");
			s.push_str(&format!(
				"a=fmtp:{} streamtype=5;profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3;config={}\r\n",
				a.payload_type, asc
			));
		}
		AudioCodec::G711Ulaw => {
			// Use static PT 0, rtpmap optional but provided for clarity.
			s.push_str(&format!("a=rtpmap:{} PCMU/8000\r\n", a.payload_type));
		}
	}
	s.push_str(&format!("a=control:trackID={}\r\n", track_id));
}

#[cfg(test)]
mod tests {
	use super::*;
	use sdp_types::Session;

	fn sample_video_h264() -> VideoParams {
		VideoParams {
			codec: VideoCodec::H264,
			payload_type: 96,
			sps: vec![0x67, 0x42, 0x00, 0x1f],
			pps: vec![0x68, 0xce, 0x38, 0x80],
			vps: None,
			profile_level_id: [0x42, 0x00, 0x1f],
		}
	}

	fn sample_audio_aac() -> AudioParams {
		AudioParams {
			codec: AudioCodec::Aac,
			payload_type: 97,
			sample_rate: 16000,
			channels: 1,
			asc_hex: Some("1408".into()),
		}
	}

	fn sample_audio_g711() -> AudioParams {
		AudioParams {
			codec: AudioCodec::G711Ulaw,
			payload_type: 0,
			sample_rate: 8000,
			channels: 1,
			asc_hex: None,
		}
	}

	#[test]
	fn video_only_sdp_parses() {
		let p = SdpParams {
			server_ip: "192.168.1.5".into(),
			session_id: "1".into(),
			session_name: "cam1".into(),
			video: Some(sample_video_h264()),
			audio: None,
		};
		let sdp = build(&p);
		let s = Session::parse(sdp.as_bytes()).expect("valid SDP");
		assert_eq!(s.medias.len(), 1);
		assert_eq!(s.medias[0].media, "video");
	}

	#[test]
	fn h264_sdp_includes_profile_level_id_and_sprop() {
		let p = SdpParams {
			server_ip: "127.0.0.1".into(),
			session_id: "1".into(),
			session_name: "cam1".into(),
			video: Some(sample_video_h264()),
			audio: None,
		};
		let sdp = build(&p);
		assert!(sdp.contains("profile-level-id=42001f"));
		assert!(sdp.contains("sprop-parameter-sets=Z0IAHw==,aM44gA=="));
	}

	#[test]
	fn video_and_aac_sdp_has_two_media_sections() {
		let p = SdpParams {
			server_ip: "127.0.0.1".into(),
			session_id: "1".into(),
			session_name: "cam1".into(),
			video: Some(sample_video_h264()),
			audio: Some(sample_audio_aac()),
		};
		let sdp = build(&p);
		let s = Session::parse(sdp.as_bytes()).expect("valid SDP");
		assert_eq!(s.medias.len(), 2);
		assert_eq!(s.medias[1].media, "audio");
	}

	#[test]
	fn g711_sdp_uses_static_payload_type_0() {
		let p = SdpParams {
			server_ip: "127.0.0.1".into(),
			session_id: "1".into(),
			session_name: "cam1".into(),
			video: Some(sample_video_h264()),
			audio: Some(sample_audio_g711()),
		};
		let sdp = build(&p);
		assert!(sdp.contains("m=audio 0 RTP/AVP 0\r\n"));
		assert!(sdp.contains("PCMU/8000"));
	}

	#[test]
	fn h265_sdp_includes_vps_sps_pps() {
		let p = SdpParams {
			server_ip: "127.0.0.1".into(),
			session_id: "1".into(),
			session_name: "cam1".into(),
			video: Some(VideoParams {
				codec: VideoCodec::H265,
				payload_type: 96,
				vps: Some(vec![0x40, 0x01]),
				sps: vec![0x42, 0x01],
				pps: vec![0x44, 0x01],
				profile_level_id: [0; 3],
			}),
			audio: None,
		};
		let sdp = build(&p);
		assert!(sdp.contains("H265/90000"));
		assert!(sdp.contains("sprop-vps="));
		assert!(sdp.contains("sprop-sps="));
		assert!(sdp.contains("sprop-pps="));
	}

	#[test]
	fn track_ids_are_sequential() {
		let p = SdpParams {
			server_ip: "127.0.0.1".into(),
			session_id: "1".into(),
			session_name: "cam1".into(),
			video: Some(sample_video_h264()),
			audio: Some(sample_audio_aac()),
		};
		let sdp = build(&p);
		assert!(sdp.contains("trackID=0"));
		assert!(sdp.contains("trackID=1"));
	}
}
