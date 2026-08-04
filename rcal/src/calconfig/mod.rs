use serde::Deserialize;
use std::fs;
use crate::uci::{CalError, CalErrorType, CalResult};
use crate::uci::base::UUID;

#[derive(Deserialize, Copy)]
struct CalConfig {
    id: String,
    label: Option<String>,
    uuid: UUID,
    default_transport: Option<String>,
    transport = Transport,
    service = Service,
}

#[derive(Deserialize, Copy)]
struct Transport {
    id: String,
    type: String,
    uri: String,
}

#[derive(Deserialize, Copy)]
struct Service {
    id: String,
    transport: Option<String>
    topic: Topic,
}

#[derive(Deserialize, Copy)]
struct Topic {
    id: String,
    type: Option<String>,
    topic: Option<String>,
}

fn parse_config(filename: &str) -> CalResult<CalConfig> {
    let config_str = fs::read_to_string(filename)?;
    let config = toml::from_str(&config_str)?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse() {
        let test_data_file = include_str!("../../tests/fixtures/calconfig_sample.toml");
        parse_config(test_data_file);
    }
}
