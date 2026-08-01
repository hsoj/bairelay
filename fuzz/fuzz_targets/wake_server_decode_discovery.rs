#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
	let _ = bairelay::wake_server::packet::decode_discovery(data);
});
