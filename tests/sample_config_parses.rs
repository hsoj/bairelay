// keep sample_config.toml in lockstep with the config surface.
// If someone adds a new config field and forgets to document it in
// sample_config.toml, the sample stays valid (TOML is permissive).
// What we CAN check cheaply is the reverse: that every key the sample
// declares still parses + validates against the current code. This
// catches renamed/removed fields breaking the example operators copy.
#[test]
fn sample_config_parses_and_validates() {
	let path = concat!(env!("CARGO_MANIFEST_DIR"), "/sample_config.toml");
	let src = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {}", path, e));
	let cfg =
		bairelay::config::parse_config(&src).unwrap_or_else(|e| panic!("parse {}: {}", path, e));
	bairelay::config::validate_config(&cfg).unwrap_or_else(|e| panic!("validate {}: {}", path, e));
	// Sanity: the sample should declare at least two concrete camera
	// blocks (the uncommented examples) so operators have shapes to
	// copy.
	assert!(
		cfg.cameras.len() >= 2,
		"sample should keep ≥2 uncommented camera blocks; got {}",
		cfg.cameras.len(),
	);
}
