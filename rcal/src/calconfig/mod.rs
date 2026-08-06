#![allow(dead_code)]
use serde::{Deserialize, Serialize};
use std::fs;
use slog_scope::{info, error};
use crate::uci::{CalError, CalImplementationErrorKind, CalResult};
use crate::uci::base::UUID;

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
struct CalConfig {
    system: System,
    #[serde(rename="uuid-factory")]
    uuidfactory: UUIDFactory,
    transport: Vec<Transport>,
    service: Vec<Service>,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
struct System {
    id: String,
    label: Option<String>,
    uuid: UUID,
    default_transport: Option<String>,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
enum UUIDFactoryType {
    #[default]
    Random,
    TimeBased,
    Namespace,

}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
struct UUIDFactory {
    #[serde(rename="type")]
    /// The factory type
    type_: UUIDFactoryType,

    /// Namespace for "namespace' generators."
    namespace: Option<UUID>,

    /// MAC address to timebased. If not specified, the
    /// default interface's mac address will be used.
    node: Option<mac_address::MacAddress>,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
struct Transport {
    id: String,
    #[serde(rename="type")]
    type_: String,
    uri: String,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
struct Service {
    id: String,
    transport: Option<String>,
    topic: Vec<Topic>,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
struct Topic {
    id: String,
    #[serde(rename="type")]
    type_: Option<String>,
    topic: Option<String>,
}

fn parse_config_from_file(filename: &str) -> CalResult<CalConfig> {
    info!("Parsing config file {}", filename);
    let config_str = fs::read_to_string(filename).map_err(|err| {
            CalError::with_impl_source(CalImplementationErrorKind::ConfigError, format!("Can't read config file: {}", filename), err)
        })?;
    parse_config(config_str.as_str())
}

fn parse_config(config_str: &str) -> CalResult<CalConfig> {
    let config = toml::from_str(&config_str).map_err(|err| {
            error!("{}", err);
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
    use slog::{Drain, Logger, o};
    use slog_term::{FullFormat, TermDecorator};
    use std::sync::Mutex;

    fn create_test_logger() -> Logger {
        // Decorator that targets stdout for Cargo's test capture framework
        let decorator = TermDecorator::new().stdout().build();

        // Mutex makes the drain safe to share across concurrent test threads
        let drain = FullFormat::new(decorator).build().fuse();
        let drain = Mutex::new(drain).fuse();

        Logger::root(drain, o!("test_context" => "unit_tests"))
    }

    #[test]
    fn test_parse_file() {
        parse_config_from_file(get_test_config_path("calconfig_sample.toml").as_str()).unwrap();
    }

    #[test]
    fn test_uuid_factory() {
        let log = create_test_logger();
        let _guard = slog_scope::set_global_logger(log);

        parse_config("[system]\nid=\"foo\"\n[uuid-factory]\ntype=\"Random\"\n").unwrap();
        parse_config("[system]\nid=\"foo\"\n[uuid-factory]\ntype=\"Namespace\"\n").unwrap();
        parse_config("[system]\nid=\"foo\"\n[uuid-factory]\ntype=\"Namespace\"\nnamespace=\"5a8595ba-29fa-4e04-8276-ef6e5c768bdb\"\n").unwrap();
        parse_config("[system]\nid=\"foo\"\n[uuid-factory]\ntype=\"TimeBased\"\n").unwrap();
        parse_config("[system]\nid=\"foo\"\n[uuid-factory]\ntype=\"TimeBased\"\nnode=\"00:11:22:33:44:55\"\n").unwrap();
    }
}
