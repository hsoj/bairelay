#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
	let _ = bairelay_neolink_core::fuzz_api::parse_bc(data);
});
