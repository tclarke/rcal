#![allow(dead_code)]
use crate::uci::base::UUID;
use crate::uci::{CalError, CalImplementationErrorKind, CalResult};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::fmt;
use std::fs;

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
pub struct CalConfig {
    pub system: System,
    #[serde(rename = "uuid-factory")]
    pub uuidfactory: UUIDFactory,
    pub transport: Vec<Transport>,
    pub service: Vec<Service>,
    /// Named externalizer configurations.
    ///
    /// Use the short built-in names `"xml"` or (with feature `compression`) `"compression"`
    /// without a section for defaults.  Add a `[externalizer.<name>]` section to override
    /// options or to define a named chain.
    pub externalizer: HashMap<String, ExternalizerConfig>,
    /// Extension sections for service-specific configuration.
    ///
    /// Any top-level TOML key not consumed by the standard fields is collected here.
    /// Services retrieve their section via [`CalConfig::get_extension`].
    #[serde(flatten)]
    pub extensions: HashMap<String, toml::Value>,
}

impl CalConfig {
    /// Deserialize a service-specific extension section from `CalConfig`.
    ///
    /// Looks up the top-level TOML key `key` in [`CalConfig::extensions`] and
    /// deserializes it into `T`.  Returns a `ConfigError` if the key is absent or
    /// if the value cannot be deserialized into `T`.
    pub fn get_extension<T: DeserializeOwned>(&self, key: &str) -> CalResult<T> {
        let val = self.extensions.get(key).ok_or_else(|| {
            CalError::new_impl(
                CalImplementationErrorKind::ConfigError,
                format!("Missing config section '[{key}]'"),
            )
        })?;
        val.clone().try_into().map_err(|err| {
            CalError::with_impl_source(
                CalImplementationErrorKind::ConfigError,
                format!("Failed to parse config section '[{key}]'"),
                err,
            )
        })
    }

    pub fn get_service(&self, name: &str) -> Option<&Service> {
        self.service.iter().find(|item| item.id == name)
    }

    pub fn get_transport(&self, name: &str) -> Option<&Transport> {
        self.transport.iter().find(|item| item.id == name)
    }

    pub fn get_transport_for_service(&self, name: &str) -> Option<&Transport> {
        let service_conf = self.get_service(name)?;
        let transport_name = service_conf
            .transport
            .as_ref()
            .or(self.system.default_transport.as_ref())?;
        self.get_transport(transport_name)
    }
}

impl fmt::Display for CalConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", toml::to_string(self).unwrap())
    }
}

/// Log level for a sink or global default.
#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

impl From<LogLevel> for slog::Level {
    fn from(l: LogLevel) -> Self {
        match l {
            LogLevel::Trace => slog::Level::Trace,
            LogLevel::Debug => slog::Level::Debug,
            LogLevel::Info => slog::Level::Info,
            LogLevel::Warn => slog::Level::Warning,
            LogLevel::Error => slog::Level::Error,
        }
    }
}

/// Log output format.
#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Colored terminal output.
    #[default]
    Pretty,
    /// Plaintext, no ANSI colors.
    Basic,
    /// key=value pairs (logfmt).
    Logfmt,
    /// JSON objects.
    Json,
}

/// Log sink destination.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase", tag = "type")]
pub enum SinkType {
    #[default]
    Stdout,
    Stderr,
    File {
        path: String,
    },
}

/// Configuration for one log sink.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(default)]
pub struct SinkConfig {
    #[serde(flatten)]
    pub sink_type: SinkType,
    pub level: LogLevel,
    pub format: LogFormat,
    /// Subsystem names to include; empty = accept all.
    pub subsystems: Vec<String>,
}

impl Default for SinkConfig {
    fn default() -> Self {
        Self {
            sink_type: SinkType::Stdout,
            level: LogLevel::Warn,
            format: LogFormat::Pretty,
            subsystems: Vec::new(),
        }
    }
}

/// Logging configuration stored under `[system.logging]`.
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
#[serde(default)]
pub struct LoggingConfig {
    pub default_level: LogLevel,
    pub sink: Vec<SinkConfig>,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
pub struct System {
    pub id: String,
    pub label: Option<String>,
    pub uuid: UUID,
    pub default_transport: Option<String>,
    pub logging: LoggingConfig,
    /// Optional MissionID UUID populated in message headers.
    pub mission_id: Option<UUID>,
    /// Message mode populated in message headers (default: "LIVE").
    pub mode: Option<String>,
    /// Classification populated in message security info (default: "U").
    pub classification: Option<String>,
    /// OwnerProducer values populated in message security info (default: ["USA"]).
    pub owner_producer: Vec<String>,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
pub enum UUIDFactoryType {
    #[default]
    Random,
    TimeBased,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
pub struct UUIDFactory {
    #[serde(rename = "type")]
    /// The factory type
    pub type_: UUIDFactoryType,

    /// Namespace for "namespace' generators."
    pub namespace: Option<UUID>,

