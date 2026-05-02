---
name: Bug report
about: Something is misbehaving — crashes, wrong output, regressions
title: "[bug] "
labels: ["bug"]
---

## Summary

<!-- One sentence: what's broken. -->

## Environment

- **bairelay version:** <!-- output of `bairelay --version` -->
- **Platform:** <!-- Linux/macOS/Windows + arch -->
- **Camera model + firmware:** <!-- e.g. Argus Eco / v3.0.0.5649_25111355 -->
- **Run mode:** <!-- mqtt-rtsp / rtsp / mqtt / one-shot CLI -->

## Steps to reproduce

1.
2.
3.

## What you expected to happen

## What actually happened

## Logs

<!--
Run with `RUST_LOG=bairelay=debug` (or `bairelay_wake_server=debug` for
wake-server bugs) and paste the relevant section here. Redact any
camera UIDs / passwords / IPs you don't want public.
-->

```
<paste log here>
```

## Configuration

<!--
Paste the relevant subset of your config.toml (with credentials
redacted). For RTSP/MQTT bugs the [mqtt] block + the affected
[[cameras]] block is usually enough.
-->

```toml
<paste config snippet here>
```

## Additional context

<!-- Pcaps, screenshots, related issues, anything else useful. -->
