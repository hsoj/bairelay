#![no_main]

use bytes::BytesMut;
use libfuzzer_sys::fuzz_target;
use bairelay::baichuan::bcudp::model::BcUdp;

fuzz_target!(|data: &[u8]| {
	let mut buf = BytesMut::from(data);
	let _ = BcUdp::deserialize(&mut buf);
});
