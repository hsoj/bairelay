//! Sans-IO translation of decoded `BcMedia` packets into RTSP frames.
//!
//! [`translate`] is a pure function: it takes one packet, the mutable
//! [`StreamTranslatorState`], the current instant, and the bridging
//! gate, and returns the list of side effects ([`Emit`]) the caller
//! must perform plus the 90 kHz PTS of any broadcast video frame. The
//! driver (`stream_source::apply_bcmedia_packet`) owns every channel,
//! lock, and buffer and applies the emits in order.
//!
//! This is the same shape [`crate::gap_bridging`] uses: the product's
//! highest-defect-cost decisions — codec detection from the first
//! decisive NAL, parameter-set extraction, PTS synthesis and
//! wraparound, AAC AOT/sample-rate derivation, and the audio gate
//! during bridging — are testable here as tables, with no runtime, no
//! channels, and no timeouts.
//!
//! Reviewer rule: no `Sender`, `Arc`, or `RwLock` in any signature in
//! this module. If a change needs one, it belongs in the driver.

use std::time::{Duration, Instant};

use bytes::Bytes;
use smallvec::SmallVec;

use crate::baichuan::bcmedia::model::{
	BcMedia, BcMediaAac, BcMediaAdpcm, BcMediaIframe, BcMediaPframe,
};
use crate::rtsp::buffer::VideoBurst;
use crate::rtsp::codec::nal::{
	detect_codec, is_decodable_nal, split_annex_b, H264NalType, H265NalType,
};
use crate::rtsp::codec::{AudioCodec, VideoCodec};
use crate::rtsp::provider::Frame;
use crate::rtsp::sdp::{AudioParams, VideoParams};

/// Mutable state owned by the reader task's translator loop.
///
/// - `detected_codec` — H.264 vs H.265 verdict, latched on the first
///   identifying NAL.
/// - `aac_pts_next` — running 90 kHz-clock-independent AAC RTP timestamp
///   counter; advances by 1024 per AAC-LC AU (2048 for HE-AAC / HE-AACv2).
/// - `g711_pts_next` — running 8 kHz G.711 µ-law RTP timestamp counter;
///   advances by output-sample count per transcoded frame.
/// - `aac_aot` — last observed ADTS AudioObjectType. Gates the one-shot
///   "unsupported AOT" warn in the AAC translator so a latched-on-bad-AOT
///   stream doesn't log per packet.
///
/// Held by `StreamSource` in an `Arc<Mutex<_>>` so a mid-probe Baichuan
/// reconnect that re-spawns `reader_task` re-binds the same state — PTS
/// counters survive, so the next audio RTP packet after reconnect is not
/// a backward DTS jump (the 4K-Terrace tail-drain symptom from 2D.1
/// live-verify, see `docs/implementation.md`).
#[derive(Debug, Default)]
pub struct StreamTranslatorState {
	pub detected_codec: Option<VideoCodec>,
	pub aac_pts_next: u32,
	pub g711_pts_next: u32,
	pub aac_aot: Option<u8>,
	/// PTS (90 kHz) of the previous Video frame dispatched through the
	/// pacer. Used by `video_frame_duration` to compute the next pacer
	/// emission interval. `None` until the first video frame.
	pub last_video_pts_90khz: Option<u32>,
	/// Latched once [`Emit::SdpAudio`] has been produced, so the audio
	/// translators stop re-deriving SDP params on every packet. This
	/// replaces the old shared `sdp_params.audio.is_none()` read-check:
	/// translator state and SDP params share one lifetime (both survive
	/// a reader re-spawn via the same `Arc`s), so a state-local latch is
	/// equivalent in production — and the driver still guards its write
	/// on `audio.is_none()` so an externally primed SDP is never
	/// clobbered.
	pub sdp_audio_emitted: bool,
}

/// One side effect the driver must perform for a translated packet.
///
/// Emits are applied strictly in order — the sequencing carries
/// invariants (SDP populates before any frame reaches the wire; audio
/// presence upgrades only after an [`Emit::Audio`] in the same batch).
#[derive(Debug)]
pub enum Emit {
	/// Broadcast a video frame. `pace` is the wall-clock hold computed
	/// from the inter-frame PTS delta; the driver routes through the
	/// video pacer when one is wired in, else broadcasts directly.
	Video { frame: Frame, pace: Duration },
	/// Broadcast an audio frame. `pace` is the AU's natural playback
	/// duration at the codec's sample rate; routing mirrors
	/// [`Emit::Video`] via the audio pacer.
	Audio { frame: Frame, pace: Duration },
	/// Set `SdpParams.video`. Unconditional — refreshed on every
	/// I-frame that carries both SPS and PPS.
	SdpVideo(VideoParams),
	/// Populate `SdpParams.audio` if still unset. The driver must keep
	/// the write guarded on `audio.is_none()` so an SDP primed outside
	/// the translator (test constructors) is never clobbered.
	SdpAudio(AudioParams),
	/// Replace the last-frame buffer's video burst (keyframe arrived).
	ReplaceVideoBurst(VideoBurst),
	/// Append a P-frame's NAL units to the current burst.
	AppendPframe(Vec<Vec<u8>>),
	/// Upgrade audio presence to `Present { codec }`. Emitted only
	/// after an [`Emit::Audio`] in the same batch — presence reflects
	/// frames the translator actually produced.
	AudioSeen(AudioCodec),
}

/// Translate one decoded `BcMedia` packet into the side effects the
/// driver must perform.
///
/// Callers MUST reuse the same `&mut state` across every packet in a
/// given stream — H.264/H.265 detection and monotonic audio PTS both
/// depend on it. `now` stamps the capture time of any produced
/// [`VideoBurst`]; passing it in keeps this function clock-free (the
/// same discipline as [`crate::gap_bridging`]).
///
/// Returns `Some(pts_90khz)` iff the emits contain a video frame; the
/// value is that frame's 90 kHz RTP timestamp, used by the bridging
/// replay-frame synth to seed the next `Bridging` PTS. Audio packets
/// and info-variant drops always return `None` — they do not count as
/// "upstream video frame arrived" for gap detection. Callers must gate
/// `last_live_frame_at` / `gap_state` / `last_emitted_pts` updates on
/// `Some(_)` so an early return (empty NAL list, P-frame before any
/// I-frame, undetectable codec) does not spuriously mark the source as
/// `Live` when subscribers saw nothing.
///
/// `bridging` is the source's current upstream-presence gate — when
/// `true`, live audio frames are dropped silently (SDP population
/// still happens, so DESCRIBE stays accurate).
pub fn translate(
	packet: &BcMedia,
	state: &mut StreamTranslatorState,
	now: Instant,
	bridging: bool,
) -> (SmallVec<[Emit; 4]>, Option<u32>) {
	match packet {
		BcMedia::Iframe(iframe) => translate_iframe(iframe, state, now),
		BcMedia::Pframe(pframe) => translate_pframe(pframe, state),
		BcMedia::Aac(aac) => (translate_aac(aac, state, bridging), None),
		BcMedia::Adpcm(adpcm) => (translate_adpcm(adpcm, state, bridging), None),
		BcMedia::InfoV1(_) | BcMedia::InfoV2(_) => (SmallVec::new(), None),
	}
}

