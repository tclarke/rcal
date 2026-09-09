use rcal_macros::{init_test_logger, rcal_trace};
use slog::{Logger, info, o};

#[init_test_logger]
#[test]
fn test_empty() {
    info!(logger, "A test log message");
}

struct Traced {
    logger: Logger,
}

#[rcal_trace]
impl Traced {
    fn method_one(&self) {}
    fn method_two(&self) -> i32 {
        42
    }
}

impl Traced {
    #[rcal_trace]
    fn single_method(&self) {}

    #[rcal_trace(logger = self.logger)]
    fn explicit_logger(&self) {}
}

#[rcal_trace]
fn bare_fn(logger: &Logger) {
    let _ = logger;
}

#[init_test_logger]
#[test]
fn test_rcal_trace_impl() {
    let t = Traced {
        logger: logger.new(o!()),
    };
    t.method_one();
    assert_eq!(t.method_two(), 42);
    t.single_method();
    t.explicit_logger();
    bare_fn(&t.logger);
}
