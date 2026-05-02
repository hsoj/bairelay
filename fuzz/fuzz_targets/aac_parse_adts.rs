#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
	let _ = bairelay_rtsp::codec::aac::parse_adts(data);
});
