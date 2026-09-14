//! ASB dump utility: subscribes to all topics listed for the "asb_dump"
//! service in CALConfig.toml and prints each received message to stdout.
//!
//! Configuration (CALConfig.toml): same layout as `asb_view`.
//!
//! Config path: `RCAL_CALCONFIG_PATH` env var, or `./CALConfig.toml`.
//!
//! Usage: asb_dump [FILE]
//!   Without FILE: print cargo-tree style hierarchy to stdout.
//!   With FILE: write RcalMessageRecord XML records to FILE; print message-count
//!              statistics to stdout (at most once per 5 s, only when counts changed).

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use rcal::QName;
use rcal::asb::get_asb_config_location;
use rcal::cal::{MessageListener, TopicQos, get_cal};
use rcal::calconfig::{SerializationFormat, parse_config_from_file};
use rcal::externalizer::{PrettyExternalizer, XmlExternalizer, write_to_bytes};
use rcal::logging::build_logger;
use rcal::uci::{CalMessage, CalResult};

// ── Generic message wrapper ───────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
struct AnyMsg(toml::Value);

impl CalMessage for AnyMsg {
    fn message_type_name() -> QName {
        QName::new(None, "any")
    }
    fn cal_create() -> Self {
        AnyMsg(toml::Value::Table(Default::default()))
    }
}

// ── Statistics (file mode) ────────────────────────────────────────────────────

struct Stats {
    counts: HashMap<String, usize>,
    total: usize,
    dirty: bool,
    last_printed: Instant,
}

impl Stats {
    fn new() -> Self {
        Self {
            counts: HashMap::new(),
            total: 0,
            dirty: false,
            last_printed: Instant::now() - Duration::from_secs(10),
        }
    }

    fn record(&mut self, topic: &str) {
        *self.counts.entry(topic.to_string()).or_insert(0) += 1;
        self.total += 1;
        self.dirty = true;
    }

    fn maybe_print(&mut self) -> bool {
        if !self.dirty || self.last_printed.elapsed() < Duration::from_secs(5) {
            return false;
        }
        self.print_now();
        true
    }

    fn print_now(&mut self) {
        let mut topics: Vec<(&str, usize)> =
            self.counts.iter().map(|(k, &v)| (k.as_str(), v)).collect();
        topics.sort_by_key(|(k, _)| *k);
        println!("--- {} total ---", self.total);
        for (topic, count) in &topics {
            println!("  {topic}: {count}");
        }
        self.dirty = false;
        self.last_printed = Instant::now();
    }
}

// ── Listener ──────────────────────────────────────────────────────────────────

struct Printer {
    topic: String,
    pretty_ext: Arc<PrettyExternalizer>,
    xml_ext: Arc<XmlExternalizer>,
    stdout: Arc<Mutex<()>>,
    file: Option<Arc<Mutex<BufWriter<File>>>>,
    stats: Option<Arc<Mutex<Stats>>>,
    logger: slog::Logger,
}

