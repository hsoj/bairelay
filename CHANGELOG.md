# Changelog

## [1.0.0] — 2026-05-03

First public binary release.

### Changes

- set_time: read DST and compensate, fix +1h drift
- mqtt: cache + republish status on broker ConnAck for HA recovery
- mqtt: title-case preserves caps + splits on `-`; fix Floodlight Tasks

## [0.9.0] — 2026-05-02

Initial repo published.

Bairelay is a pure-Rust replacement for [Neolink](https://github.com/QuantumEntangledAndy/neolink) for Reolink battery cameras (Argus class). Relevant Baichuan-protocol code from Neolink is vendored in `neolink_core`; everything else is a clean rewrite.

### Highlights vs Neolink
- Pure Rust, no GStreamer — single static binary, no native deps.
- Stable jitter-free RTSP / RTSPS with audio + video pacers, gap bridging and proper battery-camera lifecycle.
- Tighter Home Assistant integration via MQTT including image previews with overlays, reading camera presets, PTZ and more.
- HEVC handling that survives Home Assistant's `ffmpeg:` re-publish and go2rtc's snapshot transcoder.
- Local replacements for Reolink's cloud wake server `p2p.reolink.com` and motion-push listener `pushx.reolink.com` (experimental).
- Improved one-shot CLI subcommands and a `check-config` validator with a coarse exit-code table.
- TLS (`rtsps://`) parallel listener with optional client-cert mTLS.
- Drop-in compatible with Neolink configs via `mqtt.topic_prefix = "neolink"`.
- >2000 workspace tests, fuzz harness, live-verify rigs against real Argus hardware.

### Deferred
- Two-way audio (`talk`).

See `README.md` for full rationale and acknowledgements.
