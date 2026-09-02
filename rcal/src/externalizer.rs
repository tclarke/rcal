//! Abstract Externalizer / ExternalizerLoader interfaces plus XML implementation.
//!
//! ## Specification references
//! - OMSC-SPC-008 Rev K §9.13–9.15 (CXX-012071–012446)

#[cfg(feature = "compression")]
use crate::calconfig::CompressionType;
use crate::calconfig::{CalConfig, ExternalizerConfig, SerializationFormat};
use crate::uci::{CalError, CalErrorKind, CalMessage, CalResult};
use std::collections::HashMap;
use std::io::{Read, Write};

// ════════════════════════════════════════════════════════════════════════════
// Externalizer
// ════════════════════════════════════════════════════════════════════════════

/// Abstract byte-transform interface for a CAL encoding pipeline.
///
/// An `Externalizer` performs byte-level encoding/decoding (e.g. compression,
/// encryption) and reports its identity. It is intentionally message-type-agnostic;
/// M-specific serialization/deserialization is handled by the free functions
/// [`write_to_bytes`], [`write_to_string`], [`write_to_writer`],
/// [`read_from_bytes`], [`read_from_str`], and [`read_from_reader`].
///
/// Externalizers may be chained via [`next`][Externalizer::next].
/// During encoding, the head's [`encode`][Externalizer::encode] is applied first,
/// then each subsequent node's. During decoding the order is reversed.
///
/// Obtain instances via [`ExternalizerLoader`], [`build_externalizer`], or the
/// [`ExternalizerBuilder`] fluent API.
///
/// # CERT coverage
/// CXX-012071, CXX-012099, CXX-012115, CXX-012131, CXX-012146, CXX-012161,
/// CXX-012176, CXX-012238, CXX-012252, CXX-012266, CXX-012280, CXX-012294,
/// CXX-012688, CXX-012689, CXX-012690, CXX-012691
pub trait Externalizer: Send + Sync {
    /// Apply the local byte transform (default: identity).
    fn encode(&self, bytes: &[u8]) -> CalResult<Vec<u8>> {
        Ok(bytes.to_vec())
    }

    /// Reverse the local byte transform (default: identity).
    fn decode(&self, bytes: &[u8]) -> CalResult<Vec<u8>> {
        Ok(bytes.to_vec())
    }

    /// Optional next externalizer in the chain. Applied after `self` during encode,
    /// before `self` during decode.
    fn next(&self) -> Option<&dyn Externalizer> {
        None
    }

    /// The serialization format reported by this externalizer, if any.
    ///
    /// Leaf externalizers (e.g. [`XmlExternalizer`]) return `Some`; byte-transform
    /// externalizers (e.g. compression) return `None` and delegate to their `next`.
    fn serialization_format(&self) -> Option<SerializationFormat> {
        None
    }

    // ── Version / identity (CXX-012238, 012252, 012266, 012280, 012294) ─────

    /// Returns the CAL API version string (CXX-012238).
    fn get_cal_api_version(&self) -> &str;

    /// Returns the encoding identifier, e.g. `"xml"` (CXX-012252).
    fn get_encoding(&self) -> &str;

    /// Returns the UCI schema version this externalizer targets (CXX-012266).
    fn get_schema_version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    /// Returns the vendor-specific version string (CXX-012280).
    fn get_vendor_version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    /// Returns the vendor name (CXX-012294).
    fn get_vendor(&self) -> &str {
        XML_EXTERNALIZER_VENDOR
    }

    // ── Capability queries (CXX-012688–012691) ───────────────────────────────

    /// Returns `true` if this externalizer supports read operations only (CXX-012688).
    fn message_read_only(&self) -> bool {
        false
    }

    /// Returns `true` if this externalizer supports write operations only (CXX-012689).
    fn message_write_only(&self) -> bool {
        false
    }

    /// Returns `true` if read operations are supported (CXX-012690).
    fn supports_object_read(&self) -> bool {
        true
    }

    /// Returns `true` if write operations are supported (CXX-012691).
    fn supports_object_write(&self) -> bool {
        true
    }
}

