//! Logger construction from [`LoggingConfig`].

use crate::calconfig::{LogFormat, LoggingConfig, SinkConfig, SinkType};
use slog::{Drain, KV, Logger, o};
use slog_async::Async;
use slog_term::{FullFormat, PlainDecorator, TermDecorator};
use std::fs::OpenOptions;

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
        w.write_all(buf.as_bytes()).ok();
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
    drains: Vec<BoxDrain>,
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
        Box::new(SubsystemFilteredDrain {
            inner: drain,
            allowed,
        })
    }
}

fn build_sink(sink: &SinkConfig) -> Option<BoxDrain> {
    let level: slog::Level = sink.level.into();

    macro_rules! make_async_drain {
        ($drain:expr) => {{
            let async_drain = Async::new($drain.fuse()).build().fuse();
            let filtered = slog::LevelFilter::new(async_drain, level).fuse();
            with_filter(Box::new(filtered), sink.subsystems.clone())
        }};
    }

    match &sink.sink_type {
        SinkType::Stdout | SinkType::Stderr => {
            let stderr = matches!(sink.sink_type, SinkType::Stderr);
            Some(match sink.format {
                LogFormat::Pretty => {
                    let dec = if stderr {
                        TermDecorator::new().stderr().build()
                    } else {
                        TermDecorator::new().stdout().build()
                    };
                    make_async_drain!(FullFormat::new(dec).build())
                }
                LogFormat::Basic => {
                    let w: Box<dyn std::io::Write + Send + Sync> = if stderr {
                        Box::new(std::io::stderr())
                    } else {
                        Box::new(std::io::stdout())
                    };
                    make_async_drain!(FullFormat::new(PlainDecorator::new(w)).build())
                }
                LogFormat::Logfmt => {
                    let w: Box<dyn std::io::Write + Send + Sync> = if stderr {
                        Box::new(std::io::stderr())
                    } else {
                        Box::new(std::io::stdout())
                    };
                    make_async_drain!(LogfmtDrain {
                        writer: std::sync::Mutex::new(w)
                    })
                }
                LogFormat::Json => {
                    let w: Box<dyn std::io::Write + Send + Sync> = if stderr {
                        Box::new(std::io::stderr())
                    } else {
                        Box::new(std::io::stdout())
                    };
                    make_async_drain!(slog_json::Json::default(w))
                }
            })
        }
        SinkType::File { path } => {
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| eprintln!("rcal logging: failed to open log file '{}': {}", path, e))
                .ok()?;
            Some(match sink.format {
                LogFormat::Pretty | LogFormat::Basic => {
                    make_async_drain!(FullFormat::new(PlainDecorator::new(file)).build())
                }
                LogFormat::Logfmt => {
                    make_async_drain!(LogfmtDrain {
                        writer: std::sync::Mutex::new(file)
                    })
                }
                LogFormat::Json => make_async_drain!(slog_json::Json::default(file)),
            })
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Build a [`Logger`] from the provided [`LoggingConfig`].
///
/// If no sinks are configured the logger discards all output.
pub fn build_logger(config: &LoggingConfig) -> Logger {
    let mut drains: Vec<BoxDrain> = config.sink.iter().filter_map(build_sink).collect();

    if drains.is_empty() {
        let dec = TermDecorator::new().stdout().build();
        let async_drain = Async::new(FullFormat::new(dec).build().fuse())
            .build()
            .fuse();
        let filtered = slog::LevelFilter::new(async_drain, slog::Level::Info).fuse();
        drains.push(Box::new(filtered));
    }

    let fanned = FanoutDrain { drains };
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
