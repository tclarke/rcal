#[macro_export]
macro_rules! init_test_logger {
    () => ({
        use slog::{Drain, Logger, o};
        use slog_term::{FullFormat, TermDecorator};
        use slog_async::Async;

        // Decorator that targets stdout for Cargo's test capture framework
        let decorator = TermDecorator::new().stdout().build();

        // Mutex makes the drain safe to share across concurrent test threads
        let drain = FullFormat::new(decorator).build().fuse();
        let drain = Async::new(drain).build().fuse();

        Logger::root(drain, o!("test_context" => "unit_tests"))
    })
}