// ════════════════════════════════════════════════════════════════════════════
// ExternalizerLoader
// ════════════════════════════════════════════════════════════════════════════

/// Factory for obtaining [`Externalizer`] instances by encoding name.
///
/// Rust equivalent of `uci::base::ExternalizerLoader` (OMSC-SPC-008 §9.14).
/// Drop the returned `Box` to release the loader (no explicit destroy call needed).
///
/// # CERT coverage
/// CXX-012338, CXX-012367, CXX-012381
pub trait ExternalizerLoader: Send + Sync {
    /// Return a boxed [`Externalizer`] for the requested encoding.
    ///
    /// `encoding` — format identifier, e.g. `"xml"`.
    /// Returns [`CalErrorKind::SerializationError`] for unsupported encodings (CXX-012367).
    fn get_externalizer(
        &self,
        encoding: &str,
        schema_version: &str,
        vendor_version: &str,
    ) -> CalResult<Box<dyn Externalizer>>;
}

// ════════════════════════════════════════════════════════════════════════════
// Free functions (CXX-012099, 012115, 012131, 012146, 012161, 012176, 012434)
// ════════════════════════════════════════════════════════════════════════════

/// Return the default [`ExternalizerLoader`] (CXX-012434).
pub fn get_externalizer_loader() -> Box<dyn ExternalizerLoader> {
    Box::new(XmlExternalizerLoader)
}

/// Serialize `msg` through the externalizer chain and return bytes (CXX-012176).
///
/// `root` sets the XML root element name (typically the topic name).
pub fn write_to_bytes<M>(ext: &dyn Externalizer, msg: &M, root: &str) -> CalResult<Vec<u8>>
where
    M: CalMessage + serde::Serialize,
{
    let format = find_format(ext).ok_or_else(|| {
        CalError::new(
            CalErrorKind::SerializationError,
            "no serialization format in externalizer chain",
        )
    })?;
    let raw = serialize_xml(msg, root, format)?;
    encode_chain(ext, &raw)
}

/// Serialize `msg` through the externalizer chain and return a `String` (CXX-012161).
pub fn write_to_string<M>(ext: &dyn Externalizer, msg: &M, root: &str) -> CalResult<String>
where
    M: CalMessage + serde::Serialize,
{
    write_to_bytes(ext, msg, root).and_then(|b| {
        String::from_utf8(b)
            .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))
    })
}

/// Serialize `msg` through the externalizer chain and write to `writer` (CXX-012146).
pub fn write_to_writer<M>(
    ext: &dyn Externalizer,
    msg: &M,
    root: &str,
    writer: &mut dyn Write,
) -> CalResult<()>
where
    M: CalMessage + serde::Serialize,
{
    let bytes = write_to_bytes(ext, msg, root)?;
    writer
        .write_all(&bytes)
        .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))
}

/// Decode `bytes` through the full externalizer chain and deserialize to `M` (CXX-012131).
pub fn read_from_bytes<M>(ext: &dyn Externalizer, bytes: &[u8]) -> CalResult<M>
where
    M: CalMessage + serde::de::DeserializeOwned,
{
    let decoded = decode_chain(ext, bytes)?;
    quick_xml::de::from_reader(decoded.as_slice())
        .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))
}

/// Decode `s` through the full externalizer chain and deserialize to `M` (CXX-012115).
pub fn read_from_str<M>(ext: &dyn Externalizer, s: &str) -> CalResult<M>
where
    M: CalMessage + serde::de::DeserializeOwned,
{
    read_from_bytes(ext, s.as_bytes())
}

/// Read all bytes from `reader`, decode through the chain, and deserialize to `M` (CXX-012099).
pub fn read_from_reader<M>(ext: &dyn Externalizer, reader: &mut dyn Read) -> CalResult<M>
where
    M: CalMessage + serde::de::DeserializeOwned,
{
    let mut buf = Vec::new();
    reader
        .read_to_end(&mut buf)
        .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))?;
    read_from_bytes(ext, &buf)
}

// ── Chain helpers ────────────────────────────────────────────────────────────

fn find_format(ext: &dyn Externalizer) -> Option<SerializationFormat> {
    ext.serialization_format()
        .or_else(|| ext.next().and_then(find_format))
}

