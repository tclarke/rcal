//! Logger construction from [`LoggingConfig`].

use crate::calconfig::{LogFormat, LoggingConfig, SinkConfig, SinkType};
use slog::{Drain, KV, Logger, o};
use slog_async::Async;
use slog_term::{FullFormat, PlainDecorator, TermDecorator};
use std::fs::OpenOptions;
use std::sync::Arc;

// ── shared drain alias ────────────────────────────────────────────────────────

type BoxDrain = Box<dyn Drain<Ok = (), Err = slog::Never> + Send + Sync>;

// ── Subsystem-filtered drain ──────────────────────────────────────────────────

struct SubsystemFilteredDrain {
    inner: BoxDrain,
    allowed: Vec<String>,
}

impl Drain for SubsystemFilteredDrain {
    type Ok = ();
    type Err = slog::Never;

    fn log(
        &self,
        record: &slog::Record,
        values: &slog::OwnedKVList,
    ) -> Result<Self::Ok, Self::Err> {
        let mut found = String::new();
        {
            let mut ser = SubsystemExtractor(&mut found);
            let _ = values.serialize(record, &mut ser);
            let _ = record.kv().serialize(record, &mut ser);
        }
        if self.allowed.iter().any(|a| a == &found) {
            self.inner.log(record, values)
        } else {
            Ok(())
        }
    }
}

/// Extracts the value of the `"subsystem"` KV key.
struct SubsystemExtractor<'a>(&'a mut String);

impl<'a> slog::Serializer for SubsystemExtractor<'a> {
    fn emit_str(&mut self, key: slog::Key, val: &str) -> slog::Result {
        if key == "subsystem" {
            self.0.clear();
            self.0.push_str(val);
        }
        Ok(())
    }

    fn emit_arguments(&mut self, key: slog::Key, val: &std::fmt::Arguments) -> slog::Result {
        if key == "subsystem" {
            self.0.clear();
            self.0.push_str(&val.to_string());
        }
        Ok(())
    }
}

// ── Logfmt drain ─────────────────────────────────────────────────────────────

struct LogfmtDrain<W: std::io::Write + Send + Sync + 'static> {
    writer: std::sync::Mutex<W>,
}

impl<W: std::io::Write + Send + Sync + 'static> Drain for LogfmtDrain<W> {
    type Ok = ();
    type Err = slog::Never;

    fn log(
        &self,
        record: &slog::Record,
        values: &slog::OwnedKVList,
    ) -> Result<Self::Ok, Self::Err> {
        let mut buf = format!(
            "level={} msg={:?}",
            record.level().as_short_str().to_lowercase(),
            record.msg().to_string(),
        );
        let mut ser = LogfmtSerializer(&mut buf);
        let _ = values.serialize(record, &mut ser);
        let _ = record.kv().serialize(record, &mut ser);
        buf.push('\n');
        let mut w = self.writer.lock().unwrap();
        let _ = w.write_all(buf.as_bytes());
        Ok(())
    }
}

struct LogfmtSerializer<'a>(&'a mut String);

impl<'a> slog::Serializer for LogfmtSerializer<'a> {
    fn emit_arguments(&mut self, key: slog::Key, val: &std::fmt::Arguments) -> slog::Result {
        let v = val.to_string();
        if v.contains(' ') || v.contains('"') {
            self.0.push_str(&format!(" {}={:?}", key, v));
        } else {
            self.0.push_str(&format!(" {}={}", key, v));
        }
        Ok(())
    }
}

// ── Fanout drain ──────────────────────────────────────────────────────────────

struct FanoutDrain {
    drains: Arc<Vec<BoxDrain>>,
}

impl Drain for FanoutDrain {
    type Ok = ();
    type Err = slog::Never;

    fn log(
        &self,
        record: &slog::Record,
        values: &slog::OwnedKVList,
    ) -> Result<Self::Ok, Self::Err> {
        for d in self.drains.iter() {
            let _ = d.log(record, values);
        }
        Ok(())
    }
}

// Our drains are async and internally safe across unwind boundaries.
unsafe impl Sync for FanoutDrain {}
impl std::panic::UnwindSafe for FanoutDrain {}
impl std::panic::RefUnwindSafe for FanoutDrain {}

