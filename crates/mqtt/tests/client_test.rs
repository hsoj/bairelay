#[test]
fn mqtt_config_builds_client_options() {
	let config = bairelay_mqtt::MqttConfig {
		broker_addr: "192.168.1.10".to_string(),
		port: 1883,
		credentials: None,
		ca: None,
		client_auth: None,
	};
	// Construction shouldn't panic
	let _opts = config.to_mqtt_options("bairelay-test");
}

#[test]
fn mqtt_config_with_credentials() {
	let config = bairelay_mqtt::MqttConfig {
		broker_addr: "broker.local".to_string(),
		port: 8883,
		credentials: Some(("user".to_string(), "pass".to_string())),
		ca: None,
		client_auth: None,
	};
	let _opts = config.to_mqtt_options("bairelay-auth");
}