fn encode_chain(ext: &dyn Externalizer, bytes: &[u8]) -> CalResult<Vec<u8>> {
    let encoded = ext.encode(bytes)?;
    match ext.next() {
        Some(n) => encode_chain(n, &encoded),
        None => Ok(encoded),
    }
}

fn decode_chain(ext: &dyn Externalizer, bytes: &[u8]) -> CalResult<Vec<u8>> {
    let bytes = match ext.next() {
        Some(n) => decode_chain(n, bytes)?,
        None => bytes.to_vec(),
    };
    ext.decode(&bytes)
}

fn serialize_xml<M: serde::Serialize>(
    msg: &M,
    root: &str,
    format: SerializationFormat,
) -> CalResult<Vec<u8>> {
    match format {
        SerializationFormat::Xml => quick_xml::se::to_string_with_root(root, msg)
            .map(String::into_bytes)
            .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string())),
        SerializationFormat::PrettyXml => {
            let mut buf = String::new();
            let mut ser = quick_xml::se::Serializer::with_root(&mut buf, Some(root))
                .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))?;
            ser.indent(' ', 4);
            serde::Serialize::serialize(msg, ser)
                .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))?;
            Ok(buf.into_bytes())
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Constants
// ════════════════════════════════════════════════════════════════════════════

/// CAL API version string reported by the XML externalizer.
pub const XML_EXTERNALIZER_CAL_API_VERSION: &str = "2.5";
/// Encoding identifier for the XML externalizer.
pub const XML_EXTERNALIZER_ENCODING: &str = "xml";
/// Vendor name for this implementation.
pub const XML_EXTERNALIZER_VENDOR: &str = "rcal";

// ════════════════════════════════════════════════════════════════════════════
// XmlExternalizer
// ════════════════════════════════════════════════════════════════════════════

/// XML-format descriptor [`Externalizer`].
///
/// Acts as the leaf in an externalizer chain. Its `encode`/`decode` are identity
/// transforms; it reports a [`SerializationFormat`] so that [`write_to_bytes`] and
/// related free functions know how to serialize the message type `M`.
///
/// Chained byte-transforms (e.g. `CompressionExternalizer`) wrap it via `next`.
///
/// Construct with [`XmlExternalizer::new`] or via [`ExternalizerBuilder`]:
/// ```text
/// builder("xml").kind("xml").build()
/// ```
pub struct XmlExternalizer {
    format: SerializationFormat,
    /// Optional next byte-transform in the chain.
    pub next: Option<Box<dyn Externalizer>>,
}

impl XmlExternalizer {
    /// Construct with the given format and no chained externalizer.
    pub fn new(format: SerializationFormat) -> Self {
        Self { format, next: None }
    }

    /// Build from an [`ExternalizerBuilder`].
    pub fn from_builder(b: &ExternalizerBuilder) -> CalResult<Box<dyn Externalizer>> {
        Ok(Box::new(xml_from_options(&b.options)))
    }
}

impl Externalizer for XmlExternalizer {
    fn next(&self) -> Option<&dyn Externalizer> {
        self.next.as_deref()
    }

    fn serialization_format(&self) -> Option<SerializationFormat> {
        Some(self.format)
    }

    fn get_cal_api_version(&self) -> &str {
        XML_EXTERNALIZER_CAL_API_VERSION
    }

    fn get_encoding(&self) -> &str {
        XML_EXTERNALIZER_ENCODING
    }
}

fn xml_from_options(options: &HashMap<String, String>) -> XmlExternalizer {
    let pretty = options.get("pretty").is_some_and(|v| v == "true");
    let format = if pretty {
        SerializationFormat::PrettyXml
    } else {
        SerializationFormat::Xml
    };
    XmlExternalizer { format, next: None }
}

// ════════════════════════════════════════════════════════════════════════════
// CompressionExternalizer (feature = "compression")
// ════════════════════════════════════════════════════════════════════════════