// ── Sink builder ─────────────────────────────────────────────────────────────

fn with_filter(drain: BoxDrain, allowed: Vec<String>) -> BoxDrain {
    if allowed.is_empty() {
        drain
    } else {
        Box::new(SubsystemFilteredDrain { inner: drain, allowed })
    }
}

fn build_sink(sink: &SinkConfig) -> Option<BoxDrain> {
    let level: slog::Level = sink.level.into();

    macro_rules! make_async_drain {
        ($drain:expr) => {{
            let fused = $drain.fuse();
            let async_drain = Async::new(fused).build().fuse();
            let filtered = slog::LevelFilter::new(async_drain, level).fuse();
            let boxed: BoxDrain = Box::new(filtered);
            with_filter(boxed, sink.subsystems.clone())
        }};
    }

    macro_rules! make_drain_for_writer {
        ($writer:expr) => {
            match sink.format {
                LogFormat::Pretty => {
                    // Files get plaintext even when "pretty" is requested
                    let d = FullFormat::new(PlainDecorator::new($writer)).build();
                    Some(make_async_drain!(d))
                }
                LogFormat::Basic => {
                    let d = FullFormat::new(PlainDecorator::new($writer)).build();
                    Some(make_async_drain!(d))
                }
                LogFormat::Logfmt => {
                    let d = LogfmtDrain { writer: std::sync::Mutex::new($writer) };
                    Some(make_async_drain!(d))
                }
                LogFormat::Json => {
                    let d = slog_json::Json::default($writer);
                    Some(make_async_drain!(d))
                }
            }
        };
    }

    match &sink.sink_type {
        SinkType::Stdout => match sink.format {
            LogFormat::Pretty => {
                let d = FullFormat::new(TermDecorator::new().stdout().build()).build();
                Some(make_async_drain!(d))
            }
            LogFormat::Basic => {
                let d = FullFormat::new(PlainDecorator::new(std::io::stdout())).build();
                Some(make_async_drain!(d))
            }
            LogFormat::Logfmt => {
                let d = LogfmtDrain { writer: std::sync::Mutex::new(std::io::stdout()) };
                Some(make_async_drain!(d))
            }
            LogFormat::Json => {
                let d = slog_json::Json::default(std::io::stdout());
                Some(make_async_drain!(d))
            }
        },
        SinkType::Stderr => match sink.format {
            LogFormat::Pretty => {
                let d = FullFormat::new(TermDecorator::new().stderr().build()).build();
                Some(make_async_drain!(d))
            }
            LogFormat::Basic => {
                let d = FullFormat::new(PlainDecorator::new(std::io::stderr())).build();
                Some(make_async_drain!(d))
            }
            LogFormat::Logfmt => {
                let d = LogfmtDrain { writer: std::sync::Mutex::new(std::io::stderr()) };
                Some(make_async_drain!(d))
            }
            LogFormat::Json => {
                let d = slog_json::Json::default(std::io::stderr());
                Some(make_async_drain!(d))
            }
        },
        SinkType::File { path } => {
            let file = OpenOptions::new().create(true).append(true).open(path).ok()?;
            make_drain_for_writer!(file)
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Build a [`Logger`] from the provided [`LoggingConfig`].
///
/// If no sinks are configured the logger discards all output.
pub fn build_logger(config: &LoggingConfig) -> Logger {
    let drains: Vec<BoxDrain> = config.sink.iter().filter_map(build_sink).collect();

    if drains.is_empty() {
        return Logger::root(slog::Discard, o!());
    }

    let fanned = FanoutDrain { drains: Arc::new(drains) };
    Logger::root(fanned.fuse(), o!())
}

/// Build a test logger: stdout, debug level.
///
/// Respects `NO_COLOR` — uses basic format when set.
pub fn build_test_logger() -> Logger {
    let use_color = std::env::var("NO_COLOR").is_err();
    let async_drain = if use_color {
        let d = FullFormat::new(TermDecorator::new().stdout().build())
            .build()
            .fuse();
        Async::new(d).build().fuse()
    } else {
        let d = FullFormat::new(PlainDecorator::new(std::io::stdout()))
            .build()
            .fuse();
        Async::new(d).build().fuse()
    };
    Logger::root(async_drain, o!("test_context" => "unit_tests"))
}
