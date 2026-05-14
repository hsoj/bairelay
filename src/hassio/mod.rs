//! HA add-on glue: parse Supervisor's options.json + an optional TOML
//! overlay into a single bairelay [`crate::config::Config`], validate,
//! and emit. Exposed via the `bairelay render-hassio-config` subcommand.

pub mod merge;
pub mod options;
