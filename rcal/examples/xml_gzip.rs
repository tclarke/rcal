//! xml_gzip example — round-trip a UCI message through XML + gzip compression.
//!
//! Shows both direct API usage and config-driven externalizer construction via
//! [`build_externalizer`].
//!
//! Run:
//!   cargo run --example xml_gzip --features compression

use std::collections::HashMap;

use rcal::calconfig::SerializationFormat;
use rcal::calconfig::{CalConfig, CompressionType, ExternalizerConfig};
use rcal::externalizer::{
    XmlExternalizer, build_externalizer, new_gzip_externalizer, read_from_bytes, write_to_bytes,
};
use rcal::uci::CalMessage;
use rcal::uci::types::SystemStatus_;

fn main() {
    let root = SystemStatus_::message_type_name().local().to_string();
    let msg = SystemStatus_::cal_create();

    // ── 1. Direct construction ────────────────────────────────────────────────
    let xml_ext = XmlExternalizer::new(SerializationFormat::Xml);
    let gzip_ext = new_gzip_externalizer(Box::new(XmlExternalizer::new(SerializationFormat::Xml)));

    let compressed = write_to_bytes(&gzip_ext, &msg, &root).expect("serialize failed");
    let raw_len = write_to_bytes(&xml_ext, &msg, &root).unwrap().len();
    println!(
        "Direct gzip: {} bytes (raw XML: {} bytes)",
        compressed.len(),
        raw_len
    );
    let _decoded: SystemStatus_ =
        read_from_bytes(&gzip_ext, &compressed).expect("deserialize failed");

    // ── 2. Config-driven via named externalizer section ───────────────────────
    //
    // Equivalent TOML:
    //   [externalizer.gzip_xml]
    //   type = "compression"
    //   inner = "xml"
    //   compression_type = "gzip"
    //   [externalizer.gzip_xml.options]
    //   level = 6
    let mut config = CalConfig::default();
    config.externalizer.insert(
        "gzip_xml".to_string(),
        ExternalizerConfig::Compression {
            inner: "xml".to_string(),
            compression_type: CompressionType::Gzip,
            options: {
                let mut m = HashMap::new();
                m.insert("level".to_string(), toml::Value::Integer(6));
                m
            },
        },
    );

    let config_ext = build_externalizer("gzip_xml", &config).expect("build failed");
    let compressed2 =
        write_to_bytes(config_ext.as_ref(), &msg, &root).expect("config ext serialize failed");
    let _decoded2: SystemStatus_ =
        read_from_bytes::<SystemStatus_>(config_ext.as_ref(), &compressed2)
            .expect("config ext deserialize failed");
    println!("Config-driven gzip_xml: {} bytes", compressed2.len());

    // ── 3. "compression" built-in (no config section needed) ─────────────────
    let builtin_ext =
        build_externalizer("compression", &CalConfig::default()).expect("builtin build failed");
    let compressed3 =
        write_to_bytes(builtin_ext.as_ref(), &msg, &root).expect("builtin serialize failed");
    println!("Built-in \"compression\": {} bytes", compressed3.len());

    println!("All xml+gzip round-trips OK");
}
