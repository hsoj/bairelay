use std::time::Duration;

#[test]
fn backoff_increases_exponentially() {
	let mut backoff =
		bairelay::camera::ReconnectBackoff::new(Duration::from_secs(1), Duration::from_secs(60));
	assert_eq!(backoff.next_delay(), Duration::from_secs(1));
	assert_eq!(backoff.next_delay(), Duration::from_secs(2));
	assert_eq!(backoff.next_delay(), Duration::from_secs(4));
	assert_eq!(backoff.next_delay(), Duration::from_secs(8));
}

#[test]
fn backoff_caps_at_max() {
	let mut backoff =
		bairelay::camera::ReconnectBackoff::new(Duration::from_secs(1), Duration::from_secs(5));
	for _ in 0..10 {
		backoff.next_delay();
	}
	assert_eq!(backoff.next_delay(), Duration::from_secs(5));
}

#[test]
fn backoff_resets() {
	let mut backoff =
		bairelay::camera::ReconnectBackoff::new(Duration::from_secs(1), Duration::from_secs(60));
	backoff.next_delay();
	backoff.next_delay();
	backoff.reset();
	assert_eq!(backoff.next_delay(), Duration::from_secs(1));
}
