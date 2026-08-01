// ── Vendored Baichuan protocol ────────────────────────────────────────
// Derived from neolink_core by thirtythreeforty + QuantumEntangledAndy
// (https://github.com/QuantumEntangledAndy/neolink), AGPL-3.0-or-later.
// Kept in one directory so its provenance stays legible.
pub mod baichuan;

// ── Protocol servers and bridges ──────────────────────────────────────
pub mod mqtt;
pub mod rtsp;
pub mod wake_server;

// ── Camera domain ─────────────────────────────────────────────────────
pub mod audio_presence;
pub mod battery;
pub mod bc_camera;
pub mod bc_opts;
pub mod bcmedia_dump;
pub mod camera;
pub mod camera_provider;
pub mod camera_services;
pub mod camera_status;
pub mod camera_tasks;
pub mod capabilities;
pub mod cli;
pub mod cli_convert;
pub mod config;
// Test-only: a scripted stand-in for a real camera. Gated so a release
// build cannot substitute it for the real thing.
#[cfg(test)]
pub mod fake_camera;
pub mod gap_bridging;
pub mod grace_period;
pub mod hassio;
pub mod local_time;
// Test-only: pins the log strings `tests/scripts/manual-verify.sh` greps.
#[cfg(test)]
mod log_capture;
pub mod mqtt_dispatch;
pub mod mqtt_loop;
pub mod mqtt_status;
pub mod oneshot;
pub mod orchestrator;
pub mod preview_overlay;
pub mod preview_state;
pub mod ptz;
pub mod push_listener;
pub mod run_support;
pub mod startup_wake;
pub mod status_cache;
pub mod stream_source;
pub mod supervisor;
pub mod tls_load;
pub mod wake_lock;
pub mod watchdog;