/// Byte-transform [`Externalizer`] that compresses/decompresses using `flate2`.
///
/// Requires the `compression` feature. Construct with [`new_gzip_externalizer`]
/// or via [`ExternalizerBuilder`]:
/// ```text
/// builder("xml").kind("xml").chain("gzip").kind("compression").build()
/// ```
/// The `next` field holds the inner externalizer (typically [`XmlExternalizer`]).
#[cfg(feature = "compression")]
pub struct CompressionExternalizer {
    compression_type: CompressionType,
    level: flate2::Compression,
    /// Optional next externalizer in the chain (inner serializer or further transform).
    pub next: Option<Box<dyn Externalizer>>,
}

#[cfg(feature = "compression")]
impl CompressionExternalizer {
    /// Construct a compression externalizer.
    ///
    /// `next` is typically a [`XmlExternalizer`] for message serialization.
    pub fn new(
        next: Option<Box<dyn Externalizer>>,
        compression_type: CompressionType,
        level: flate2::Compression,
    ) -> Self {
        Self {
            compression_type,
            level,
            next,
        }
    }

    /// Build from an [`ExternalizerBuilder`], wrapping `next` as the inner externalizer.
    pub fn from_builder(
        b: &ExternalizerBuilder,
        next: Option<Box<dyn Externalizer>>,
    ) -> CalResult<Box<dyn Externalizer>> {
        Ok(Box::new(compression_from_options(&b.options, next)))
    }
}

#[cfg(feature = "compression")]
impl Externalizer for CompressionExternalizer {
    fn encode(&self, bytes: &[u8]) -> CalResult<Vec<u8>> {
        compress(bytes, self.level, &self.compression_type)
    }

    fn decode(&self, bytes: &[u8]) -> CalResult<Vec<u8>> {
        decompress(bytes, &self.compression_type)
    }

    fn next(&self) -> Option<&dyn Externalizer> {
        self.next.as_deref()
    }

    fn get_cal_api_version(&self) -> &str {
        XML_EXTERNALIZER_CAL_API_VERSION
    }

    fn get_encoding(&self) -> &str {
        self.compression_type.as_str()
    }
}

#[cfg(feature = "compression")]
fn compression_from_options(
    options: &HashMap<String, String>,
    next: Option<Box<dyn Externalizer>>,
) -> CompressionExternalizer {
    let ct = options
        .get("compression_type")
        .and_then(|s| s.parse::<CompressionType>().ok())
        .unwrap_or_default();
    let level = options
        .get("level")
        .and_then(|s| s.parse::<u32>().ok())
        .map(|l| flate2::Compression::new(l.clamp(0, 9)))
        .unwrap_or_default();
    CompressionExternalizer {
        compression_type: ct,
        level,
        next,
    }
}

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

/// Wrap `next` in a gzip [`CompressionExternalizer`] with default compression level.
///
/// Requires the `compression` feature.
#[cfg(feature = "compression")]
pub fn new_gzip_externalizer(next: Box<dyn Externalizer>) -> CompressionExternalizer {
    CompressionExternalizer::new(
        Some(next),
        CompressionType::Gzip,
        flate2::Compression::default(),
    )
}

// ════════════════════════════════════════════════════════════════════════════
// ExternalizerBuilder
// ════════════════════════════════════════════════════════════════════════════

/// Fluent builder for constructing [`Externalizer`] chains.
///
/// # Example
/// ```text
/// let ext = builder("xml")
///     .kind("xml")
///     .chain("gzip")
///     .kind("compression")
///     .option("level", 6)
///     .build();
/// ```
///
/// The chain above produces `CompressionExternalizer { next: XmlExternalizer }`.
/// `chain()` wraps the current builder in a new outer builder; the outermost node
/// is the chain head (applied first during encode).
pub struct ExternalizerBuilder {
    name: String,
    kind: Option<String>,
    options: HashMap<String, String>,
    next: Option<Box<ExternalizerBuilder>>,
}

impl Default for ExternalizerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ExternalizerBuilder {
    /// Create a builder with no name (useful for testing).
    pub fn new() -> Self {
        Self {
            name: String::new(),
            kind: None,
            options: HashMap::new(),
            next: None,
        }
    }

