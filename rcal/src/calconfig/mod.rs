#![allow(dead_code)]
use serde::{Deserialize, Serialize};
use std::fs;
use crate::uci::{CalError, CalImplementationErrorKind, CalResult};
use crate::uci::base::UUID;

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(default)]
struct CalConfig {
    system: System,
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
    let config_str = fs::read_to_string(filename).map_err(|err| {
            CalError::with_impl_source(CalImplementationErrorKind::ConfigError, format!("Can't read config file: {}", filename), err)
        })?;
    parse_config(config_str.as_str())
}

fn parse_config(config_str: &str) -> CalResult<CalConfig> {
    let config = toml::from_str(&config_str).map_err(|err| {
            CalError::with_impl_source(CalImplementationErrorKind::ConfigError, "Can't parse configurtion", err)
    })?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::path::PathBuf;
    use super::*;

    #[test]
    fn test_parse_file() {
        let mut file_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        file_path.push("tests");
        file_path.push("fixtures");
        file_path.push("calconfig_sample.toml");
        parse_config_from_file(&file_path.to_str().unwrap()).unwrap();
    }
}
