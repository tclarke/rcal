#![allow(dead_code)]
use crate::uci::base::UUID;
use crate::uci::{CalError, CalImplementationErrorKind, CalResult};
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::sync::Mutex;

lazy_static! {
    pub static ref CAL_CONFIG: Mutex<CalConfig> = Mutex::new(CalConfig::default());
}

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
    pub fn get_service(&self, name: &String) -> Option<&Service> {
        self.service.iter().find(|item| item.id == *name)
    }

    pub fn get_transport(&self, name: &String) -> Option<&Transport> {
        self.transport.iter().find(|item| item.id == *name)
    }

    pub fn get_transport_for_service(&self, name: &String) -> Option<&Transport> {
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
    #[serde(rename = "type")]
    /// The factory type
    pub type_: UUIDFactoryType,

    /// Namespace for "namespace' generators."
    pub namespace: Option<UUID>,

    /// MAC address to timebased. If not specified, the
    /// default interface's mac address will be used.
    pub node: Option<mac_address::MacAddress>,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
pub struct Transport {
    pub id: String,
    #[serde(rename = "type")]
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
    file_path.into_string().unwrap()
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
