const XML_KEY: [u32; 8] = [
	0x1f2d3c4b, 0x5a6c7f8d, 0x38172e4b, 0x8271635a, 0x863f1a2b, 0xa5c6f7d8, 0x8371e1b4, 0x17f2d3a5,
];

pub(crate) fn decrypt(offset: u32, buf: &[u8]) -> Vec<u8> {
	let key = XML_KEY
		.iter()
		.flat_map(|i| i.wrapping_add(offset).to_le_bytes())
		.cycle();
	buf.iter().zip(key).map(|(byte, key)| key ^ byte).collect()
}

pub(crate) fn encrypt(offset: u32, buf: &[u8]) -> Vec<u8> {
	decrypt(offset, buf)
}

#[test]
fn test_udp_xml_crypto() {
	let sample = include_bytes!("samples/xml_crypto_sample1.bin");
	let should_be = include_bytes!("samples/xml_crypto_sample1_plaintext.bin");

	let decrypted = decrypt(87, &sample[..]);
	assert_eq!(decrypted, &should_be[..]);
}

#[test]
fn test_udp_xml_crypto_roundtrip() {
	let zeros: [u8; 256] = [0; 256];

	let decrypted = encrypt(0, &zeros[..]);
	let encrypted = decrypt(0, &decrypted[..]);
	assert_eq!(encrypted, &zeros[..]);
}

#[test]
fn test_udp_xml_crypto_high_tid_does_not_overflow() {
	// Regression for the `i + offset` overflow that panicked in debug
	// builds whenever the tid (used as the XOR offset by the wake server)
	// pushed at least one of the eight `XML_KEY` words past `u32::MAX`.
	// Wrapping is the correct semantics: the wire format is symmetric XOR
	// and the reference implementation already wraps.
	let zeros = [0u8; 256];
	let enc = encrypt(0xdeadbeef, &zeros);
	let dec = decrypt(0xdeadbeef, &enc);
	assert_eq!(dec, zeros);

	let enc_max = encrypt(u32::MAX, &zeros);
	let dec_max = decrypt(u32::MAX, &enc_max);
	assert_eq!(dec_max, zeros);
}