/// Returns `Some(pts_90khz)` iff the emits contain a video keyframe.
/// The early-return paths (empty NAL list, undetectable codec, no
/// slice NALs after the parameter-set strip) return `None` so the
/// caller's gap marker does not flip to `Live` when subscribers saw
/// nothing — note the last of those still carries SDP/burst emits.
fn translate_iframe(
	iframe: &BcMediaIframe,
	state: &mut StreamTranslatorState,
	now: Instant,
) -> (SmallVec<[Emit; 4]>, Option<u32>) {
	let mut emits = SmallVec::new();
	let nals = split_annex_b(&iframe.data);
	if nals.is_empty() {
		return (emits, None);
	}

	// Detect the codec from the first NAL that gives a verdict.
	if state.detected_codec.is_none() {
		for nal in &nals {
			if let Some(c) = detect_codec(nal) {
				state.detected_codec = Some(c);
				break;
			}
		}
	}
	let codec = match state.detected_codec {
		Some(c) => c,
		None => {
			tracing::warn!("I-frame with no detectable codec; dropping");
			return (emits, None);
		}
	};

	// Filter NALs to the standard single-layer whitelist. Reolink Argus
	// firmware emits HEVC NAL type 62 (UNSPEC62) inside access units;
	// ffmpeg's RTP-HEVC depacketizer rejects them with `Unsupported
	// (HEVC) NAL type (62)` and the resulting decode disruption surfaces
	// as `Could not find ref with POC N` / `Skipping invalid undecodable
	// NALU` and visible spinning in mpv / HA. Multi-layer NALs (any
	// `nuh_layer_id != 0`) trigger ffmpeg's `Multi-layer HEVC coding is
	// not implemented` for the same reason. The official Reolink app's
	// proprietary decoder ignores both classes; standard decoders need
	// us to strip them. See `is_decodable_nal` for the whitelist.
	let nals: Vec<&[u8]> = nals
		.into_iter()
		.filter(|n| is_decodable_nal(n, codec))
		.collect();
	if nals.is_empty() {
		return (emits, None);
	}

	// Reorder NALs so non-slice NALs (parameter sets, SEI, AUD, prefix
	// data) precede slice NALs. The RTP packetizer sets the marker bit on
	// the last NAL of the access unit; if a camera emits SEI/AUD after
	// the slice the marker would land on them instead of the slice,
	// breaking the access-unit boundary signal for strict decoders.
	let (mut non_slice, mut slice): (Vec<&[u8]>, Vec<&[u8]>) =
		nals.iter().partition(|n| !is_slice_nal(n, codec));
	non_slice.append(&mut slice);
	let reordered: Vec<&[u8]> = non_slice;

	// Extract parameter sets + IDR NALs per codec.
	let (parameter_sets, iframe_nals, sps_bytes, pps_bytes, vps_bytes) =
		extract_iframe_parts(codec, &reordered);

	// Update SDP params. Only do this if we have both SPS and PPS;
	// otherwise wait for a future I-frame.
	if let (Some(sps), Some(pps)) = (sps_bytes.as_ref(), pps_bytes.as_ref()) {
		let profile_level_id = if sps.len() >= 4 {
			[sps[1], sps[2], sps[3]]
		} else {
			[0u8; 3]
		};
		emits.push(Emit::SdpVideo(VideoParams {
			codec,
			payload_type: 96,
			sps: sps.clone(),
			pps: pps.clone(),
			vps: vps_bytes.clone(),
			profile_level_id,
		}));
	}

	// Refresh the last-frame buffer with a fresh burst. We store the
	// already reordered iframe_nals so that burst replay preserves the
	// same non-slice-then-slice ordering that marker-bit placement
	// depends on. captured_pts_90khz lets the session send loop replay
	// with a timestamp continuous with the live stream — see buffer.rs.
	let burst_pts = micros_to_90khz(iframe.microseconds);
	emits.push(Emit::ReplaceVideoBurst(VideoBurst {
		codec,
		parameter_sets,
		iframe_nals,
		pframe_nals: Vec::new(),
		captured_at: now,
		captured_pts_90khz: burst_pts,
	}));

	// Build the outbound Frame::Video carrying only non-parameter-set
	// NALs (the iframe slice[s], plus any SEI/AUD). The SDP
	// `sprop-vps/sps/pps` fmtp attribute carries the parameter sets
	// out-of-band — clients (VLC, ffmpeg, mpv, gstreamer, HA's stream:
	// component) all consume those during DESCRIBE and initialize their
	// decoders from them. Sending VPS/SPS/PPS in-band additionally makes
	// a downstream `-c copy -f rtsp` re-packer (HA's go2rtc `ffmpeg:`
	// wrap) combine the three small NALs at the same RTP timestamp into
	// an HEVC RFC 7798 §4.4.2 AP (Aggregation Packet, NAL type 48).
	// go2rtc's own RTPDepay does not de-aggregate AP; the raw AP header
	// bytes then reach its `/api/frame.jpeg` transcoder and ffmpeg exits
	// with status 183 (invalid input data). Stripping the in-band copies
	// leaves only the IDR slice on the wire; ffmpeg has nothing to
	// aggregate and the go2rtc pipeline succeeds.
	let nals_bytes: Vec<Bytes> = reordered
		.iter()
		.filter(|n| !is_parameter_set_nal(n, codec))
		.map(|n| Bytes::copy_from_slice(n))
		.collect();
	if nals_bytes.is_empty() {
		// Access unit was made entirely of parameter sets (VPS/SPS/PPS,
		// no slice). Stripping the parameter sets leaves nothing for
		// downstream packetization; emitting a zero-NAL `Frame::Video`
		// would yield a marker-bit-only RTP packet that strict
		// receivers reject. SDP `sprop-*` fmtp attributes already
		// carry the parameter sets out-of-band — no information is
		// lost by dropping this access unit. The SDP/burst emits above
		// still apply.
		tracing::debug!(
			"I-frame access unit had no slice NALs after parameter-set strip; dropping"
		);
		return (emits, None);
	}

	let pts_90khz = micros_to_90khz(iframe.microseconds);
	let frame = Frame::Video {
		codec,
		nals: nals_bytes,
		pts_90khz,
		keyframe: true,
		access_unit_end: true,
	};
	// The pace duration holds each frame in the driver's pacer until
	// its natural inter-PTS wallclock interval elapses since the
	// previous emit, so the receiver sees a steady frame rate even when
	// the camera bursts (Argus 4 K HEVC delivers a GOP in ~900 ms then
	// idles ~1.1 s). Without pacing, mpv reports `(Buffering)` whenever
	// the camera pauses transmission.
	let pace = video_frame_duration(state, pts_90khz);
	emits.push(Emit::Video { frame, pace });
	(emits, Some(pts_90khz))
}

/// Returns `Some(pts_90khz)` iff the emits contain a video P-frame.
/// Returns `None` when the P-frame arrives before any I-frame has been
/// seen (codec undetected) or after NAL splitting/filtering produces an
/// empty list. The gap marker must not flip to `Live` in those cases —
/// subscribers saw nothing.
fn translate_pframe(
	pframe: &BcMediaPframe,
	state: &mut StreamTranslatorState,
) -> (SmallVec<[Emit; 4]>, Option<u32>) {
	let mut emits = SmallVec::new();
	let codec = match state.detected_codec {
		Some(c) => c,
		None => {
			// Haven't seen an I-frame yet — drop this P-frame. Clients
			// can't decode without the preceding keyframe anyway.
			return (emits, None);
		}
	};
	let nals = split_annex_b(&pframe.data);
	if nals.is_empty() {
		return (emits, None);
	}

	// Same NAL whitelist as the I-frame path — Reolink Argus emits
	// proprietary HEVC NAL type 62 / multi-layer NALs inside P-frame
	// access units too, and ffmpeg's RTP-HEVC depacketizer rejects
	// them. See `is_decodable_nal` for the rationale.
	let nals: Vec<&[u8]> = nals
		.into_iter()
		.filter(|n| is_decodable_nal(n, codec))
		.collect();
	if nals.is_empty() {
		return (emits, None);
	}

	// Reorder: non-slice NALs first, slice NALs last — same reasoning as
	// the I-frame path (marker bit must land on the trailing slice
	// packet).
	let (mut non_slice, mut slice): (Vec<&[u8]>, Vec<&[u8]>) =
		nals.iter().partition(|n| !is_slice_nal(n, codec));
	non_slice.append(&mut slice);
	let reordered: Vec<&[u8]> = non_slice;

	// Append to the last-frame buffer so reconnecting clients can replay
	// the recent burst (I-frame + trailing P-frames) while waiting for
	// the next keyframe. Store the reordered sequence so burst replay
	// keeps the marker-bit placement guarantee.
	let nals_owned: Vec<Vec<u8>> = reordered.iter().map(|n| (*n).to_vec()).collect();
	emits.push(Emit::AppendPframe(nals_owned));

	let nals_bytes: Vec<Bytes> = reordered
		.iter()
		.map(|n| Bytes::copy_from_slice(n))
		.collect();
	let pts_90khz = micros_to_90khz(pframe.microseconds);
	let frame = Frame::Video {
		codec,
		nals: nals_bytes,
		pts_90khz,
		keyframe: false,
		access_unit_end: true,
	};
	// See the I-frame path for why video frames carry a pace duration.
	let pace = video_frame_duration(state, pts_90khz);
	emits.push(Emit::Video { frame, pace });
	(emits, Some(pts_90khz))
}

