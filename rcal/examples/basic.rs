//! Basic rcal_main example.
//!
//! Demonstrates the default `#[rcal_macros::rcal_main]` macro: injects a
//! tokio runtime, loads CAL config, and creates a `root_logger`.
//!
//! Run:
//!   RCAL_CONFIG=CALConfig.toml cargo run --example basic

use std::sync::Arc;

use rcal::asb;
use slog::{error, info, warn};

#[rcal_macros::rcal_main]
async fn main() {
    info!(root_logger, "basic example starting");

    // `__rcal_config` and `root_logger` are injected by the macro.
    let config = Arc::new(__rcal_config);

    let bus = match asb::get_asb("basic-example", "default", config, root_logger.clone()).await {
        Ok(b) => b,
        Err(e) => {
            error!(root_logger, "fatal: failed to create ASB"; "error" => %e);
            std::process::exit(1);
        }
    };

    info!(root_logger, "ASB created successfully");

    match bus.lock().unwrap().close() {
        Ok(()) => info!(root_logger, "done"),
        Err(e) => warn!(root_logger, "ASB close error"; "error" => %e),
    }
}
