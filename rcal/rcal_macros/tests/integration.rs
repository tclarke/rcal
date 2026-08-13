use rcal_macros::init_test_logger;
use slog::info;

#[test]
fn test_empty() {
    let test_logger = init_test_logger!();
    info!(test_logger, "A test log message");
}
