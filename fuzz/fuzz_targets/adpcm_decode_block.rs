#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
	let mut dec = bairelay::rtsp::transcode::adpcm::AdpcmDecoder::new();
	let _ = dec.decode_block(data);
});
