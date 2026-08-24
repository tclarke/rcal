//! Custom tokio runtime example.
//!
//! Demonstrates `#[rcal_macros::rcal_main]` with `tokio_main = false` so the
//! caller can supply a custom `#[tokio::main]` attribute (e.g. to control
//! worker thread count).
//!
//! Run:
//!   cargo run --example custom_tokio

use std::sync::Arc;

use rcal::cal;
use slog::{error, info, warn};

#[rcal_macros::rcal_main(
    tokio_main = false,
    logger = "my_logger",
    config = "examples/CALConfig.toml"
)]
#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    info!(my_logger, "custom_tokio example starting";
          "workers" => 4);

    // `rcal_config` and `my_logger` are injected by the macro.
    let config = Arc::new(rcal_config);

    let bus = match cal::get_cal("custom-example", "default", config, my_logger.clone()).await {
        Ok(b) => b,
        Err(e) => {
            error!(my_logger, "fatal: failed to create ASB"; "error" => %e);
            std::process::exit(1);
        }
    };

    info!(my_logger, "ASB created successfully");

    match bus.lock().unwrap().close() {
        Ok(()) => info!(my_logger, "done"),
        Err(e) => warn!(my_logger, "ASB close error"; "error" => %e),
    }
}