    /// Set the externalizer type (e.g. `"xml"`, `"compression"`).
    pub fn kind(mut self, kind: &str) -> Self {
        self.kind = Some(kind.to_string());
        self
    }

    /// Set a typed option (stored via `ToString`).
    pub fn option<T: ToString>(mut self, key: &str, value: T) -> Self {
        self.options.insert(key.to_string(), value.to_string());
        self
    }

    /// Wrap `self` in a new outer externalizer named `outer_name`.
    ///
    /// Subsequent [`kind`][Self::kind] and [`option`][Self::option] calls configure
    /// the outer node. Call [`build`][Self::build] on the returned builder to
    /// assemble the full chain.
    pub fn chain(self, outer_name: &str) -> Self {
        ExternalizerBuilder {
            name: outer_name.to_string(),
            kind: None,
            options: HashMap::new(),
            next: Some(Box::new(self)),
        }
    }

    /// Build the externalizer chain.
    pub fn build(self) -> CalResult<Box<dyn Externalizer>> {
        let ExternalizerBuilder {
            name,
            kind,
            options,
            next,
        } = self;
        let effective_kind = kind.as_deref().unwrap_or(&name).to_string();
        match next {
            None => match effective_kind.as_str() {
                "xml" => Ok(Box::new(xml_from_options(&options))),
                other => Err(CalError::new(
                    CalErrorKind::SerializationError,
                    format!("unknown leaf externalizer type: '{other}'"),
                )),
            },
            Some(inner_b) => {
                #[cfg(feature = "compression")]
                if effective_kind == "compression" {
                    let inner_ext = inner_b.build()?;
                    return Ok(Box::new(compression_from_options(
                        &options,
                        Some(inner_ext),
                    )));
                }
                drop(inner_b);
                Err(CalError::new(
                    CalErrorKind::SerializationError,
                    format!("unknown chain externalizer type: '{effective_kind}'"),
                ))
            }
        }
    }
}

/// Create a new [`ExternalizerBuilder`] with the given descriptive name.
pub fn builder(name: impl Into<String>) -> ExternalizerBuilder {
    ExternalizerBuilder {
        name: name.into(),
        kind: None,
        options: HashMap::new(),
        next: None,
    }
}

// ════════════════════════════════════════════════════════════════════════════
// build_externalizer — config-driven factory
// ════════════════════════════════════════════════════════════════════════════

/// Build an [`Externalizer`] by name from [`CalConfig`].
///
/// Lookup order:
/// 1. If `name` appears in `config.externalizer`, use that section.
/// 2. Otherwise fall back to built-in defaults:
///    - `"xml"` → [`XmlExternalizer`] (compact)
///    - `"compression"` / `"gzip"` → gzip-wrapped `"xml"` (requires `compression` feature)
pub fn build_externalizer(name: &str, config: &CalConfig) -> CalResult<Box<dyn Externalizer>> {
    match config.externalizer.get(name) {
        Some(ext_cfg) => build_from_config(ext_cfg, config),
        None => build_builtin(name),
    }
}

fn build_from_config(
    ext_cfg: &ExternalizerConfig,
    _config: &CalConfig,
) -> CalResult<Box<dyn Externalizer>> {
    match ext_cfg {
        ExternalizerConfig::Xml { pretty } => {
            let format = if *pretty {
                SerializationFormat::PrettyXml
            } else {
                SerializationFormat::Xml
            };
            Ok(Box::new(XmlExternalizer { format, next: None }))
        }
        #[cfg(feature = "compression")]
        ExternalizerConfig::Compression {
            inner,
            compression_type,
            options,
        } => {
            let inner_ext = build_externalizer(inner, _config)?;
            let level = options
                .get("level")
                .and_then(|v| v.as_integer())
                .map(|l| flate2::Compression::new(l.clamp(0, 9) as u32))
                .unwrap_or_default();
            Ok(Box::new(CompressionExternalizer {
                compression_type: compression_type.clone(),
                level,
                next: Some(inner_ext),
            }))
        }
    }
}

