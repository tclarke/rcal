#![allow(dead_code)]
use crate::uci::base::UUID;
use crate::uci::{CalError, CalImplementationErrorKind, CalResult};
use serde::{Deserialize, Serialize};
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
}

impl CalConfig {
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

/// Serialization format used for CAL Messages on a transport.
#[derive(Deserialize, Serialize, Default, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SerializationFormat {
    /// Whitespace-compressed XML (default).
    #[default]
    Xml,
    /// Indented, human-readable XML.
    PrettyXml,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
pub struct Transport {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub uri: String,
    /// Serialization format for CAL Messages on this transport (default: `xml`).
    pub format: SerializationFormat,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
pub struct Service {
    pub id: String,
    pub transport: Option<String>,
    pub topic: Vec<Topic>,
    /// Optional service UUID used to populate the ServiceID field in message headers.
    pub uuid: Option<UUID>,
    /// Duration string for periodic status message interval (e.g. "1s", "500ms").
    pub status_delay: Option<String>,
    /// When true, the service registers a ServiceStatusDataRequest reader and responds automatically.
    pub service_status_data_request_enable: bool,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
pub struct Topic {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: Option<String>,
    pub topic: Option<String>,
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
}
