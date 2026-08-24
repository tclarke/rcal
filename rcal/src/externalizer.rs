//! Abstract Externalizer / ExternalizerLoader interfaces plus XML implementation.
//!
//! ## Specification references
//! - OMSC-SPC-008 Rev K §9.13–9.15 (CXX-012071–012446)

#![warn(missing_docs)]

#[cfg(feature = "compression")]
use crate::calconfig::CompressionType;
use crate::calconfig::{CalConfig, ExternalizerConfig, SerializationFormat};
use crate::uci::{CalError, CalErrorKind, CalMessage, CalResult};
use std::io::{Read, Write};

// ════════════════════════════════════════════════════════════════════════════
// Externalizer<M>
// ════════════════════════════════════════════════════════════════════════════

/// Abstract serialization/deserialization interface for a single CAL Message type.
///
/// Rust equivalent of `uci::base::Externalizer` (OMSC-SPC-008 §9.13).
/// The trait is generic over `M: CalMessage` so that `dyn Externalizer<M>` is
/// a valid trait object for any specific message type `M`.
///
/// Obtain instances from an [`ExternalizerLoader`] or construct them directly
/// (e.g. [`XmlExternalizer`]).
///
/// # CERT coverage
/// CXX-012071, CXX-012099, CXX-012115, CXX-012131, CXX-012146, CXX-012161,
/// CXX-012176, CXX-012238, CXX-012252, CXX-012266, CXX-012280, CXX-012294,
/// CXX-012688, CXX-012689, CXX-012690, CXX-012691
pub trait Externalizer<M: CalMessage>: Send + Sync {
    // ── Read (CXX-012099, 012115, 012131) ───────────────────────────────────

    /// Deserialize a message by reading from `reader` (CXX-012099).
    fn read_from_reader(&self, reader: &mut dyn Read) -> CalResult<M>;

    /// Deserialize a message from a UTF-8 string (CXX-012115).
    fn read_from_str(&self, s: &str) -> CalResult<M>;

    /// Deserialize a message from a byte slice (CXX-012131).
    fn read_from_bytes(&self, bytes: &[u8]) -> CalResult<M>;

    // ── Write (CXX-012146, 012161, 012176) ──────────────────────────────────

    /// Serialize `msg` and write the bytes to `writer` (CXX-012146).
    fn write_to_writer(&self, msg: &M, writer: &mut dyn Write) -> CalResult<()>;

    /// Serialize `msg` to a [`String`] (CXX-012161).
    fn write_to_string(&self, msg: &M) -> CalResult<String>;

    /// Serialize `msg` to a byte vector (CXX-012176).
    fn write_to_bytes(&self, msg: &M) -> CalResult<Vec<u8>>;

    // ── Version / identity (CXX-012238, 012252, 012266, 012280, 012294) ─────

    /// Returns the CAL API version string (CXX-012238).
    fn get_cal_api_version(&self) -> &str;

    /// Returns the encoding identifier, e.g. `"xml"` (CXX-012252).
    fn get_encoding(&self) -> &str;

    /// Returns the UCI schema version this externalizer targets (CXX-012266).
    fn get_schema_version(&self) -> &str;

    /// Returns the vendor-specific version string (CXX-012280).
    fn get_vendor_version(&self) -> &str;

    /// Returns the vendor name (CXX-012294).
    fn get_vendor(&self) -> &str;

    // ── Capability queries (CXX-012688–012691) ───────────────────────────────

    /// Returns `true` if this externalizer supports read operations only (CXX-012688).
    fn message_read_only(&self) -> bool;

    /// Returns `true` if this externalizer supports write operations only (CXX-012689).
    fn message_write_only(&self) -> bool;

    /// Returns `true` if read operations are supported (CXX-012690).
    fn supports_object_read(&self) -> bool;

    /// Returns `true` if write operations are supported (CXX-012691).
    fn supports_object_write(&self) -> bool;
}

// ════════════════════════════════════════════════════════════════════════════
// ExternalizerLoader<M>
// ════════════════════════════════════════════════════════════════════════════

