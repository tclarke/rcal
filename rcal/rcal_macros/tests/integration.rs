use rcal_macros::init_test_logger;
use slog::info;

#[init_test_logger]
#[test]
fn test_empty() {
    info!(logger, "A test log message");
}
