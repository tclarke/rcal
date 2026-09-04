//! ASB dump utility: subscribes to all topics listed for the "asb_dump"
//! service in CALConfig.toml and prints each received message to stdout
//! in TOML format.
//!
//! Configuration (CALConfig.toml):
//! ```toml
//! [system]
//! id = "MySystem"
//! uuid = "..."
//! default_transport = "T"
//!
//! [[transport]]
//! id = "T"
//! type = "zmq"
//! uri = "tcp://127.0.0.1:5555"
//!
//! [[service]]
//! id = "asb_dump"
//!
//! [[service.topic]]
//! id = "my_topic"
//! ```
//!
//! Config path: `RCAL_CONFIG` env var, or `./CALConfig.toml`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rcal::QName;
use rcal::asb::get_asb_config_location;
use rcal::asb::zmq::ZmqAsb;
use rcal::cal::{AbstractCalExt, MessageListener, TopicQos};
use rcal::calconfig::{SerializationFormat, parse_config_from_file};
use rcal::externalizer::{TomlExternalizer, write_to_bytes};
use rcal::uci::{CalMessage, CalResult};

// ── Generic message wrapper ───────────────────────────────────────────────────
//
// AnyMsg wraps toml::Value so we can use the typed CAL reader API without
// knowing the concrete message schema at compile time.  The XML externalizer
// deserializes the wire bytes into toml::Value via serde; the TomlExternalizer
// serializes it back out.

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

// ── Listener ──────────────────────────────────────────────────────────────────

struct TomlPrinter {
    topic: String,
    ext: Arc<TomlExternalizer>,
    stdout: Arc<Mutex<()>>,
}

impl MessageListener<AnyMsg> for TomlPrinter {
    fn on_message(&self, msg: &Arc<AnyMsg>) {
        let _guard = self.stdout.lock().unwrap();
        match write_to_bytes(self.ext.as_ref(), msg.as_ref(), &self.topic) {
            Ok(bytes) => {
                println!("# topic: {}", self.topic);
                print!("{}", String::from_utf8_lossy(&bytes));
                println!();
            }
            Err(e) => eprintln!("serialize error on topic '{}': {e}", self.topic),
        }
    }
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> CalResult<()> {
    let config_path = get_asb_config_location(None)?;
    let config = Arc::new(parse_config_from_file(&config_path)?);

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
            rcal::uci::CalError::new(
                rcal::uci::CalErrorKind::InitializationFailure,
                "no transport configured for service \"asb_dump\"",
            )
        })?
        .clone();

    let logger = slog::Logger::root(slog::Discard, slog::o!());
    let mut bus = ZmqAsb::new(
        "asb_dump",
        &tconfig.id,
        logger,
        Arc::clone(&config),
        &tconfig,
    )
    .await?;

    let toml_ext = Arc::new(TomlExternalizer::new(SerializationFormat::Toml));
    let stdout_lock = Arc::new(Mutex::new(()));

    let mut readers: Vec<Box<dyn rcal::cal::AbstractReader<AnyMsg>>> = Vec::new();

    for topic in &service.topic {
        let mut reader = <ZmqAsb as AbstractCalExt<AnyMsg>>::create_reader(
            &mut bus,
            &topic.id,
            TopicQos::default(),
        )?;
        let printer = Arc::new(TomlPrinter {
            topic: topic.id.clone(),
            ext: Arc::clone(&toml_ext),
            stdout: Arc::clone(&stdout_lock),
        });
        reader.add_listener(printer)?;
        readers.push(reader);
    }

    eprintln!(
        "asb_dump: listening on {} topic(s). Press Ctrl-C to stop.",
        readers.len()
    );

    // Block until interrupted; listener callbacks print to stdout.
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