/// Factory for obtaining [`Externalizer`] instances by encoding name.
///
/// Rust equivalent of `uci::base::ExternalizerLoader` (OMSC-SPC-008 §9.14).
/// `destroy_externalizer` (CXX-012381) is unnecessary in Rust — `Box` drop
/// handles deallocation.
///
/// # CERT coverage
/// CXX-012338, CXX-012367, CXX-012381
pub trait ExternalizerLoader<M: CalMessage>: Send + Sync {
    /// Return a boxed [`Externalizer`] for the requested encoding.
    ///
    /// `encoding` — format identifier, e.g. `"xml"`.
    /// `schema_version` — target UCI schema version.
    /// `vendor_version` — implementor-defined version string.
    ///
    /// Returns [`CalErrorKind::SerializationError`] if the encoding is not
    /// supported by this loader (CXX-012367).
    fn get_externalizer(
        &self,
        encoding: &str,
        schema_version: &str,
        vendor_version: &str,
    ) -> CalResult<Box<dyn Externalizer<M>>>;
}

// ════════════════════════════════════════════════════════════════════════════
// Global free functions (CXX-012434, CXX-012446)
// ════════════════════════════════════════════════════════════════════════════

/// Return the default [`ExternalizerLoader`] for message type `M` (CXX-012434).
///
/// The default loader supports `"xml"` encoding via [`XmlExternalizerLoader`].
/// To release the loader, simply drop the returned `Box` — no explicit destroy
/// call is needed (contrast with CXX-012446, which is a C++ memory-management
/// artefact with no Rust equivalent).
pub fn get_externalizer_loader<M: CalMessage + serde::Serialize + serde::de::DeserializeOwned>()
-> Box<dyn ExternalizerLoader<M>> {
    Box::new(XmlExternalizerLoader)
}

// ════════════════════════════════════════════════════════════════════════════
// XmlExternalizer
// ════════════════════════════════════════════════════════════════════════════

/// CAL API version string reported by the XML externalizer.
pub const XML_EXTERNALIZER_CAL_API_VERSION: &str = "2.5";
/// Encoding identifier for the XML externalizer.
pub const XML_EXTERNALIZER_ENCODING: &str = "xml";
/// Vendor name for this implementation.
pub const XML_EXTERNALIZER_VENDOR: &str = "rcal";

/// XML-based [`Externalizer`] backed by `quick_xml` / `serde`.
///
/// Implements [`Externalizer<M>`] for all `M: CalMessage + serde::Serialize +
/// serde::de::DeserializeOwned`. The `root` field sets the XML root element
/// name written by [`write_to_string`][XmlExternalizer::write_to_string];
/// callers such as [`ZmqWriter`][crate::asb::zmq::ZmqWriter] pass the topic
/// name here.
pub struct XmlExternalizer {
    format: SerializationFormat,
    root: String,
}

impl XmlExternalizer {
    /// Construct a new `XmlExternalizer` with the given format and root element name.
    pub fn new(format: SerializationFormat, root: impl Into<String>) -> Self {
        Self {
            format,
            root: root.into(),
        }
    }
}

