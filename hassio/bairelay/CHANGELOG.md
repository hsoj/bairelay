# Bairelay HA Add-On — Changelog

## 1.1.2

Maintenance release:
- Renamed crates to avoid clashing namespaces
- Published to crates.io
- Updated CI to fix code review issues
- Added automattic crates.io publishing

### Changes

- codeql: build-mode none + v4 actions
- ci: explicit workflow permissions + codeql advanced setup
- ci: auto-publish to crates.io on release publish
- bairelay: fix cargo exclude patterns with leading slash
- bairelay-neolink-core: pin env_logger dev-dep for crates.io
- crates: exclude unneeded files.
- crates: update documentation, add publish-crates.sh
- crates: workspace.dependencies, polish, release.sh bump
- rename: bairelay-* namespace for crates.io publishing
## 1.1.1

HAOS App fixes and improvements:
- separate `address` / `uid` / `username` / `idle_disconnect` fields per camera (was a single `host_or_uid` blob).
- `/config/bairelay/config.toml` is now the authoritative editable config — bootstrapped from an annotated template on first start, merged with the form on every restart. The file no longer needs to restate `username` for partial-overrride camera entries.
- `/config/bairelay/effective.toml` written every start (read-only diagnostic of the merged result, with defaults stripped).
- HA MQTT discovery auto-enabled at default topic `homeassistant`; entities appear in HA without manual `[mqtt.discovery]` configuration.
- Configuration form labels and inline descriptions localised via `translations/en.yaml`.
       
### Changes

- release: mirror CHANGELOG section to hassio/bairelay/CHANGELOG.md
- hassio: config.toml authoritative; auto-discovery; UI labels
- hassio: split address/uid/username; add idle_disconnect (default true)
- hassio: pass --mqtt-ssl as flag; silence bashio Service-not-enabled noise
- docs: HA UI is Settings -> Apps -> Install app (renamed in 2026.2)
- tests: ip-monitor-route daemon kills Colima's re-added eth0 route
- release: arm64 case match; build+release skip on release:published
- ci: force rustup default stable after dtolnay; print rustup show
- Updated CHANGELOG.md and fixed release script.
- ci: pin toolchain via rust-toolchain.toml
## 1.1.0

Initial release. RTSP / MQTT bridge for Reolink battery cameras, packaged as a Home Assistant Add-On. Configuration via the HA options form (identity per camera) + an optional `/config/bairelay/config.toml` overlay for advanced settings.