/// Translate a `BcMedia::Aac` packet into a `Frame::Audio { Aac { .. } }`
/// emit, plus [`Emit::SdpAudio`] on first observation.
///
/// The packet carries ADTS-framed AAC audio (sync 0xFFF, profile,
/// sr_idx, channels, frame_length, body). We parse the ADTS header
/// via `crate::rtsp::codec::aac::parse_adts` and strip it before
/// emitting — the RTP packetizer wraps raw AU data in the RFC 3640
/// AU-hbr payload itself. SDP emission is one-shot via
/// `state.sdp_audio_emitted`.
///
/// Emits [`Emit::AudioSeen`] after the audio frame so presence upgrades
/// from `Unknown`/`Absent` to `Present { codec: Aac }` only when a
/// frame was actually produced.
///
/// Silently drops the frame when `bridging` is set; see body for
/// invariant details (SDP populates first, presence untouched, PTS
/// counter advances so Live resume continues cleanly).
fn translate_aac(
	aac: &BcMediaAac,
	state: &mut StreamTranslatorState,
	bridging: bool,
) -> SmallVec<[Emit; 4]> {
	use crate::rtsp::codec::aac::{
		build_audio_specific_config_hex, parse_adts, AAC_PAYLOAD_TYPE, ADTS_HEADER_LEN,
	};
	use crate::rtsp::provider::AudioPayload;

	let mut emits = SmallVec::new();

	let Some(header) = parse_adts(&aac.data) else {
		tracing::debug!("ADTS header parse failed; dropping AAC packet");
		return emits;
	};

	// `channels == 0` means the channel configuration is carried in
	// the program config element inside the AAC body (MPEG-4 §1.6.1.1).
	// Bairelay's SDP / packetizer pipeline can't parse the PCE, so the
	// downstream RTP players would render "0 channels" — silence. Drop
	// rather than emit a no-audio Frame::Audio that confuses receivers.
	// One-shot warn keyed on `state.aac_aot` to match the unsupported-
	// AOT branch's chatter discipline.
	if header.channels == 0 {
		if state.aac_aot != Some(header.aot) {
			tracing::warn!(
				aot = header.aot,
				"translate_aac: PCE-specified channel config (channels=0); dropping AAC packet"
			);
			state.aac_aot = Some(header.aot);
		}
		return emits;
	}

	// Emit SDP audio on first observation. The latch stays unset on an
	// ASC-build failure so the next packet retries (and re-warns) —
	// matching the old behaviour where SDP audio never populated and
	// the read-check kept failing.
	if !state.sdp_audio_emitted {
		if let Some(asc) =
			build_audio_specific_config_hex(header.aot, header.sample_rate, header.channels)
		{
			emits.push(Emit::SdpAudio(AudioParams {
				codec: AudioCodec::Aac,
				payload_type: AAC_PAYLOAD_TYPE,
				sample_rate: header.sample_rate,
				channels: header.channels,
				asc_hex: Some(asc),
			}));
			state.sdp_audio_emitted = true;
		} else {
			tracing::warn!(
				sample_rate = header.sample_rate,
				channels = header.channels,
				"AAC sample_rate/channels unsupported for AudioSpecificConfig"
			);
		}
	}

	// Strip ADTS header; body is what the AU-hbr packetizer expects.
	// parse_adts accepts any frame_length ≥ some minimum, so defend
	// against a malformed frame_length that's still < header length.
	if aac.data.len() < ADTS_HEADER_LEN || header.frame_length < ADTS_HEADER_LEN {
		tracing::debug!(
			frame_length = header.frame_length,
			data_len = aac.data.len(),
			"AAC frame_length/data too small for ADTS header; dropping"
		);
		return emits;
	}
	let payload = &aac.data[ADTS_HEADER_LEN..];
	// Clamp to the ADTS header's declared frame_length — trailing
	// bytes beyond it can appear on some firmwares.
	let au_bytes_len = header
		.frame_length
		.saturating_sub(ADTS_HEADER_LEN)
		.min(payload.len());
	if au_bytes_len == 0 {
		// Empty AAC body (truncated packet, or frame_length exactly
		// equal to ADTS_HEADER_LEN). Dropping is preferable to emitting
		// a zero-length AU that would become a malformed RTP packet
		// downstream (build_au_hbr_payload on an empty slice would
		// produce a header with size=0 and no body).
		//
		// We also do NOT emit AudioSeen here: a subscriber waiting for
		// Frame::Audio on the broadcast would observe nothing, so
		// "Present" would lie. Treat this as if we hadn't seen a usable
		// AAC packet yet. SDP audio may already ride in the emits above
		// — that's fine, DESCRIBE advertising audio before any audio
		// reaches the broadcast is already the pre-SETUP reality.
		tracing::debug!("AAC packet with empty body; dropping");
		return emits;
	}
	let au_data = Bytes::copy_from_slice(&payload[..au_bytes_len]);

	// Monotonic RTP timestamp. AAC-LC carries 1024 samples per access
	// unit (RFC 3640 / ISO 14496-3); HE-AAC / HE-AACv2 carry 2048. The
	// RTP clock rate equals the audio sample rate, so each emitted AU
	// advances the counter by the per-AU sample count. The packetizer
	// forwards this `pts` verbatim into the RTP header (see
	// src/rtsp/server/packetizer.rs dispatch_audio). Zero-PTS
	// audio caused ffmpeg/mpv/gst-launch to reject streams with
	// "DTS N >= N" on the 4K HEVC camera; monotonic increments fix the
	// root cause. Wrap with `wrapping_add` — RTP timestamps intentionally
	// wrap at 2^32.
	//
	// Unsupported AOTs (1/3/4/...) have no confirmed per-AU sample count,
	// so we drop the frame rather than guess a step and drift. Warn
	// once per new AOT via `state.aac_aot` so a latched-on-bad-AOT
	// stream doesn't log per packet.
	let samples_per_au = match aac_samples_per_au(header.aot) {
		Some(n) => n,
		None => {
			if state.aac_aot != Some(header.aot) {
				tracing::warn!(
					aot = header.aot,
					"translate_aac: unsupported AudioObjectType; dropping AAC packet"
				);
				state.aac_aot = Some(header.aot);
			}
			return emits;
		}
	};
	if state.aac_aot != Some(header.aot) {
		// One-shot per-AOT trace so operators can see the cadence
		// parameters bairelay is using for this stream when debugging.
		// Kept at debug level — every camera connect logs once per
		// stream, which is too chatty for INFO.
		tracing::debug!(
			aot = header.aot,
			sample_rate = header.sample_rate,
			channels = header.channels,
			samples_per_au,
			aac_frames = header.aac_frames,
			"AAC stream parameters"
		);
		state.aac_aot = Some(header.aot);
	}
	// ADTS may pack 1..=4 AAC frames per packet (RFC 7798 / ISO 13818-7
	// §6.2 `number_of_raw_data_blocks_in_frame`). The RTP timestamp must
	// advance by every contained frame, not just one. Argus firmwares
	// observed in the field have packed audio across packets, so this
	// matters: a fixed-1024 step against a multi-frame packet leaves the
	// PTS-vs-NTP slope below clock_rate and surfaces as `Invalid audio
	// PTS` jumps in mpv every few seconds.
	let pts_step = samples_per_au.saturating_mul(header.aac_frames as u32);

	// Advance the PTS counter BEFORE the Bridging gate. The camera's
	// audio cadence is the only wallclock proxy we have during a gap.
	let pts = state.aac_pts_next;
	state.aac_pts_next = state.aac_pts_next.wrapping_add(pts_step);

	// Drop live audio while `Bridging`. Video is frozen (replay frames
	// only), so forwarding audio would produce nonsensical A/V
	// correlation downstream. Keep the drop silent — it fires on every
	// audio packet during a gap, so a log line would spam. SDP and
	// presence are untouched: the SDP emit already rides above
	// (DESCRIBE stays accurate), and presence should reflect frames
	// that actually reached subscribers.
	if bridging {
		return emits;
	}

	let frame = Frame::Audio {
		payload: AudioPayload::Aac {
			au_data,
			sample_rate: header.sample_rate,
			channels: header.channels,
		},
		pts,
	};

	// The pace duration is the codec-natural slot (`pts_step /
	// sample_rate`); the driver's pacer holds each frame that long,
	// capping accumulated lead time.
	let pace = paced_audio_duration(pts_step, header.sample_rate);
	emits.push(Emit::Audio { frame, pace });

	// Presence upgrades regardless of how the driver's dispatch fares.
	// A broadcast SendError just means no subscribers (or pacer back-
	// pressure); the frame was still "emitted" from the translator's
	// perspective and presence reflects what we produced, not what
	// anyone read. The empty-body drop above is the one case where we
	// skip the upgrade.
	emits.push(Emit::AudioSeen(AudioCodec::Aac));
	emits
}