impl<M> Externalizer<M> for XmlExternalizer
where
    M: CalMessage + serde::Serialize + serde::de::DeserializeOwned,
{
    fn read_from_reader(&self, reader: &mut dyn Read) -> CalResult<M> {
        let mut buf = Vec::new();
        reader
            .read_to_end(&mut buf)
            .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))?;
        self.read_from_bytes(&buf)
    }

    fn read_from_str(&self, s: &str) -> CalResult<M> {
        quick_xml::de::from_str(s)
            .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))
    }

    fn read_from_bytes(&self, bytes: &[u8]) -> CalResult<M> {
        let s = std::str::from_utf8(bytes)
            .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))?;
        self.read_from_str(s)
    }

    fn write_to_writer(&self, msg: &M, writer: &mut dyn Write) -> CalResult<()> {
        let s = self.write_to_string(msg)?;
        writer
            .write_all(s.as_bytes())
            .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))
    }

    fn write_to_string(&self, msg: &M) -> CalResult<String> {
        match self.format {
            SerializationFormat::Xml => quick_xml::se::to_string_with_root(&self.root, msg)
                .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string())),
            SerializationFormat::PrettyXml => {
                let mut buf = String::new();
                let mut ser = quick_xml::se::Serializer::with_root(&mut buf, Some(&self.root))
                    .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))?;
                ser.indent(' ', 4);
                serde::Serialize::serialize(msg, ser)
                    .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))?;
                Ok(buf)
            }
        }
    }

    fn write_to_bytes(&self, msg: &M) -> CalResult<Vec<u8>> {
        self.write_to_string(msg).map(String::into_bytes)
    }

    fn get_cal_api_version(&self) -> &str {
        XML_EXTERNALIZER_CAL_API_VERSION
    }

    fn get_encoding(&self) -> &str {
        XML_EXTERNALIZER_ENCODING
    }

    fn get_schema_version(&self) -> &str {
        // ponytail: returns crate version; replace with schema version constant when defined
        env!("CARGO_PKG_VERSION")
    }

    fn get_vendor_version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn get_vendor(&self) -> &str {
        XML_EXTERNALIZER_VENDOR
    }

    fn message_read_only(&self) -> bool {
        false
    }

    fn message_write_only(&self) -> bool {
        false
    }

    fn supports_object_read(&self) -> bool {
        true
    }

    fn supports_object_write(&self) -> bool {
        true
    }
}

// ════════════════════════════════════════════════════════════════════════════
// ChainExternalizer<M>
// ════════════════════════════════════════════════════════════════════════════

/// Byte transform function used by [`ChainExternalizer`].
type ByteTransform = Box<dyn Fn(&[u8]) -> CalResult<Vec<u8>> + Send + Sync>;

/// Byte-level pipeline wrapper around an [`Externalizer`].
///
/// Applies `encode` to serialized bytes before writing, and `decode` to raw
/// bytes before deserializing. Use this to add compression, encryption, or any
/// other byte transform without changing the inner externalizer.
///
/// **Implicit chaining** is also available: pass a wrapping `impl Write`
/// (e.g. a gzip encoder) to [`write_to_writer`][Externalizer::write_to_writer]
/// and a wrapping `impl Read` to [`read_from_reader`][Externalizer::read_from_reader]
/// directly — no `ChainExternalizer` needed.
///
/// # Example (conceptual)
/// ```text
/// let chain = ChainExternalizer::new(
///     Box::new(XmlExternalizer::new(SerializationFormat::Xml, "Msg")),
///     |bytes| gzip_compress(bytes),
///     |bytes| gzip_decompress(bytes),
/// );
/// // write path: XML → gzip bytes
/// // read path:  gzip bytes → XML → M
/// ```
pub struct ChainExternalizer<M: CalMessage> {
    inner: Box<dyn Externalizer<M>>,
    encode: ByteTransform,
    decode: ByteTransform,
}

impl<M: CalMessage> ChainExternalizer<M> {
    /// Construct a chained externalizer.
    ///
    /// `encode` transforms bytes *after* serialization (e.g. compress/encrypt).
    /// `decode` transforms bytes *before* deserialization (e.g. decompress/decrypt).
    pub fn new(
        inner: Box<dyn Externalizer<M>>,
        encode: impl Fn(&[u8]) -> CalResult<Vec<u8>> + Send + Sync + 'static,
        decode: impl Fn(&[u8]) -> CalResult<Vec<u8>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner,
            encode: Box::new(encode),
            decode: Box::new(decode),
        }
    }
}

