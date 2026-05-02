use thiserror::Error;

#[derive(Debug, Error)]
pub enum MqttError {
	#[error("MQTT connection error: {0}")]
	ConnectionError(String),

	#[error("MQTT publish error: {0}")]
	PublishError(String),

	#[error("MQTT subscribe error: {0}")]
	SubscribeError(String),

	#[error("MQTT client error: {0}")]
	ClientError(#[from] rumqttc::ClientError),

	#[error("MQTT connection failure: {0}")]
	ConnectionFailure(#[from] Box<rumqttc::ConnectionError>),

	#[error("invalid configuration: {0}")]
	ConfigError(String),
}
