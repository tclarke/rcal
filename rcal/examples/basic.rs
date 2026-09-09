//! Basic rcal_main example.
//!
//! Demonstrates the default `#[rcal_macros::rcal_main]` macro: injects a
//! tokio runtime, loads CAL config, and creates a `root_logger`.
//!
//! Run:
//!   cargo run --example basic

use std::sync::Arc;

use rcal::cal;
use slog::{error, info, warn};

#[rcal_macros::rcal_main(config = "examples/CALConfig.toml")]
async fn main() {
    info!(root_logger, "basic example starting");

    // `rcal_config` and `root_logger` are injected by the macro.
    let config = Arc::new(rcal_config);

    let mut bus = match cal::get_cal(
        "basic-example",
        Some("default"),
        config,
        root_logger.clone(),
    )
    .await
    {
        Ok(b) => b,
        Err(e) => {
            error!(root_logger, "fatal: failed to create ASB"; "error" => %e);
            std::process::exit(1);
        }
    };

    info!(root_logger, "ASB created successfully");

    match bus.close() {
        Ok(()) => info!(root_logger, "done"),
        Err(e) => warn!(root_logger, "ASB close error"; "error" => %e),
    }
}