impl<M: CalMessage> Externalizer<M> for ChainExternalizer<M> {
    fn read_from_reader(&self, reader: &mut dyn Read) -> CalResult<M> {
        let mut buf = Vec::new();
        reader
            .read_to_end(&mut buf)
            .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))?;
        self.read_from_bytes(&buf)
    }

    fn read_from_str(&self, s: &str) -> CalResult<M> {
        self.read_from_bytes(s.as_bytes())
    }

    fn read_from_bytes(&self, bytes: &[u8]) -> CalResult<M> {
        let decoded = (self.decode)(bytes)?;
        self.inner.read_from_bytes(&decoded)
    }

    fn write_to_writer(&self, msg: &M, writer: &mut dyn Write) -> CalResult<()> {
        let encoded = self.write_to_bytes(msg)?;
        writer
            .write_all(&encoded)
            .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))
    }

    fn write_to_string(&self, msg: &M) -> CalResult<String> {
        let bytes = self.write_to_bytes(msg)?;
        String::from_utf8(bytes)
            .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))
    }

    fn write_to_bytes(&self, msg: &M) -> CalResult<Vec<u8>> {
        let raw = self.inner.write_to_bytes(msg)?;
        (self.encode)(&raw)
    }

    fn get_cal_api_version(&self) -> &str {
        self.inner.get_cal_api_version()
    }

    fn get_encoding(&self) -> &str {
        self.inner.get_encoding()
    }

    fn get_schema_version(&self) -> &str {
        self.inner.get_schema_version()
    }

    fn get_vendor_version(&self) -> &str {
        self.inner.get_vendor_version()
    }

    fn get_vendor(&self) -> &str {
        self.inner.get_vendor()
    }

    fn message_read_only(&self) -> bool {
        self.inner.message_read_only()
    }

    fn message_write_only(&self) -> bool {
        self.inner.message_write_only()
    }

    fn supports_object_read(&self) -> bool {
        self.inner.supports_object_read()
    }

    fn supports_object_write(&self) -> bool {
        self.inner.supports_object_write()
    }
}

// ════════════════════════════════════════════════════════════════════════════
// CompressionExternalizer (feature = "compression")
// ════════════════════════════════════════════════════════════════════════════

/// A [`ChainExternalizer`] pre-configured with a compression codec.
///
/// Construct with [`new_gzip_externalizer`] or via [`build_externalizer`] with
/// a `[externalizer.<name>]` section of `type = "compression"` in config.
///
/// Requires the `compression` feature.
#[cfg(feature = "compression")]
pub type CompressionExternalizer<M> = ChainExternalizer<M>;

/// Encoding identifier for XML + gzip (used by [`XmlExternalizerLoader`]).
#[cfg(feature = "compression")]
pub const XML_GZIP_EXTERNALIZER_ENCODING: &str = "xml+gzip";

#[cfg(feature = "compression")]
fn compress(bytes: &[u8], level: flate2::Compression, ct: &CompressionType) -> CalResult<Vec<u8>> {
    use flate2::write::{DeflateEncoder, GzEncoder, ZlibEncoder};
    macro_rules! enc {
        ($T:ident) => {{
            let mut e = $T::new(Vec::new(), level);
            e.write_all(bytes)
                .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))?;
            e.finish()
                .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))
        }};
    }
    match ct {
        CompressionType::Gzip => enc!(GzEncoder),
        CompressionType::Deflate => enc!(DeflateEncoder),
        CompressionType::Zlib => enc!(ZlibEncoder),
    }
}

#[cfg(feature = "compression")]
fn decompress(bytes: &[u8], ct: &CompressionType) -> CalResult<Vec<u8>> {
    use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
    macro_rules! dec {
        ($T:ident) => {{
            let mut d = $T::new(bytes);
            let mut out = Vec::new();
            d.read_to_end(&mut out)
                .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))?;
            Ok(out)
        }};
    }
    match ct {
        CompressionType::Gzip => dec!(GzDecoder),
        CompressionType::Deflate => dec!(DeflateDecoder),
        CompressionType::Zlib => dec!(ZlibDecoder),
    }
}

#[cfg(feature = "compression")]
fn level_from_options(
    options: &std::collections::HashMap<String, toml::Value>,
) -> flate2::Compression {
    options
        .get("level")
        .and_then(|v| v.as_integer())
        .map(|l| flate2::Compression::new(l.clamp(0, 9) as u32))
        .unwrap_or_default()
}

#[cfg(feature = "compression")]
fn make_codec(ct: CompressionType, level: flate2::Compression) -> (ByteTransform, ByteTransform) {
    let ct2 = ct.clone();
    let encoder: ByteTransform = Box::new(move |b| compress(b, level, &ct));
    let decoder: ByteTransform = Box::new(move |b| decompress(b, &ct2));
    (encoder, decoder)
}