fn build_builtin(name: &str) -> CalResult<Box<dyn Externalizer>> {
    match name {
        "xml" => Ok(Box::new(XmlExternalizer {
            format: SerializationFormat::Xml,
            next: None,
        })),
        #[cfg(feature = "compression")]
        "compression" | "gzip" => {
            let xml = build_builtin("xml")?;
            Ok(Box::new(CompressionExternalizer {
                compression_type: CompressionType::Gzip,
                level: flate2::Compression::default(),
                next: Some(xml),
            }))
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
/// Supports `"xml"` encoding.
/// Returns [`CalErrorKind::SerializationError`] for unrecognised encoding strings.
#[derive(Default)]
pub struct XmlExternalizerLoader;

impl ExternalizerLoader for XmlExternalizerLoader {
    fn get_externalizer(
        &self,
        encoding: &str,
        _schema_version: &str,
        _vendor_version: &str,
    ) -> CalResult<Box<dyn Externalizer>> {
        match encoding {
            XML_EXTERNALIZER_ENCODING => Ok(Box::new(XmlExternalizer {
                format: SerializationFormat::default(),
                next: None,
            })),
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

    const ROOT: &str = "TestMsg";

    #[test]
    fn xml_round_trip_str() {
        let ext = XmlExternalizer::new(SerializationFormat::Xml);
        let msg = TestMsg { value: 42 };
        let s = write_to_string(&ext, &msg, ROOT).unwrap();
        let decoded: TestMsg = read_from_str(&ext, &s).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn xml_round_trip_bytes() {
        let ext = XmlExternalizer::new(SerializationFormat::Xml);
        let msg = TestMsg { value: 7 };
        let bytes = write_to_bytes(&ext, &msg, ROOT).unwrap();
        let decoded: TestMsg = read_from_bytes(&ext, &bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn xml_round_trip_reader_writer() {
        let ext = XmlExternalizer::new(SerializationFormat::Xml);
        let msg = TestMsg { value: 99 };
        let mut buf = Vec::new();
        write_to_writer(&ext, &msg, ROOT, &mut buf).unwrap();
        let decoded: TestMsg = read_from_reader(&ext, &mut buf.as_slice()).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn xml_externalizer_identity() {
        let ext: Box<dyn Externalizer> = Box::new(XmlExternalizer::new(SerializationFormat::Xml));
        assert_eq!(ext.get_encoding(), "xml");
        assert_eq!(ext.get_cal_api_version(), "2.5");
        assert_eq!(ext.get_vendor(), "rcal");
        assert!(!ext.message_read_only());
        assert!(!ext.message_write_only());
        assert!(ext.supports_object_read());
        assert!(ext.supports_object_write());
    }

    #[test]
    fn loader_ok() {
        let loader = XmlExternalizerLoader;
        let ext = loader.get_externalizer("xml", "2.5", "1.0").unwrap();
        assert_eq!(ext.get_encoding(), "xml");
    }

    #[test]
    fn loader_unknown_encoding() {
        let loader = XmlExternalizerLoader;
        let result = loader.get_externalizer("binary", "2.5", "1.0");
        assert_eq!(
            result.err().unwrap().kind(),
            &CalErrorKind::SerializationError
        );
    }

    #[cfg(feature = "compression")]
    #[test]
    fn compression_round_trip() {
        let xml: Box<dyn Externalizer> = Box::new(XmlExternalizer::new(SerializationFormat::Xml));
        let gzip = new_gzip_externalizer(xml);
        let msg = TestMsg { value: 55 };
        let compressed = write_to_bytes(&gzip, &msg, ROOT).unwrap();
        let decoded: TestMsg = read_from_bytes(&gzip, &compressed).unwrap();
        assert_eq!(decoded, msg);
    }

    #[cfg(feature = "compression")]
    #[test]
    fn builder_xml_gzip() {
        let ext = builder("xml")
            .kind("xml")
            .chain("gzip")
            .kind("compression")
            .build()
            .unwrap();
        let msg = TestMsg { value: 99 };
        let compressed = write_to_bytes(ext.as_ref(), &msg, ROOT).unwrap();
        let decoded: TestMsg = read_from_bytes(ext.as_ref(), &compressed).unwrap();
        assert_eq!(decoded, msg);
        assert_eq!(ext.get_encoding(), "gzip");
    }
}
