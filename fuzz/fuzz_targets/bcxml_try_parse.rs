#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
	let _ = bairelay::baichuan::fuzz_api::parse_bc_xml(data);
});
