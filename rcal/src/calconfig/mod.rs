#![allow(dead_code)]
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use slog::{info, error};
use std::fmt;
use std::fs;
use std::sync::Mutex;
use crate::uci::{CalError, CalImplementationErrorKind, CalResult};
use crate::uci::base::UUID;


lazy_static! {
    pub static ref CAL_CONFIG: Mutex<CalConfig> = Mutex::new(CalConfig::default());
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
pub struct CalConfig {
    pub system: System,
    #[serde(rename="uuid-factory")]
    pub uuidfactory: UUIDFactory,
    pub transport: Vec<Transport>,
    pub service: Vec<Service>,
}

impl fmt::Display for CalConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", toml::to_string(self).unwrap())
    }
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
pub struct System {
    pub id: String,
    pub label: Option<String>,
    pub uuid: UUID,
    pub default_transport: Option<String>,
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
    #[serde(rename="type")]
    /// The factory type
    pub type_: UUIDFactoryType,

    /// MAC address to timebased. If not specified, the
    /// default interface's mac address will be used.
    pub node: Option<mac_address::MacAddress>,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
pub struct Transport {
    pub id: String,
    #[serde(rename="type")]
    pub type_: String,
    pub uri: String,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
pub struct Service {
    pub id: String,
    pub transport: Option<String>,
    pub topic: Vec<Topic>,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
pub struct Topic {
    pub id: String,
    #[serde(rename="type")]
    pub type_: Option<String>,
    pub topic: Option<String>,
}

fn parse_config_from_file(filename: &str, logger: slog::Logger) -> CalResult<CalConfig> {
    info!(logger, "Parsing config file {}", filename);
    let config_str = fs::read_to_string(filename).map_err(|err| {
            CalError::with_impl_source(CalImplementationErrorKind::ConfigError, format!("Can't read config file: {}", filename), err)
        })?;
    parse_config(config_str.as_str(), logger)
}

fn parse_config(config_str: &str, logger: slog::Logger) -> CalResult<CalConfig> {
    let config = toml::from_str(config_str).map_err(|err| {
            error!(logger, "{}", err);
            CalError::with_impl_source(CalImplementationErrorKind::ConfigError, "Can't parse configuration", err)
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
    file_path.into_string().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use procedural_macros::init_test_logger;

    #[test]
    fn test_parse_file() {
        let logger = init_test_logger!();
        parse_config_from_file(get_test_config_path("calconfig_sample.toml").as_str(), logger).unwrap();
    }

    #[test]
    fn test_uuid_factory() {
        let logger = init_test_logger!();
        parse_config("[system]\nid=\"foo\"\n[uuid-factory]\ntype=\"Random\"\n", logger.clone()).unwrap();
        parse_config("[system]\nid=\"foo\"\n[uuid-factory]\ntype=\"TimeBased\"\n", logger.clone()).unwrap();
        parse_config("[system]\nid=\"foo\"\n[uuid-factory]\ntype=\"TimeBased\"\nnode=\"00:11:22:33:44:55\"\n", logger.clone()).unwrap();
    }
}