#[rcal_macros::rcal_trace]
impl MessageListener<AnyMsg> for Printer {
    fn on_message(&self, msg: &Arc<AnyMsg>) {
        slog::trace!(self.logger, "message received"; "topic" => &self.topic);

        if let Some(ref f) = self.file {
            // File mode: write XML record to file, update stats for stdout.
            match write_to_bytes(self.xml_ext.as_ref(), msg.as_ref(), &self.topic) {
                Ok(bytes) => {
                    slog::debug!(self.logger, "serialized message"; "topic" => &self.topic, "bytes" => bytes.len());
                    let xml = String::from_utf8_lossy(&bytes);
                    let xml = xml.trim_end_matches('\n');
                    let wall = Utc::now().to_rfc3339();
                    let line = format!(
                        "<RcalMessageRecord><WallTime>{wall}</WallTime><Topic>{}</Topic><Message>{xml}</Message></RcalMessageRecord>\n",
                        self.topic
                    );
                    if let Ok(mut w) = f.lock() {
                        let _ = w.write_all(line.as_bytes());
                    }
                    if let Some(ref s) = self.stats {
                        if let Ok(mut st) = s.lock() {
                            st.record(&self.topic);
                        }
                    }
                }
                Err(e) => {
                    slog::error!(self.logger, "serialize error"; "topic" => &self.topic, "error" => %e);
                    eprintln!("serialize error on topic '{}': {e}", self.topic);
                }
            }
        } else {
            // Stream mode: pretty-print to stdout.
            match write_to_bytes(self.pretty_ext.as_ref(), msg.as_ref(), &self.topic) {
                Ok(bytes) => {
                    slog::debug!(self.logger, "serialized message"; "topic" => &self.topic, "bytes" => bytes.len());
                    let _guard = self.stdout.lock().unwrap();
                    print!("{}", String::from_utf8_lossy(&bytes));
                    println!();
                }
                Err(e) => {
                    slog::error!(self.logger, "serialize error"; "topic" => &self.topic, "error" => %e);
                    eprintln!("serialize error on topic '{}': {e}", self.topic);
                }
            }
        }
    }
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> CalResult<()> {
    let args: Vec<String> = std::env::args().collect();
    let output_file: Option<Arc<Mutex<BufWriter<File>>>> = args.get(1).map(|path| {
        let f = File::create(path).unwrap_or_else(|e| {
            eprintln!("asb_dump: cannot open output file '{}': {e}", path);
            std::process::exit(1);
        });
        Arc::new(Mutex::new(BufWriter::new(f)))
    });
    let stats: Option<Arc<Mutex<Stats>>> = output_file
        .as_ref()
        .map(|_| Arc::new(Mutex::new(Stats::new())));

    let config_path = get_asb_config_location(None)?;
    let config = Arc::new(parse_config_from_file(&config_path)?);

    let logger = build_logger(&config.system.logging);
    slog::info!(logger, "asb_dump starting"; "config" => &config_path);

    let service = config
        .get_service("asb_dump")
        .ok_or_else(|| {
            rcal::uci::CalError::new(
                rcal::uci::CalErrorKind::InitializationFailure,
                "no [[service]] with id = \"asb_dump\" in config",
            )
        })?
        .clone();

    let tconfig = config
        .get_transport_for_service("asb_dump")
        .ok_or_else(|| {
            slog::warn!(
                logger,
                "no transport configured for asb_dump; check default_transport"
            );
            rcal::uci::CalError::new(
                rcal::uci::CalErrorKind::InitializationFailure,
                "no transport configured for service \"asb_dump\"",
            )
        })?
        .clone();

    slog::debug!(logger, "using transport"; "id" => &tconfig.id);

    let mut bus = get_cal(
        "asb_dump",
        Some(tconfig.id.clone()),
        Arc::clone(&config),
        logger.clone(),
    )
    .await?;

    let pretty_ext = Arc::new(PrettyExternalizer::new());
    let xml_ext = Arc::new(XmlExternalizer::new(SerializationFormat::Xml));
    let stdout_lock = Arc::new(Mutex::new(()));

    let mut readers: Vec<Box<dyn rcal::cal::AbstractReader<AnyMsg>>> = Vec::new();

    for topic in &service.topic {
        slog::debug!(logger, "subscribing to topic"; "id" => &topic.id);
        let mut reader = bus.create_reader::<AnyMsg>(&topic.id, TopicQos::default())?;
        let printer = Arc::new(Printer {
            topic: topic.id.clone(),
            pretty_ext: Arc::clone(&pretty_ext),
            xml_ext: Arc::clone(&xml_ext),
            stdout: Arc::clone(&stdout_lock),
            file: output_file.as_ref().map(Arc::clone),
            stats: stats.as_ref().map(Arc::clone),
            logger: logger.new(slog::o!("topic" => topic.id.clone())),
        });
        reader.add_listener(printer)?;
        readers.push(reader);
    }

    if readers.is_empty() {
        slog::warn!(
            logger,
            "no topics configured for asb_dump; nothing to listen to"
        );
    }

    slog::info!(logger, "listening"; "topics" => readers.len());
    eprintln!(
        "asb_dump: listening on {} topic(s){}. Press Ctrl-C to stop.",
        readers.len(),
        args.get(1)
            .map(|p| format!(", writing to '{p}'"))
            .unwrap_or_default(),
    );

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    loop {
        tokio::select! {
            _ = &mut ctrl_c => break,
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                if let Some(ref s) = stats {
                    if let Ok(mut st) = s.lock() {
                        st.maybe_print();
                    }
                }
            }
        }
    }

    if let Some(ref s) = stats {
        if let Ok(mut st) = s.lock() {
            if st.dirty {
                st.print_now();
            }
        }
    }
    if let Some(f) = output_file {
        if let Ok(mut w) = f.lock() {
            let _ = w.flush();
        }
    }

    Ok(())
}