    /// MAC address to timebased. If not specified, the
    /// default interface's mac address will be used.
    pub node: Option<mac_address::MacAddress>,
}

/// Serialization format used by externalizers.
///
/// Transport-level externalizer selection is configured via
/// [`Transport::externalizer`] and [`CalConfig::externalizer`].
#[derive(Deserialize, Serialize, Default, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SerializationFormat {
    /// Whitespace-compressed XML (default).
    #[default]
    Xml,
    /// Indented, human-readable XML.
    PrettyXml,
    /// TOML serialization.
    Toml,
}

/// Configuration for a named externalizer.
///
/// Reference the name in [`Transport::externalizer`].
/// Built-in names (`"xml"`, and with feature `compression`: `"compression"`) work
/// without a section entry; add a section only to override defaults or build a chain.
///
/// # Examples (TOML)
/// ```toml
/// [externalizer.pretty]
/// type = "xml"
/// pretty = true
///
/// [externalizer.gzip_xml]
/// type = "compression"
/// inner = "xml"          # which externalizer to wrap (default: "xml")
/// compression_type = "gzip"   # gzip | deflate | zlib  (default: "gzip")
/// [externalizer.gzip_xml.options]
/// level = 6              # 0–9, default per algorithm
/// ```
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ExternalizerConfig {
    /// XML serialization.
    Xml {
        /// Use indented, human-readable XML (default: `false`).
        #[serde(default)]
        pretty: bool,
    },
    /// TOML serialization.
    Toml,
    /// Byte-level compression chain wrapping an inner externalizer.
    ///
    /// Requires the `compression` feature.
    #[cfg(feature = "compression")]
    Compression {
        /// Name of the inner externalizer to wrap (default: `"xml"`).
        #[serde(default = "default_inner_externalizer")]
        inner: String,
        /// Compression algorithm (default: `"gzip"`).
        #[serde(default)]
        compression_type: CompressionType,
        /// Algorithm-specific options (e.g. `level = 6`).
        #[serde(default)]
        options: HashMap<String, toml::Value>,
    },
}

#[cfg(feature = "compression")]
impl CompressionType {
    /// Returns the string identifier for this compression type.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Gzip => "gzip",
            Self::Deflate => "deflate",
            Self::Zlib => "zlib",
        }
    }
}

#[cfg(feature = "compression")]
impl std::str::FromStr for CompressionType {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "gzip" => Ok(Self::Gzip),
            "deflate" => Ok(Self::Deflate),
            "zlib" => Ok(Self::Zlib),
            _ => Err(()),
        }
    }
}

#[cfg(feature = "compression")]
fn default_inner_externalizer() -> String {
    "xml".to_string()
}

/// Compression algorithm for [`ExternalizerConfig::Compression`].
///
/// All variants are enabled by `flate2`'s default features.
#[cfg(feature = "compression")]
#[derive(Deserialize, Serialize, Default, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CompressionType {
    #[default]
    Gzip,
    Deflate,
    Zlib,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
pub struct Transport {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub uri: String,
    /// Name of the externalizer to use for this transport (default: `"xml"`).
    ///
    /// Use a built-in name (`"xml"`, `"compression"`) or reference a
    /// `[externalizer.<name>]` section in `CalConfig`.
    pub externalizer: Option<String>,
}

/// A name-to-UUID mapping used for components and capabilities (CAL-005203).
#[derive(Deserialize, Serialize, Default, Debug, Clone)]
pub struct NamedUuid {
    /// Logical name for the component or capability.
    pub name: String,
    /// UUID assigned to this component or capability.
    pub uuid: UUID,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
pub struct Service {
    pub id: String,
    pub transport: Option<String>,
    pub topic: Vec<Topic>,
    /// Optional service UUID used to populate the ServiceID field in message headers.
    pub uuid: Option<UUID>,
    /// Optional subsystem UUID this service belongs to (CAL-005203, CERT CXX-011170).
    pub subsystem_uuid: Option<UUID>,
    /// Component UUIDs accessible via this service (CAL-005203, CERT CXX-011171).
    pub components: Vec<NamedUuid>,
    /// Capability UUIDs accessible via this service (CAL-005203, CERT CXX-011172).
    pub capabilities: Vec<NamedUuid>,
    /// Duration string for periodic status message interval (e.g. "1s", "500ms").
    pub status_delay: Option<String>,
    /// When true, the service registers a ServiceStatusDataRequest reader and responds automatically.
    pub service_status_data_request_enable: bool,
}

impl Service {
    /// Returns the UUID of the named component, or `None` if not configured.
    pub fn get_component_uuid(&self, name: &str) -> Option<UUID> {
        self.components
            .iter()
            .find(|c| c.name == name)
            .map(|c| c.uuid)
    }

    /// Returns the UUID of the named capability, or `None` if not configured.
    pub fn get_capability_uuid(&self, name: &str) -> Option<UUID> {
        self.capabilities
            .iter()
            .find(|c| c.name == name)
            .map(|c| c.uuid)
    }
}

/// Reliability policy in TOML config — mirrors `cal::Reliability` but serde-friendly.
#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReliabilityConfig {
    #[default]
    BestEffort,
    Reliable,
}