/// Wrap `inner` in a gzip [`ChainExternalizer`] with default compression level.
///
/// For custom compression type or level, use [`build_externalizer`] with a
/// `[externalizer.<name>]` config section.
///
/// Requires the `compression` feature.
#[cfg(feature = "compression")]
pub fn new_gzip_externalizer<M: CalMessage>(
    inner: Box<dyn Externalizer<M>>,
) -> CompressionExternalizer<M> {
    let (enc, dec) = make_codec(CompressionType::Gzip, flate2::Compression::default());
    ChainExternalizer::new(inner, enc, dec)
}

// ════════════════════════════════════════════════════════════════════════════
// build_externalizer — config-driven factory
// ════════════════════════════════════════════════════════════════════════════

/// Build an [`Externalizer`] by name from [`CalConfig`].
///
/// Lookup order:
/// 1. If `name` appears in `config.externalizer`, use that section's settings.
/// 2. Otherwise fall back to built-in defaults:
///    - `"xml"` → [`XmlExternalizer`] (compact)
///    - `"compression"` → gzip-wrapped `"xml"` (requires `compression` feature)
///
/// The `root` argument sets the XML root element name (typically the topic name).
pub fn build_externalizer<M>(
    name: &str,
    config: &CalConfig,
    root: impl Into<String>,
) -> CalResult<Box<dyn Externalizer<M>>>
where
    M: CalMessage + serde::Serialize + serde::de::DeserializeOwned,
{
    let root = root.into();
    match config.externalizer.get(name) {
        Some(ext_cfg) => build_from_config(ext_cfg, config, root),
        None => build_builtin(name, config, root),
    }
}

fn build_from_config<M>(
    ext_cfg: &ExternalizerConfig,
    _config: &CalConfig,
    root: String,
) -> CalResult<Box<dyn Externalizer<M>>>
where
    M: CalMessage + serde::Serialize + serde::de::DeserializeOwned,
{
    match ext_cfg {
        ExternalizerConfig::Xml { pretty } => {
            let format = if *pretty {
                SerializationFormat::PrettyXml
            } else {
                SerializationFormat::Xml
            };
            Ok(Box::new(XmlExternalizer::new(format, root)))
        }
        #[cfg(feature = "compression")]
        ExternalizerConfig::Compression {
            inner,
            compression_type,
            options,
        } => {
            let level = level_from_options(options);
            let inner_ext = build_externalizer::<M>(inner, _config, root)?;
            let (enc, dec) = make_codec(compression_type.clone(), level);
            Ok(Box::new(ChainExternalizer::new(inner_ext, enc, dec)))
        }
    }
}

fn build_builtin<M>(
    name: &str,
    _config: &CalConfig,
    root: String,
) -> CalResult<Box<dyn Externalizer<M>>>
where
    M: CalMessage + serde::Serialize + serde::de::DeserializeOwned,
{
    match name {
        "xml" => Ok(Box::new(XmlExternalizer::new(
            SerializationFormat::Xml,
            root,
        ))),
        #[cfg(feature = "compression")]
        "compression" => {
            let inner: Box<dyn Externalizer<M>> = build_builtin("xml", _config, root)?;
            Ok(Box::new(new_gzip_externalizer(inner)))
        }
        other => Err(CalError::new(
            CalErrorKind::SerializationError,
            format!("unknown externalizer: '{other}'"),
        )),
    }
}

// ════════════════════════════════════════════════════════════════════════════
// XmlExternalizerLoader
// ════════════════════════════════════════════════════════════════════════════

/// [`ExternalizerLoader`] that produces [`XmlExternalizer`] instances.
///
/// Supports `"xml"` encoding (and `"xml+gzip"` with the `compression` feature).
/// Returns [`CalErrorKind::SerializationError`] for unrecognised encoding strings.
#[derive(Default)]
pub struct XmlExternalizerLoader;

