# Bairelay

## What this is

Bairelay bridges Reolink battery cameras (Argus-class) to standard protocols. It exposes RTSP for video and uses MQTT for camera state, control, and Home Assistant discovery. Battery cameras wake on demand and sleep again after the RTSP session ends, so HA cards and automations work without keeping the camera awake all the time.

## Installation

- Go to **Settings → Apps** and click **Install app** (bottom-right).
- Click the three-dot menu (⋮) → **Repositories**.
- Paste `https://github.com/mgc8/bairelay` and click **Add**.
- The Bairelay app appears in the list. Click it, then **Install**.
- Switch to the **Configuration** tab and fill in the three options fields (`topic_prefix`, `log_level`, `cameras` list).
- **Start**. The add-on automatically uses HA's Mosquitto integration; no broker config is needed if MQTT is already set up in HA.

## Configuration: options form

| Field | Type | Description |
|-------|------|-------------|
| `topic_prefix` | string | MQTT topic root, default `bairelay`. Set to `neolink` for drop-in compatibility with an existing neolink deployment. |
| `log_level` | enum: `info` / `debug` / `trace` | Default `info`. `debug` surfaces protocol-level traces; `trace` is everything. |
| `cameras` | list | One entry per camera. Each entry has the sub-fields below. |

Each camera entry:

| Sub-field | Required | Description |
|-----------|----------|-------------|
| `name` | yes | Alphanumeric, underscore, or hyphen. Used as the camera's MQTT topic segment and HA entity name. |
| `address` | one of address/uid | LAN IP or hostname (e.g. `192.168.1.50`). Leave blank if using `uid`. |
| `uid` | one of address/uid | Reolink P2P UID (16 alphanumeric characters, e.g. `9527000ABCDEF123`). Leave blank if using `address`. |
| `username` | no | Camera account username. Defaults to `admin` (Reolink's stock account). |
| `password` | yes | The camera account password. |
| `idle_disconnect` | no | Drop the camera connection when no clients are streaming, letting battery cameras sleep. Defaults to `true`. Turn off for always-on cameras you want kept warm. |

Fill in exactly one of `address` or `uid` per camera. If both are present, `uid` is used and `address` is ignored.

## Configuration: TOML overlay

Settings the HA options form doesn't expose — TLS, wake server, push listener, per-camera floodlight / PIR / pause / gap-bridging, discovery mode, a custom RTSP port — live in a TOML overlay file at `/config/bairelay/config.toml`.

- Use the HA **File editor** add-on or SSH to create and edit it.
- The overlay is merged on top of the HA options. Cameras are matched by `name`; overlay fields override base fields.
- See `sample_config.toml` in the bairelay repo for the full list of available settings: <https://github.com/mgc8/bairelay/blob/main/sample_config.toml>.

## Worked example: TOML overlay

A minimal overlay that enables the wake server and turns on the floodlight for one camera:

```toml
# /config/bairelay/config.toml — bairelay overlay
#
# Override or extend the configuration generated from the HA options form.
# Per-camera entries match by name; you can specify just the fields you
# want to change.

[wake_server]
enable = true

[[cameras]]
name = "Hallway"
username = "admin"  # required for any [[cameras]] entry, even if unchanged
enable_floodlight = true
```

**Important operator-facing gotcha (do not skip):**

Every `[[cameras]]` block in your overlay must include a `username` line, even when you're only overriding one other field. The base config from the HA options form always sets `username = "admin"` (Reolink's stock account), but the TOML parser sees the overlay file in isolation before the merge step, and `username` has no default at the parser level. You will see a parse error on startup if you forget. A fix to make this field default-friendly is on the bairelay roadmap; for now, restate it.

## Troubleshooting

### Cameras don't appear in Home Assistant

Check that the MQTT integration is installed and enabled in HA: **Settings → Devices & Services**. The add-on auto-discovers it via the `mqtt:want` service link. Also check the add-on's **Log** tab — connection failures surface there.

### Battery cameras never wake or never connect

The wake server is opt-in via the TOML overlay (`[wake_server] enable = true`) **and** requires a DNS hijack at your LAN's DNS resolver so the cameras' outbound P2P traffic to `p2p*.reolink.com` resolves to your Home Assistant host instead of Reolink's cloud. Configure the resolver to map those hostnames to the HA IP, then restart the cameras. See `docs/cloud-interception.md` Part I in the bairelay repo for wire-level details.

### Logs

The **Log** tab on the add-on page shows the running output. Set `log_level: debug` in the options to surface bairelay-internal protocol traces, or `trace` for noisier per-packet detail. The add-on entrypoint sets `RUST_LOG` automatically based on `log_level`.

## Network ports

The add-on uses host networking. By default it binds:

- `8554/tcp` — plain RTSP (always on).
- `8555/tcp` — `rtsps://` (only when the overlay sets `certificate = "..."`).
- `9999/udp` and `58200/udp` — wake server (only when `[wake_server] enable = true`).

The push listener (a separate motion-detection mechanism that intercepts the camera's `pushx.reolink.com` connection) is opt-in and configurable to any port. Configure it in the overlay only if you need it.

## Repository

Source code, issue tracker, full sample config, and deeper docs: <https://github.com/mgc8/bairelay>.
