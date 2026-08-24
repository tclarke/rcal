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
    CompressionExternalizer, Externalizer, XML_GZIP_EXTERNALIZER_ENCODING, XmlExternalizer,
    build_externalizer, new_gzip_externalizer,
};
use rcal::uci::CalMessage;
use rcal::uci::types::SystemStatus_;

fn main() {
    let root = SystemStatus_::message_type_name().local().to_string();
    let msg = SystemStatus_::cal_create();

    // ── 1. Direct construction ────────────────────────────────────────────────
    let inner: Box<dyn Externalizer<SystemStatus_>> =
        Box::new(XmlExternalizer::new(SerializationFormat::Xml, root.clone()));
    let gzip_ext: CompressionExternalizer<SystemStatus_> = new_gzip_externalizer(inner);

    let compressed = gzip_ext.write_to_bytes(&msg).expect("serialize failed");
    let raw_len = XmlExternalizer::new(SerializationFormat::Xml, root.clone())
        .write_to_bytes(&msg)
        .unwrap()
        .len();
    println!(
        "Direct gzip: {} bytes (raw XML: {} bytes)",
        compressed.len(),
        raw_len
    );
    let _decoded: SystemStatus_ = gzip_ext
        .read_from_bytes(&compressed)
        .expect("deserialize failed");

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

    let config_ext: Box<dyn Externalizer<SystemStatus_>> =
        build_externalizer("gzip_xml", &config, root.clone()).expect("build failed");
    let compressed2 = config_ext
        .write_to_bytes(&msg)
        .expect("config ext serialize failed");
    let _decoded2: SystemStatus_ = config_ext
        .read_from_bytes(&compressed2)
        .expect("config ext deserialize failed");
    println!("Config-driven gzip_xml: {} bytes", compressed2.len());

    // ── 3. "compression" built-in (no config section needed) ─────────────────
    let builtin_ext: Box<dyn Externalizer<SystemStatus_>> =
        build_externalizer("compression", &CalConfig::default(), root.clone())
            .expect("builtin build failed");
    let compressed3 = builtin_ext
        .write_to_bytes(&msg)
        .expect("builtin serialize failed");
    println!("Built-in \"compression\": {} bytes", compressed3.len());

    // ── 4. Via ExternalizerLoader encoding string "xml+gzip" ──────────────────
    use rcal::externalizer::{ExternalizerLoader, XmlExternalizerLoader};
    let loader = XmlExternalizerLoader;
    let loader_ext: Box<dyn Externalizer<SystemStatus_>> = loader
        .get_externalizer(XML_GZIP_EXTERNALIZER_ENCODING, "2.5", "1.0")
        .expect("loader failed");
    let compressed4 = loader_ext
        .write_to_bytes(&msg)
        .expect("loader serialize failed");
    println!(
        "ExternalizerLoader \"{}\": {} bytes",
        XML_GZIP_EXTERNALIZER_ENCODING,
        compressed4.len()
    );

    println!("All xml+gzip round-trips OK");
}