impl<M> ExternalizerLoader<M> for XmlExternalizerLoader
where
    M: CalMessage + serde::Serialize + serde::de::DeserializeOwned,
{
    fn get_externalizer(
        &self,
        encoding: &str,
        _schema_version: &str,
        _vendor_version: &str,
    ) -> CalResult<Box<dyn Externalizer<M>>> {
        match encoding {
            XML_EXTERNALIZER_ENCODING => Ok(Box::new(XmlExternalizer::new(
                SerializationFormat::default(),
                M::message_type_name().local().to_string(),
            ))),
            #[cfg(feature = "compression")]
            XML_GZIP_EXTERNALIZER_ENCODING => Ok(Box::new(new_gzip_externalizer(Box::new(
                XmlExternalizer::new(
                    SerializationFormat::default(),
                    M::message_type_name().local().to_string(),
                ),
            )))),
            other => Err(CalError::new(
                CalErrorKind::SerializationError,
                format!("unsupported externalizer encoding: '{other}'"),
            )),
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    /// Minimal CalMessage stub for externalizer round-trip tests.
    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
    struct TestMsg {
        value: i32,
    }

    impl CalMessage for TestMsg {
        fn message_type_name() -> crate::QName {
            crate::QName::new(None, "TestMsg")
        }
        fn cal_create() -> Self {
            TestMsg { value: 0 }
        }
    }

    #[test]
    fn xml_externalizer_round_trip_str() {
        let ext = XmlExternalizer::new(SerializationFormat::Xml, "TestMsg");
        let msg = TestMsg { value: 42 };
        let s = ext.write_to_string(&msg).unwrap();
        let decoded: TestMsg = ext.read_from_str(&s).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn xml_externalizer_round_trip_bytes() {
        let ext = XmlExternalizer::new(SerializationFormat::Xml, "TestMsg");
        let msg = TestMsg { value: 7 };
        let bytes = ext.write_to_bytes(&msg).unwrap();
        let decoded: TestMsg = ext.read_from_bytes(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn xml_externalizer_round_trip_reader_writer() {
        let ext = XmlExternalizer::new(SerializationFormat::Xml, "TestMsg");
        let msg = TestMsg { value: 99 };
        let mut buf = Vec::new();
        ext.write_to_writer(&msg, &mut buf).unwrap();
        let decoded: TestMsg = ext.read_from_reader(&mut buf.as_slice()).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn xml_externalizer_identity() {
        let ext: Box<dyn Externalizer<TestMsg>> =
            Box::new(XmlExternalizer::new(SerializationFormat::Xml, "TestMsg"));
        assert_eq!(ext.get_encoding(), "xml");
        assert_eq!(ext.get_cal_api_version(), "2.5");
        assert_eq!(ext.get_vendor(), "rcal");
        assert!(!ext.message_read_only());
        assert!(!ext.message_write_only());
        assert!(ext.supports_object_read());
        assert!(ext.supports_object_write());
    }

    #[test]
    fn xml_externalizer_loader_ok() {
        let loader: XmlExternalizerLoader = Default::default();
        let ext: Box<dyn Externalizer<TestMsg>> =
            loader.get_externalizer("xml", "2.5", "1.0").unwrap();
        assert_eq!(ext.get_encoding(), "xml");
    }

    #[test]
    fn chain_externalizer_round_trip() {
        // trivial rot-0 "transform" (identity) to verify the chain plumbing
        let inner: Box<dyn Externalizer<TestMsg>> =
            Box::new(XmlExternalizer::new(SerializationFormat::Xml, "TestMsg"));
        let chain = ChainExternalizer::new(
            inner,
            |b| Ok(b.iter().map(|x| x.wrapping_add(1)).collect()),
            |b| Ok(b.iter().map(|x| x.wrapping_sub(1)).collect()),
        );
        let msg = TestMsg { value: 55 };
        let bytes = chain.write_to_bytes(&msg).unwrap();
        let decoded: TestMsg = chain.read_from_bytes(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn xml_externalizer_loader_unknown_encoding() {
        let loader: XmlExternalizerLoader = Default::default();
        let result: CalResult<Box<dyn Externalizer<TestMsg>>> =
            loader.get_externalizer("binary", "2.5", "1.0");
        let err = result.err().expect("expected error");
        assert_eq!(err.kind(), &CalErrorKind::SerializationError);
    }
}