/// Translate a `BcMedia::Adpcm` packet into a `Frame::Audio { G711Ulaw }`
/// emit by decoding ADPCM → PCM16 (16 kHz) → PCM16 (8 kHz) → µ-law.
///
/// Emits [`Emit::SdpAudio`] with G.711 µ-law params (static RTP PT 0
/// per RFC 3551, 8 kHz mono) on first observation, one-shot via
/// `state.sdp_audio_emitted` — the same latch the AAC translator uses.
///
/// Emits [`Emit::AudioSeen`] only after a frame emit — dropped packets
/// (decode failures, empty blocks) leave presence untouched.
///
/// Reolink ADPCM packets carry the full predictor+step header at the
/// start of every block, so a per-packet decoder with fresh state is
/// correct — no cross-packet continuation is needed.
///
/// Silently drops the frame when `bridging` is set; same invariants as
/// the AAC translator (SDP emits first, presence untouched, PTS
/// counter advances so Live resume continues cleanly).
fn translate_adpcm(
	adpcm: &BcMediaAdpcm,
	state: &mut StreamTranslatorState,
	bridging: bool,
) -> SmallVec<[Emit; 4]> {
	use crate::rtsp::codec::g711::{encode as g711_encode, G711_PAYLOAD_TYPE};
	use crate::rtsp::provider::AudioPayload;
	use crate::rtsp::transcode::{adpcm::AdpcmDecoder, resample::decimate_16_to_8};

	let mut emits = SmallVec::new();

	let mut dec = AdpcmDecoder::new();
	let pcm_16k = match dec.decode_block(&adpcm.data) {
		Ok(p) => p,
		Err(e) => {
			tracing::debug!(error = ?e, "ADPCM decode failed; dropping packet");
			return emits;
		}
	};

	if pcm_16k.is_empty() {
		tracing::debug!("ADPCM block decoded to zero samples; dropping");
		return emits;
	}

	let pcm_8k = decimate_16_to_8(&pcm_16k);
	if pcm_8k.is_empty() {
		tracing::debug!("ADPCM block too short after decimation; dropping");
		return emits;
	}

	let ulaw = Bytes::from(g711_encode(&pcm_8k));

	// Emit SDP audio on first observation (same latch as the AAC path).
	if !state.sdp_audio_emitted {
		emits.push(Emit::SdpAudio(AudioParams {
			codec: AudioCodec::G711Ulaw,
			payload_type: G711_PAYLOAD_TYPE,
			sample_rate: 8_000,
			channels: 1,
			asc_hex: None,
		}));
		state.sdp_audio_emitted = true;
	}

	// Advance the PTS counter BEFORE the Bridging gate — same rationale
	// as the AAC path: the transcoded output sample count is the
	// wallclock proxy we use to keep A/V aligned on Live resume. G.711
	// (µ-law, RFC 3551 PT 0) uses a static 8 kHz clock with one RTP
	// tick per output sample, so `ulaw.len()` is the natural step.
	let sample_count = ulaw.len() as u32;
	let pts = state.g711_pts_next;
	state.g711_pts_next = state.g711_pts_next.wrapping_add(sample_count);

	// Drop live audio while `Bridging`. See the AAC translator for the
	// full reasoning — same invariants apply (silent drop, SDP emit
	// already rides above, presence untouched).
	if bridging {
		return emits;
	}

	let frame = Frame::Audio {
		payload: AudioPayload::G711Ulaw { samples: ulaw },
		pts,
	};
	// G.711 µ-law is 1 byte per sample at 8 kHz, so the produced ulaw
	// length is also the sample count for pacing purposes.
	let pace = paced_audio_duration(sample_count, 8_000);
	emits.push(Emit::Audio { frame, pace });

	emits.push(Emit::AudioSeen(AudioCodec::G711Ulaw));
	emits
}

/// Converts microseconds (from `BcMedia` packets) to a 90 kHz RTP clock
/// via `µs * 9 / 100`. Wrapping arithmetic is the desired RTP behaviour.
fn micros_to_90khz(micros: u32) -> u32 {
	((micros as u64).wrapping_mul(9) / 100) as u32
}

/// Returns true if `nal` is a codec parameter-set NAL (SPS/PPS for
/// H.264, VPS/SPS/PPS for H.265). Used to strip parameter sets from
/// the outbound live broadcast — SDP's `sprop-*` fmtp attributes
/// already carry these out-of-band, and leaving them in-band lets
/// downstream re-muxers (notably HA's go2rtc `ffmpeg:` wrap)
/// aggregate them into an HEVC AP that go2rtc can't de-aggregate.
/// See the outbound-NAL filter in `translate_iframe` for the full
/// trace. Also used by the bridging replay to filter cached bursts.
pub(crate) fn is_parameter_set_nal(nal: &[u8], codec: VideoCodec) -> bool {
	if nal.is_empty() {
		return false;
	}
	match codec {
		VideoCodec::H264 => {
			let ty = H264NalType::from_header_byte(nal[0]);
			matches!(ty, H264NalType::SPS | H264NalType::PPS)
		}
		VideoCodec::H265 => {
			let ty = H265NalType::from_header_byte(nal[0]);
			matches!(ty, H265NalType::VPS | H265NalType::SPS | H265NalType::PPS)
		}
	}
}

/// Returns true if `nal` is a video-coded slice NAL for the given codec.
///
/// Used by the packetizer-feeder so non-slice NALs (SPS/PPS/VPS/SEI/AUD/...)
/// can be moved ahead of slice NALs, letting the marker bit land on the
/// trailing slice packet.
fn is_slice_nal(nal: &[u8], codec: VideoCodec) -> bool {
	if nal.is_empty() {
		return false;
	}
	match codec {
		VideoCodec::H264 => {
			let ty = H264NalType::from_header_byte(nal[0]);
			// Non-IDR slice (1), IDR slice (5). Also types 2..=4 are
			// data-partitioned slices (A/B/C); treat them as slice NALs
			// for completeness, although Reolink doesn't emit them.
			matches!(ty, 1..=5)
		}
		VideoCodec::H265 => {
			let ty = H265NalType::from_header_byte(nal[0]);
			// HEVC VCL NALs: 0..=9 (trailing/TSA/STSA/RADL/RASL),
			// 16..=21 (BLA/IDR/CRA). Non-VCL starts at 32.
			matches!(ty, 0..=9 | 16..=21)
		}
	}
}

/// Extract parameter sets (SPS/PPS for H.264, VPS/SPS/PPS for H.265) and
/// IDR NALs from a split I-frame NAL sequence, for both
/// [`crate::rtsp::buffer::LastFrameBuffer`] insertion and SDP generation.
#[expect(
	clippy::type_complexity,
	reason = "one-caller tuple return; naming a struct for it would outweigh the tuple"
)]
fn extract_iframe_parts(
	codec: VideoCodec,
	nals: &[&[u8]],
) -> (
	Vec<Vec<u8>>,    // parameter_sets
	Vec<Vec<u8>>,    // iframe_nals
	Option<Vec<u8>>, // sps
	Option<Vec<u8>>, // pps
	Option<Vec<u8>>, // vps (H.265 only)
) {
	let mut parameter_sets: Vec<Vec<u8>> = Vec::new();
	let mut iframe_nals: Vec<Vec<u8>> = Vec::new();
	let mut sps: Option<Vec<u8>> = None;
	let mut pps: Option<Vec<u8>> = None;
	let mut vps: Option<Vec<u8>> = None;

	for nal in nals {
		if nal.is_empty() {
			continue;
		}
		let owned: Vec<u8> = (*nal).to_vec();
		match codec {
			VideoCodec::H264 => {
				let ty = H264NalType::from_header_byte(owned[0]);
				match ty {
					H264NalType::SPS => {
						sps = Some(owned.clone());
						parameter_sets.push(owned);
					}
					H264NalType::PPS => {
						pps = Some(owned.clone());
						parameter_sets.push(owned);
					}
					H264NalType::IDR_SLICE => {
						iframe_nals.push(owned);
					}
					_ => {
						// SEI/AUD/etc — skip for burst contents.
					}
				}
			}
			VideoCodec::H265 => {
				if owned.is_empty() {
					continue;
				}
				let ty = H265NalType::from_header_byte(owned[0]);
				match ty {
					H265NalType::VPS => {
						vps = Some(owned.clone());
						parameter_sets.push(owned);
					}
					H265NalType::SPS => {
						sps = Some(owned.clone());
						parameter_sets.push(owned);
					}
					H265NalType::PPS => {
						pps = Some(owned.clone());
						parameter_sets.push(owned);
					}
					H265NalType::IDR_W_RADL
					| H265NalType::IDR_N_LP
					| H265NalType::CRA
					| H265NalType::BLA_W_LP => {
						iframe_nals.push(owned);
					}
					_ => {}
				}
			}
		}
	}

	(parameter_sets, iframe_nals, sps, pps, vps)
}

