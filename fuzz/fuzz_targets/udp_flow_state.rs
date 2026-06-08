#![no_main]

use libfuzzer_sys::fuzz_target;
use bairelay_neolink_core::fuzz_api;

fuzz_target!(|data: &[u8]| {
	fuzz_api::flow_state_drive_arbitrary(data);
});
