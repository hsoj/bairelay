#![no_main]

use bairelay::rtsp::codec::nal::{is_decodable_nal, split_annex_b};
use bairelay::rtsp::codec::VideoCodec;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
	for nal in split_annex_b(data) {
		let _ = is_decodable_nal(nal, VideoCodec::H264);
		let _ = is_decodable_nal(nal, VideoCodec::H265);
	}
});