/// Sample count per AAC access unit, keyed on ADTS AudioObjectType.
///
/// AAC-LC (AOT=2) is 1024 samples/AU (RFC 3640 / ISO 14496-3). HE-AAC
/// (AOT=5) and HE-AACv2 (AOT=29) carry 2048 samples/AU because of the
/// SBR doubling. Any other AOT is unsupported by this translator —
/// callers MUST drop the frame rather than guess a step, otherwise the
/// AAC RTP timestamp counter drifts and downstream muxers reject the
/// stream with "DTS N >= N" style errors.
///
/// Pure helper so the branch is unit-testable without ADTS synthesis
/// (ADTS only encodes the lower 2 bits of `aot - 1`, i.e. AOT values
/// 1..=4, so AOT=5/29 can't be reached via `parse_adts` in production).
fn aac_samples_per_au(aot: u8) -> Option<u32> {
	match aot {
		2 => Some(1024),
		5 | 29 => Some(2048),
		_ => None,
	}
}

/// Compute the wall-clock duration the video pacer should hold this
/// frame for, based on the gap to the previously emitted video PTS.
/// First frame: 0 (emit immediately). Otherwise: `(pts - last_video_pts)
/// / 90000` seconds. PTS is at 90 kHz; wrap-safe via `wrapping_sub`.
///
/// Anomaly cap: Argus default GOP is ≤2 s; a delta beyond ~5 s of video
/// time signals one of (a) the camera's PTS clock reset, (b) we missed
/// an entire GOP and `wrapping_sub` produced a near-`u32::MAX` value
/// because the previous PTS was numerically larger, or (c) the source
/// itself paused upstream (the gap-bridging path handles this; the
/// pacer should not contribute additional delay). In all three cases
/// "emit immediately" (duration 0) is correct — we don't want the
/// pacer to stall for hours on a single anomalous frame, and we don't
/// want to accept a near-full-u32 wait as a legitimate inter-frame
/// interval.
const PACER_ANOMALY_CAP_TICKS: u32 = 90_000 * 5;
fn video_frame_duration(state: &mut StreamTranslatorState, pts_90khz: u32) -> Duration {
	let dur = match state.last_video_pts_90khz {
		Some(prev) => {
			let delta = pts_90khz.wrapping_sub(prev);
			let ticks = if delta > PACER_ANOMALY_CAP_TICKS {
				0
			} else {
				delta
			};
			Duration::from_micros((ticks as u64 * 1_000_000) / 90_000)
		}
		None => Duration::ZERO,
	};
	state.last_video_pts_90khz = Some(pts_90khz);
	dur
}