/// Per-topic Quality of Service settings (CERT CAL-005210).
///
/// All fields are optional; absent fields leave the corresponding `TopicQos` field at its
/// default value.
#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
pub struct TopicQosConfig {
    /// Reliability policy (`best_effort` or `reliable`).
    pub reliability: Option<ReliabilityConfig>,
    /// Minimum inter-message gap in milliseconds (CAL-005431).
    pub time_based_filter_ms: Option<u64>,
    /// Maximum message age in milliseconds before eviction from the reader buffer (CAL-005437).
    pub expiration_ms: Option<u64>,
    /// Maximum writer-side buffered messages (CAL-005444, CAL-005445).
    pub writer_buffer: Option<usize>,
    /// Maximum reader-side buffered messages (CAL-015746, CAL-016079).
    pub reader_buffer: Option<usize>,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
pub struct Topic {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: Option<String>,
    pub topic: Option<String>,
    /// Optional per-topic QoS defaults (CAL-005210).
    pub qos: Option<TopicQosConfig>,
}

pub fn parse_config_from_file(filename: &str) -> CalResult<CalConfig> {
    let config_str = fs::read_to_string(filename).map_err(|err| {
        CalError::with_impl_source(
            CalImplementationErrorKind::ConfigError,
            format!("Can't read config file: {}", filename),
            err,
        )
    })?;
    parse_config(config_str.as_str())
}

pub fn parse_config(config_str: &str) -> CalResult<CalConfig> {
    let config = toml::from_str(config_str).map_err(|err| {
        CalError::with_impl_source(
            CalImplementationErrorKind::ConfigError,
            "Can't parse configuration",
            err,
        )
    })?;
    Ok(config)
}

#[cfg(test)]
use std::env;
#[cfg(test)]
use std::path::PathBuf;
/// A test utility that returns a full path to a configutation file [`filename`]
/// Only usable in unit tests. This will panic! if there's a problem converting the
/// path into a string.
#[cfg(test)]
pub fn get_test_config_path(filename: &str) -> String {
    let mut file_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    file_path.push("tests");
    file_path.push("fixtures");
    file_path.push(filename);
    file_path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_file() {
        parse_config_from_file(get_test_config_path("calconfig_sample.toml").as_str()).unwrap();
    }

    #[test]
    fn test_uuid_factory() {
        parse_config("[system]\nid=\"foo\"\n[uuid-factory]\ntype=\"Random\"\n").unwrap();
        parse_config("[system]\nid=\"foo\"\n[uuid-factory]\ntype=\"TimeBased\"\n").unwrap();
        parse_config("[system]\nid=\"foo\"\n[uuid-factory]\ntype=\"TimeBased\"\nnode=\"00:11:22:33:44:55\"\n").unwrap();
    }

    #[test]
    fn test_topic_qos_config_parses() {
        let toml = r#"
[system]
id = "test"

[[service]]
id = "Svc"

[[service.topic]]
id = "SystemStatus"

[service.topic.qos]
reliability = "best_effort"
time_based_filter_ms = 100
expiration_ms = 5000
reader_buffer = 10
writer_buffer = 5
"#;
        let cfg = parse_config(toml).unwrap();
        let svc = cfg.get_service("Svc").unwrap();
        let topic = svc.topic.iter().find(|t| t.id == "SystemStatus").unwrap();
        let qos = topic.qos.as_ref().unwrap();
        assert_eq!(qos.reliability, Some(ReliabilityConfig::BestEffort));
        assert_eq!(qos.time_based_filter_ms, Some(100));
        assert_eq!(qos.expiration_ms, Some(5000));
        assert_eq!(qos.reader_buffer, Some(10));
        assert_eq!(qos.writer_buffer, Some(5));
    }

    #[test]
    fn test_topic_without_qos_parses() {
        let toml = "[system]\nid=\"foo\"\n[[service]]\nid=\"Svc\"\n[[service.topic]]\nid=\"T\"\n";
        let cfg = parse_config(toml).unwrap();
        let topic = &cfg.get_service("Svc").unwrap().topic[0];
        assert!(topic.qos.is_none());
    }

    #[test]
    fn test_get_extension_roundtrip() {
        #[derive(Deserialize, PartialEq, Debug)]
        struct MyExt {
            value: u32,
            label: String,
        }
        let toml = r#"
[system]
id = "test"
[my_service]
value = 42
label = "hello"
"#;
        let cfg = parse_config(toml).unwrap();
        let ext: MyExt = cfg.get_extension("my_service").unwrap();
        assert_eq!(ext.value, 42);
        assert_eq!(ext.label, "hello");
    }

    #[test]
    fn test_get_extension_missing_returns_error() {
        let cfg = parse_config("[system]\nid=\"foo\"\n").unwrap();
        assert!(cfg.get_extension::<toml::Value>("nonexistent").is_err());
    }
}