/// Convert a per-AU sample count + sample rate to the corresponding
/// wall-clock duration. Used to schedule the driver-side pacer's next
/// emission slot.
fn paced_audio_duration(samples: u32, sample_rate: u32) -> Duration {
	if sample_rate == 0 {
		return Duration::ZERO;
	}
	let micros = (samples as u64).saturating_mul(1_000_000) / sample_rate as u64;
	Duration::from_micros(micros)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::baichuan::bcmedia::model::VideoType;

	fn state_with_codec(codec: VideoCodec) -> StreamTranslatorState {
		StreamTranslatorState {
			detected_codec: Some(codec),
			..Default::default()
		}
	}

	/// Valid AOT=2 (AAC-LC) mono ADTS fixture: 7-byte header + 9-byte body.
	fn aac_lc_packet() -> BcMediaAac {
		let mut data = vec![0xFF, 0xF9, 0x60, 0x40, 0x02, 0x00, 0xFC];
		data.extend_from_slice(&[0xAA; 9]);
		BcMediaAac { data }
	}

	/// Silent ADPCM block: 4-byte predictor/step header + 16 nibble bytes.
	fn silent_adpcm_packet() -> BcMediaAdpcm {
		BcMediaAdpcm {
			data: vec![0u8; 4 + 16],
		}
	}

	// ── translate: emit sequences ─────────────────────────────────────

	/// An H.265 keyframe with VPS+SPS+PPS+IDR produces the full ordered
	/// batch: SDP video first, then the burst replacement, then the
	/// wire frame — and the returned PTS matches the frame's.
	#[test]
	fn iframe_emits_sdp_then_burst_then_frame_in_order() {
		let iframe = BcMediaIframe {
			video_type: VideoType::H265,
			microseconds: 1_000_000,
			data: vec![
				0x00, 0x00, 0x01, 0x40, 0x01, 0x0C, // VPS
				0x00, 0x00, 0x01, 0x42, 0x01, 0x02, // SPS
				0x00, 0x00, 0x01, 0x44, 0x01, 0xC0, // PPS
				0x00, 0x00, 0x01, 0x26, 0x01, 0xAA, // IDR_W_RADL
			],
			time: None,
		};
		let mut state = StreamTranslatorState::default();
		let (emits, pts) = translate(&BcMedia::Iframe(iframe), &mut state, Instant::now(), false);
		assert_eq!(pts, Some(90_000));
		assert_eq!(emits.len(), 3);
		assert!(matches!(&emits[0], Emit::SdpVideo(v) if v.codec == VideoCodec::H265));
		assert!(matches!(&emits[1], Emit::ReplaceVideoBurst(b) if b.captured_pts_90khz == 90_000));
		assert!(matches!(
			&emits[2],
			Emit::Video {
				frame: Frame::Video { keyframe: true, .. },
				..
			}
		));
		assert_eq!(state.detected_codec, Some(VideoCodec::H265));
	}

	/// An access unit that is nothing but parameter sets still updates
	/// SDP and the burst, but must not produce a zero-NAL wire frame —
	/// and must not report a video PTS to the gap detector.
	#[test]
	fn iframe_with_only_parameter_sets_emits_sdp_and_burst_but_no_frame() {
		let iframe = BcMediaIframe {
			video_type: VideoType::H265,
			microseconds: 0,
			data: vec![
				0x00, 0x00, 0x01, 0x40, 0x01, 0x0C, // VPS
				0x00, 0x00, 0x01, 0x42, 0x01, 0x02, // SPS
				0x00, 0x00, 0x01, 0x44, 0x01, 0xC0, // PPS
			],
			time: None,
		};
		let mut state = StreamTranslatorState::default();
		let (emits, pts) = translate(&BcMedia::Iframe(iframe), &mut state, Instant::now(), false);
		assert_eq!(pts, None);
		assert_eq!(emits.len(), 2);
		assert!(matches!(&emits[0], Emit::SdpVideo(_)));
		assert!(matches!(&emits[1], Emit::ReplaceVideoBurst(_)));
	}

	#[test]
	fn info_variants_translate_to_nothing() {
		use crate::baichuan::bcmedia::model::BcMediaInfoV1;
		let info = BcMedia::InfoV1(BcMediaInfoV1 {
			video_width: 0,
			video_height: 0,
			fps: 0,
			start_year: 0,
			start_month: 0,
			start_day: 0,
			start_hour: 0,
			start_min: 0,
			start_seconds: 0,
			end_year: 0,
			end_month: 0,
			end_day: 0,
			end_hour: 0,
			end_min: 0,
			end_seconds: 0,
		});
		let mut state = StreamTranslatorState::default();
		let (emits, pts) = translate(&info, &mut state, Instant::now(), false);
		assert!(emits.is_empty());
		assert_eq!(pts, None);
	}

	/// First live AAC packet: SDP audio, then the frame, then the
	/// presence upgrade — in that order. Second packet: no SDP re-emit.
	#[test]
	fn aac_first_packet_emits_sdp_frame_presence_then_latches_sdp() {
		let mut state = StreamTranslatorState::default();
		let emits = translate_aac(&aac_lc_packet(), &mut state, false);
		assert_eq!(emits.len(), 3);
		assert!(matches!(&emits[0], Emit::SdpAudio(a) if a.codec == AudioCodec::Aac));
		assert!(matches!(
			&emits[1],
			Emit::Audio {
				frame: Frame::Audio { pts: 0, .. },
				..
			}
		));
		assert!(matches!(&emits[2], Emit::AudioSeen(AudioCodec::Aac)));
		assert!(state.sdp_audio_emitted);

		let emits = translate_aac(&aac_lc_packet(), &mut state, false);
		assert_eq!(emits.len(), 2);
		assert!(matches!(
			&emits[0],
			Emit::Audio {
				frame: Frame::Audio { pts: 1024, .. },
				..
			}
		));
		assert!(matches!(&emits[1], Emit::AudioSeen(AudioCodec::Aac)));
		assert_eq!(state.aac_pts_next, 2 * 1024);
	}

	/// When bridging, AAC frames are dropped (no Audio, no AudioSeen)
	/// but the PTS counter keeps advancing — the camera's audio cadence
	/// is the wallclock proxy that keeps A/V aligned on Live resume.
	/// SDP still emits so DESCRIBE stays accurate.
	#[test]
	fn aac_bridging_drops_frame_but_advances_pts() {
		let mut state = StreamTranslatorState::default();
		let emits = translate_aac(&aac_lc_packet(), &mut state, true);
		assert_eq!(emits.len(), 1);
		assert!(matches!(&emits[0], Emit::SdpAudio(_)));
		assert_eq!(
			state.aac_pts_next, 1024,
			"PTS must advance through Bridging to keep A/V in sync on resume"
		);
	}

	/// End-to-end PTS continuity across a Bridging window: two live,
	/// three bridged (dropped), then the resume frame carries the
	/// counter that includes the gap.
	#[test]
	fn aac_pts_advances_through_bridging_and_resumes_in_live() {
		let mut state = StreamTranslatorState::default();
		for _ in 0..2 {
			let emits = translate_aac(&aac_lc_packet(), &mut state, false);
			assert!(emits.iter().any(|e| matches!(e, Emit::Audio { .. })));
		}
		assert_eq!(state.aac_pts_next, 2 * 1024);

		for _ in 0..3 {
			let emits = translate_aac(&aac_lc_packet(), &mut state, true);
			assert!(
				!emits.iter().any(|e| matches!(e, Emit::Audio { .. })),
				"no audio during Bridging"
			);
		}
		assert_eq!(
			state.aac_pts_next,
			5 * 1024,
			"PTS advances through Bridging even though no audio reaches the wire"
		);

		let emits = translate_aac(&aac_lc_packet(), &mut state, false);
		match emits
			.iter()
			.find(|e| matches!(e, Emit::Audio { .. }))
			.expect("post-resume frame")
		{
			Emit::Audio {
				frame: Frame::Audio { pts, .. },
				..
			} => assert_eq!(*pts, 5 * 1024, "post-resume PTS reflects the gap"),
			_ => unreachable!(),
		}
		assert_eq!(state.aac_pts_next, 6 * 1024);
	}

	/// `channels == 0` means the channel layout lives in a PCE inside
	/// the AAC body, which this pipeline can't parse — the packet must
	/// drop with no emits and no PTS movement, and the warn latch
	/// records the AOT so a latched stream doesn't log per packet.
	#[test]
	fn aac_pce_channel_config_drops_packet() {
		// Same AAC-LC fixture shape, but byte3's top two bits (ch_low)
		// cleared and byte2's ch_high bit already 0 → channels = 0.
		let mut data = vec![0xFF, 0xF9, 0x60, 0x00, 0x02, 0x00, 0xFC];
		data.extend_from_slice(&[0xAA; 9]);
		let mut state = StreamTranslatorState::default();
		let emits = translate_aac(&BcMediaAac { data: data.clone() }, &mut state, false);
		assert!(emits.is_empty());
		assert_eq!(state.aac_pts_next, 0);
		assert_eq!(state.aac_aot, Some(2), "warn latch records the AOT");
		// Second packet takes the already-latched path and still drops.
		let emits = translate_aac(&BcMediaAac { data }, &mut state, false);
		assert!(emits.is_empty());
	}

	/// A reserved ADTS sample-rate index defeats AudioSpecificConfig
	/// derivation: no SDP emit, and the latch stays unset so the next
	/// packet retries.
	#[test]
	fn aac_unsupported_sample_rate_emits_no_sdp_and_keeps_retrying() {
		// sr_idx=13 (reserved): byte2 = profile 01 | sr_idx 1101 | ch_high 0.
		let mut data = vec![0xFF, 0xF9, 0x74, 0x40, 0x02, 0x80, 0xFC];
		data.extend_from_slice(&[0xAA; 5]);
		let mut state = StreamTranslatorState::default();
		let emits = translate_aac(&BcMediaAac { data }, &mut state, false);
		assert!(
			!emits.iter().any(|e| matches!(e, Emit::SdpAudio(_))),
			"no SDP without a buildable AudioSpecificConfig"
		);
		assert!(!state.sdp_audio_emitted, "latch stays unset for retry");
	}

	#[test]
	fn aac_malformed_adts_translates_to_nothing() {
		let mut state = StreamTranslatorState::default();
		let emits = translate_aac(
			&BcMediaAac {
				data: vec![0x00, 0x01, 0x02],
			},
			&mut state,
			false,
		);
		assert!(emits.is_empty());
		assert_eq!(state.aac_pts_next, 0);
	}

	/// Unsupported AudioObjectType: the SDP emit may ride (ASC is
	/// AOT-agnostic enough to build), but no frame is produced and the
	/// PTS counter must not move — guessing a step would drift A/V.
	#[test]
	fn aac_unsupported_aot_drops_frame_and_leaves_pts() {
		// profile=0 → AOT=1 (AAC Main): unsupported per-AU sample count.
		let mut data = vec![0xFF, 0xF9, 0x20, 0x40, 0x02, 0x00, 0xFC];
		data.extend_from_slice(&[0xAA; 9]);
		let mut state = StreamTranslatorState::default();
		let emits = translate_aac(&BcMediaAac { data }, &mut state, false);
		assert!(!emits.iter().any(|e| matches!(e, Emit::Audio { .. })));
		assert!(!emits.iter().any(|e| matches!(e, Emit::AudioSeen(_))));
		assert_eq!(state.aac_pts_next, 0, "no PTS step without a sample count");
		assert_eq!(state.aac_aot, Some(1), "warn latch records the bad AOT");
	}

	/// First live ADPCM packet mirrors the AAC emit order with G.711
	/// params, and the PTS step equals the transcoded sample count.
	#[test]
	fn adpcm_first_packet_emits_sdp_frame_presence() {
		let mut state = StreamTranslatorState::default();
		let emits = translate_adpcm(&silent_adpcm_packet(), &mut state, false);
		assert_eq!(emits.len(), 3);
		assert!(matches!(
			&emits[0],
			Emit::SdpAudio(a) if a.codec == AudioCodec::G711Ulaw && a.sample_rate == 8_000
		));
		assert!(matches!(
			&emits[1],
			Emit::Audio {
				frame: Frame::Audio { pts: 0, .. },
				..
			}
		));
		assert!(matches!(&emits[2], Emit::AudioSeen(AudioCodec::G711Ulaw)));
		assert!(state.sdp_audio_emitted);
		assert!(state.g711_pts_next > 0, "PTS advances by the sample count");
	}

	/// Bridging mirror of the AAC contract: dropped frame, advancing
	/// counter, no presence emit.
	#[test]
	fn adpcm_bridging_drops_frame_but_advances_pts() {
		let mut state = StreamTranslatorState::default();
		let emits = translate_adpcm(&silent_adpcm_packet(), &mut state, true);
		assert!(!emits.iter().any(|e| matches!(e, Emit::Audio { .. })));
		assert!(!emits.iter().any(|e| matches!(e, Emit::AudioSeen(_))));
		assert!(
			state.g711_pts_next > 0,
			"PTS must advance through Bridging to keep A/V in sync on resume; got {}",
			state.g711_pts_next
		);
	}

	#[test]
	fn adpcm_empty_data_translates_to_nothing() {
		let mut state = StreamTranslatorState::default();
		let emits = translate_adpcm(&BcMediaAdpcm { data: vec![] }, &mut state, false);
		assert!(emits.is_empty());
		assert_eq!(state.g711_pts_next, 0);
	}

	/// A block whose decode fails or decimates to zero samples is
	/// dropped before any emit.
	#[test]
	fn adpcm_short_block_after_decimation_translates_to_nothing() {
		// Minimal header-only block: magic 0x00 0x01 + 2-byte predictor,
		// no body nibbles. Either the decoder rejects it or it produces
		// <2 samples and decimation yields zero — both paths must emit
		// nothing.
		let mut state = StreamTranslatorState::default();
		let emits = translate_adpcm(
			&BcMediaAdpcm {
				data: vec![0x00, 0x01, 0x00, 0x00],
			},
			&mut state,
			false,
		);
		assert!(
			!emits.iter().any(|e| matches!(e, Emit::Audio { .. })),
			"no frame from a block with nothing to transcode"
		);
		assert_eq!(state.g711_pts_next, 0);
	}

	/// The SDP-audio latch is shared across codecs, mirroring the old
	/// shared `sdp_params.audio.is_none()` guard: once AAC announced,
	/// ADPCM must not re-announce.
	#[test]
	fn sdp_audio_latch_is_shared_between_codecs() {
		let mut state = StreamTranslatorState::default();
		let emits = translate_aac(&aac_lc_packet(), &mut state, false);
		assert!(emits.iter().any(|e| matches!(e, Emit::SdpAudio(_))));
		let emits = translate_adpcm(&silent_adpcm_packet(), &mut state, false);
		assert!(!emits.iter().any(|e| matches!(e, Emit::SdpAudio(_))));
	}

	// ── translate: video early-return paths ──────────────────────────

	#[test]
	fn pframe_before_first_iframe_translates_to_nothing() {
		let pframe = BcMediaPframe {
			video_type: VideoType::H264,
			microseconds: 0,
			data: vec![0x00, 0x00, 0x01, 0x41, 0xAA],
		};
		let mut state = StreamTranslatorState::default();
		let (emits, pts) = translate_pframe(&pframe, &mut state);
		assert!(emits.is_empty());
		assert_eq!(pts, None);
	}

	#[test]
	fn pframe_empty_nal_split_translates_to_nothing() {
		let pframe = BcMediaPframe {
			video_type: VideoType::H264,
			microseconds: 0,
			data: vec![],
		};
		let mut state = state_with_codec(VideoCodec::H264);
		let (emits, pts) = translate_pframe(&pframe, &mut state);
		assert!(emits.is_empty());
		assert_eq!(pts, None);
	}

	#[test]
	fn iframe_empty_nal_split_translates_to_nothing() {
		let iframe = BcMediaIframe {
			video_type: VideoType::H264,
			microseconds: 0,
			data: vec![],
			time: None,
		};
		let mut state = StreamTranslatorState::default();
		let (emits, pts) = translate_iframe(&iframe, &mut state, Instant::now());
		assert!(emits.is_empty());
		assert_eq!(pts, None);
	}

	#[test]
	fn iframe_undetectable_codec_translates_to_nothing() {
		// 0x80: forbidden_zero_bit set → detect_codec returns None.
		let iframe = BcMediaIframe {
			video_type: VideoType::H264,
			microseconds: 0,
			data: vec![0x00, 0x00, 0x01, 0x80, 0x00],
			time: None,
		};
		let mut state = StreamTranslatorState::default();
		let (emits, pts) = translate_iframe(&iframe, &mut state, Instant::now());
		assert!(emits.is_empty());
		assert_eq!(pts, None);
	}

	/// An I-frame whose NALs are all non-decodable (Reolink UNSPEC62
	/// metadata) after the codec is already latched: everything filters
	/// out, so no emits and no PTS — the gap marker must not flip.
	#[test]
	fn iframe_with_only_nonstandard_nals_translates_to_nothing() {
		let iframe = BcMediaIframe {
			video_type: VideoType::H265,
			microseconds: 0,
			data: vec![0x00, 0x00, 0x01, 0x7C, 0x01, 0xDE, 0xAD],
			time: None,
		};
		let mut state = state_with_codec(VideoCodec::H265);
		let (emits, pts) = translate_iframe(&iframe, &mut state, Instant::now());
		assert!(emits.is_empty());
		assert_eq!(pts, None);
	}

	#[test]
	fn iframe_drops_h265_unspec62_and_multilayer_nals() {
		// Argus emits HEVC NAL type 62 (UNSPEC62) inside its access units
		// and ffmpeg's RTP-HEVC depacketizer rejects them. Verify both
		// type-62 and a synthetic multi-layer NAL are stripped before
		// the outbound `Frame::Video` is built. Parameter sets (VPS, SPS,
		// PPS) and the IDR slice survive; the IDR is the only NAL on the
		// wire after the in-band parameter-set strip.
		let iframe = BcMediaIframe {
			video_type: VideoType::H265,
			microseconds: 0,
			data: vec![
				// VPS (type 32, byte 0x40, byte 1 0x01)
				0x00, 0x00, 0x01, 0x40, 0x01, 0x0C, 0x01, // SPS (type 33, byte 0x42)
				0x00, 0x00, 0x01, 0x42, 0x01, 0x02, 0x03, 0x04,
				// PPS (type 34, byte 0x44)
				0x00, 0x00, 0x01, 0x44, 0x01, 0xC0,
				// UNSPEC62 (byte 0x7C, byte 1 0x01) — Reolink proprietary metadata.
				0x00, 0x00, 0x01, 0x7C, 0x01, 0xDE, 0xAD, 0xBE, 0xEF,
				// IDR_W_RADL (type 19) with multi-layer nuh_layer_id == 1
				// (byte0 0x27, byte1 0x09) — dropped by the layer-id check.
				0x00, 0x00, 0x01, 0x27, 0x09, 0xCA, 0xFE,
				// Standard IDR_W_RADL (byte 0x26, byte 1 0x01) — survives.
				0x00, 0x00, 0x01, 0x26, 0x01, 0xAA, 0xBB,
			],
			time: None,
		};
		let mut state = StreamTranslatorState::default();
		let (emits, pts) = translate_iframe(&iframe, &mut state, Instant::now());
		assert_eq!(state.detected_codec, Some(VideoCodec::H265));
		let pts = pts.expect("Some");
		let frame = emits
			.iter()
			.find_map(|e| match e {
				Emit::Video { frame, .. } => Some(frame),
				_ => None,
			})
			.expect("frame emitted");
		match frame {
			Frame::Video {
				codec,
				nals,
				keyframe,
				pts_90khz,
				..
			} => {
				assert_eq!(*codec, VideoCodec::H265);
				assert!(keyframe);
				assert_eq!(*pts_90khz, pts);
				// Exactly one NAL on the wire: the standard IDR. Both
				// the UNSPEC62 NAL and the multi-layer IDR were dropped.
				assert_eq!(
					nals.len(),
					1,
					"expected single IDR after filter, got {nals:?}"
				);
				let only = &nals[0];
				assert_eq!(
					only[0], 0x26,
					"first byte should be standard IDR_W_RADL header"
				);
				assert_eq!(only[1], 0x01, "second byte should be layer_id=0, tid+1=1");
			}
			Frame::Audio { .. } => panic!("expected video frame, got audio"),
		}
	}

	#[test]
	fn pframe_drops_h265_unspec62_nals() {
		let pframe = BcMediaPframe {
			video_type: VideoType::H265,
			microseconds: 0,
			data: vec![
				// UNSPEC62 (Reolink proprietary) — must be dropped.
				0x00, 0x00, 0x01, 0x7C, 0x01, 0xDE, 0xAD,
				// Standard TRAIL_R slice (type 1, byte 0x02) — survives.
				0x00, 0x00, 0x01, 0x02, 0x01, 0x11, 0x22,
			],
		};
		let mut state = state_with_codec(VideoCodec::H265);
		let (emits, pts) = translate_pframe(&pframe, &mut state);
		let pts = pts.expect("Some");
		assert!(matches!(&emits[0], Emit::AppendPframe(nals) if nals.len() == 1));
		match &emits[1] {
			Emit::Video {
				frame:
					Frame::Video {
						codec,
						nals,
						keyframe,
						pts_90khz,
						..
					},
				..
			} => {
				assert_eq!(*codec, VideoCodec::H265);
				assert!(!keyframe);
				assert_eq!(*pts_90khz, pts);
				assert_eq!(nals.len(), 1, "only standard slice should remain");
				assert_eq!(nals[0][0], 0x02);
			}
			other => panic!("expected video frame, got {other:?}"),
		}
	}

	#[test]
	fn pframe_with_only_nonstandard_nals_translates_to_nothing() {
		// A P-frame containing exclusively non-decodable NALs is dropped
		// (no emits, no `last_live_frame_at` update upstream).
		let pframe = BcMediaPframe {
			video_type: VideoType::H265,
			microseconds: 0,
			data: vec![0x00, 0x00, 0x01, 0x7C, 0x01, 0xAB, 0xCD],
		};
		let mut state = state_with_codec(VideoCodec::H265);
		let (emits, pts) = translate_pframe(&pframe, &mut state);
		assert!(emits.is_empty());
		assert_eq!(pts, None);
	}

	#[test]
	fn iframe_short_sps_populates_zero_profile_level_id() {
		// SPS with len < 4 → profile_level_id falls back to [0u8; 3].
		// 0x67 = SPS; only 2 bytes (short). 0x68 = PPS; 0x65 = IDR.
		let iframe = BcMediaIframe {
			video_type: VideoType::H264,
			microseconds: 0,
			data: vec![
				0x00, 0x00, 0x01, 0x67, 0x42, // SPS only 2 bytes
				0x00, 0x00, 0x01, 0x68, 0xce, // PPS
				0x00, 0x00, 0x01, 0x65, 0xaa, // IDR
			],
			time: None,
		};
		let mut state = StreamTranslatorState::default();
		let (emits, _) = translate_iframe(&iframe, &mut state, Instant::now());
		match &emits[0] {
			Emit::SdpVideo(v) => assert_eq!(v.profile_level_id, [0u8; 3]),
			other => panic!("expected SdpVideo first, got {other:?}"),
		}
	}

	/// A second keyframe re-emits SDP video (refresh semantics) and the
	/// pace duration reflects the inter-frame PTS delta.
	#[test]
	fn second_iframe_paces_by_pts_delta() {
		let build = |micros: u32| BcMediaIframe {
			video_type: VideoType::H264,
			microseconds: micros,
			data: vec![
				0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1F, // SPS
				0x00, 0x00, 0x01, 0x68, 0xce, // PPS
				0x00, 0x00, 0x01, 0x65, 0xaa, // IDR
			],
			time: None,
		};
		let mut state = StreamTranslatorState::default();
		let (emits, _) = translate_iframe(&build(0), &mut state, Instant::now());
		match emits.last() {
			Some(Emit::Video { pace, .. }) => assert_eq!(*pace, Duration::ZERO),
			other => panic!("expected trailing Video emit, got {other:?}"),
		}
		// 100 ms later on the camera clock → 100 ms pace.
		let (emits, _) = translate_iframe(&build(100_000), &mut state, Instant::now());
		match emits.last() {
			Some(Emit::Video { pace, .. }) => assert_eq!(*pace, Duration::from_millis(100)),
			other => panic!("expected trailing Video emit, got {other:?}"),
		}
	}

	// ── pure helpers ──────────────────────────────────────────────────

	#[test]
	fn video_frame_duration_decision_table() {
		let mut state = StreamTranslatorState::default();
		// First frame: emit immediately.
		assert_eq!(video_frame_duration(&mut state, 90_000), Duration::ZERO);
		// Normal 1 s advance.
		assert_eq!(
			video_frame_duration(&mut state, 180_000),
			Duration::from_secs(1)
		);
		// Anomalous jump past the 5 s cap: emit immediately.
		assert_eq!(
			video_frame_duration(&mut state, 180_000 + PACER_ANOMALY_CAP_TICKS + 1),
			Duration::ZERO
		);
		// Backward PTS (wrapping_sub produces a huge delta): capped to 0.
		let prev = state.last_video_pts_90khz.expect("latched");
		assert_eq!(video_frame_duration(&mut state, prev - 1), Duration::ZERO);
	}

	#[test]
	fn micros_to_90khz_edge_cases() {
		assert_eq!(micros_to_90khz(0), 0);
		// 1 second = 1_000_000 µs → 90_000 ticks.
		assert_eq!(micros_to_90khz(1_000_000), 90_000);
		// 100 µs = 9 ticks.
		assert_eq!(micros_to_90khz(100), 9);
		// Large-but-representable value wraps cleanly inside u32.
		let big = u32::MAX / 10;
		// Must not panic (wrapping arithmetic).
		let _ = micros_to_90khz(big);
	}

	#[test]
	fn aac_samples_per_au_branches_on_aot() {
		// AAC-LC → 1024 samples/AU.
		assert_eq!(aac_samples_per_au(2), Some(1024));
		// HE-AAC (SBR) and HE-AACv2 (PS) both double to 2048/AU.
		assert_eq!(aac_samples_per_au(5), Some(2048));
		assert_eq!(aac_samples_per_au(29), Some(2048));
		// Unsupported AOTs: we have no confirmed sample count, so the
		// helper reports None and the caller drops the frame.
		assert_eq!(aac_samples_per_au(1), None);
		assert_eq!(aac_samples_per_au(3), None);
		assert_eq!(aac_samples_per_au(4), None);
		assert_eq!(aac_samples_per_au(0), None);
		assert_eq!(aac_samples_per_au(255), None);
	}

	#[test]
	fn paced_audio_duration_aac_lc_at_16khz_is_64ms() {
		assert_eq!(
			paced_audio_duration(1024, 16_000),
			Duration::from_micros(64_000)
		);
	}

	#[test]
	fn paced_audio_duration_g711_at_8khz_per_byte_is_125us() {
		assert_eq!(
			paced_audio_duration(160, 8_000),
			Duration::from_micros(20_000)
		);
	}

	#[test]
	fn paced_audio_duration_zero_sample_rate_returns_zero() {
		assert_eq!(paced_audio_duration(1024, 0), Duration::ZERO);
	}

	// ── NAL classification helpers ────────────────────────────────────

	#[test]
	fn is_parameter_set_nal_empty_returns_false() {
		assert!(!is_parameter_set_nal(&[], VideoCodec::H264));
		assert!(!is_parameter_set_nal(&[], VideoCodec::H265));
	}

	#[test]
	fn is_parameter_set_nal_h264_sps_and_pps_match() {
		// 0x67 = nal_ref_idc=3, type=7 (SPS). 0x68 = type=8 (PPS).
		assert!(is_parameter_set_nal(&[0x67, 0x00], VideoCodec::H264));
		assert!(is_parameter_set_nal(&[0x68, 0x00], VideoCodec::H264));
		// 0x65 = IDR slice — not a parameter set.
		assert!(!is_parameter_set_nal(&[0x65, 0x00], VideoCodec::H264));
	}

	#[test]
	fn is_parameter_set_nal_h265_vps_sps_pps_match() {
		assert!(is_parameter_set_nal(&[0x40, 0x01], VideoCodec::H265));
		assert!(is_parameter_set_nal(&[0x42, 0x01], VideoCodec::H265));
		assert!(is_parameter_set_nal(&[0x44, 0x01], VideoCodec::H265));
		assert!(!is_parameter_set_nal(&[0x26, 0x01], VideoCodec::H265));
	}

	#[test]
	fn is_slice_nal_empty_returns_false() {
		assert!(!is_slice_nal(&[], VideoCodec::H264));
		assert!(!is_slice_nal(&[], VideoCodec::H265));
	}

	#[test]
	fn is_slice_nal_recognises_h264_vcl_types() {
		assert!(is_slice_nal(&[0x41, 0x00], VideoCodec::H264));
		assert!(is_slice_nal(&[0x65, 0x00], VideoCodec::H264));
		assert!(!is_slice_nal(&[0x67, 0x00], VideoCodec::H264));
		assert!(!is_slice_nal(&[0x68, 0x00], VideoCodec::H264));
	}

	#[test]
	fn is_slice_nal_recognises_h265_vcl_types() {
		assert!(is_slice_nal(&[0x02, 0x01], VideoCodec::H265));
		assert!(is_slice_nal(&[0x26, 0x01], VideoCodec::H265));
		assert!(!is_slice_nal(&[0x40, 0x01], VideoCodec::H265));
	}

	#[test]
	fn extract_iframe_parts_h264_splits_sps_pps_idr_and_skips_sei() {
		let sps = [0x67u8, 0x42, 0x00, 0x1F];
		let pps = [0x68u8, 0xCE, 0x3C, 0x80];
		let sei = [0x06u8, 0x00];
		let idr = [0x65u8, 0xAA, 0xBB];
		let nals: Vec<&[u8]> = vec![&sps, &pps, &sei, &idr];
		let (params, iframes, out_sps, out_pps, out_vps) =
			extract_iframe_parts(VideoCodec::H264, &nals);
		assert_eq!(params.len(), 2);
		assert_eq!(iframes.len(), 1);
		assert!(out_sps.is_some() && out_pps.is_some() && out_vps.is_none());
	}

	#[test]
	fn extract_iframe_parts_h265_collects_vps_sps_pps_and_idr() {
		let vps = [0x40u8, 0x01, 0x0C, 0x01];
		let sps = [0x42u8, 0x01, 0x02];
		let pps = [0x44u8, 0x01, 0xC0];
		let idr = [0x26u8, 0x01, 0xAF];
		let nals: Vec<&[u8]> = vec![&vps, &sps, &pps, &idr];
		let (params, iframes, out_sps, out_pps, out_vps) =
			extract_iframe_parts(VideoCodec::H265, &nals);
		assert_eq!(params.len(), 3);
		assert_eq!(iframes.len(), 1);
		assert!(out_vps.is_some() && out_sps.is_some() && out_pps.is_some());
	}

	#[test]
	fn extract_iframe_parts_skips_empty_nals() {
		let empty: &[u8] = &[];
		let sps = [0x67u8, 0x42];
		let idr = [0x65u8, 0xAA];
		let (_, iframes, _, _, _) = extract_iframe_parts(VideoCodec::H264, &[empty, &sps, &idr]);
		assert_eq!(iframes.len(), 1);
	}

	#[test]
	fn extract_iframe_parts_h265_skips_empty_nal_and_non_parameter() {
		// Empty NAL hits the H.265 `continue`; a TRAIL slice (type 1,
		// a VCL NAL that is neither parameter set nor IDR-class) hits
		// the default `_ => {}` arm.
		let empty: &[u8] = &[];
		let trail = [0x02u8, 0x01, 0x00]; // type=1 (TRAIL_N)
		let vps = [0x40u8, 0x01, 0xaa];
		let idr = [0x26u8, 0x01, 0xbb]; // IDR_W_RADL
		let (params, iframes, _, _, out_vps) =
			extract_iframe_parts(VideoCodec::H265, &[empty, &trail, &vps, &idr]);
		assert_eq!(params.len(), 1);
		assert_eq!(iframes.len(), 1);
		assert!(out_vps.is_some());
	}
}
