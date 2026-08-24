//! XMLSchema (`xs:`) built-in type mappings.
//!
//! Maps W3C XML Schema 1.0 primitive types to Rust types as required by
//! OMSC-SPC-001 Rev L and OMSC-SPC-008 Rev K.
//!
//! ## Type mapping rationale
//!
//! | XSD type           | Rust alias          | Spec requirement               |
//! |--------------------|---------------------|--------------------------------|
//! | `xs:boolean`       | `bool`                  | OMSC-SPC-008 Table 9.1-1   |
//! | `xs:long`          | `i64`                   | OMSC-SPC-008 Table 9.1-1   |
//! | `xs:int`           | `i32`                   | OMSC-SPC-008 Table 9.1-1   |
//! | `xs:short`         | `i16`                   | OMSC-SPC-008 Table 9.1-1   |
//! | `xs:byte`          | `i8`                    | OMSC-SPC-008 Table 9.1-1   |
//! | `xs:unsignedLong`  | `u64`                   | OMSC-SPC-008 Table 9.1-1   |
//! | `xs:unsignedInt`   | `u32`                   | OMSC-SPC-008 Table 9.1-1   |
//! | `xs:unsignedShort` | `u16`                   | OMSC-SPC-008 Table 9.1-1   |
//! | `xs:unsignedByte`  | `u8`                    | OMSC-SPC-008 Table 9.1-1   |
//! | `xs:double`        | `f64`                   | OMSC-SPC-008 Table 9.1-1   |
//! | `xs:float`         | `f32`                   | OMSC-SPC-008 Table 9.1-1   |
//! | `xs:integer`       | `i64`                   | **CERT CAL-016024**        |
//! | `xs:duration`      | `i64` (nanoseconds)     | **CERT CAL-016027**        |
//! | `xs:dateTime`      | `DateTime` (chrono UTC) | **CERT CAL-016028**        |
//! | `xs:time`          | `i64` (ns since 00:00Z) | **CERT CAL-016029**        |
//! | `xs:string`        | `String`                | OMSC-SPC-008 Table 9.1-2   |
//! | `xs:anyURI`        | `String`                | **CERT CXX-004940**        |
//! | `xs:normalizedString` | `String`             | **CERT CXX-004940**        |
//! | `xs:token`         | `String`                | **CERT CXX-004940**        |
//! | `xs:hexBinary`     | `Vec<u8>`               | OMSC-SPC-008 Table 9.1-3   |
//! | `xs:base64Binary`  | `Vec<u8>`               | OMSC-SPC-008 Table 9.1-3   |
//!
//! ## `String` vs `&str`
//!
//! Unlike the original draft (`type String<'a> = &'a str`), string fields
//! are mapped to owned `String` values.  CAL Messages must own their data
//! so that they can be serialised and passed across thread boundaries
//! (`Send + Sync`).  Use `&str` only for temporary, read-only access.
//!
//! ## `xs:integer` (arbitrary-precision)
//!
//! XSD `xs:integer` is theoretically unbounded, but CERT CAL-016024 mandates
//! that the CAL represent it as a **signed 64-bit integer** to provide a
//! performant API.  CAL Clients must not rely on values outside `i64::MIN ..= i64::MAX`.

#![allow(dead_code)]
#![warn(missing_docs)]

// ── Simple primitive types (OMSC-SPC-008 Table 9.1-1) ───────────────────────

/// `xs:boolean` — two-valued logic.
pub type Boolean = bool;

/// `xs:long` — signed 64-bit integer.
pub type Long = i64;

/// `xs:int` — signed 32-bit integer.
pub type Int = i32;

/// `xs:short` — signed 16-bit integer.
pub type Short = i16;

/// `xs:byte` — signed 8-bit integer.
pub type Byte = i8;

/// `xs:unsignedLong` — unsigned 64-bit integer.
pub type UnsignedLong = u64;

/// `xs:unsignedInt` — unsigned 32-bit integer.
pub type UnsignedInt = u32;

/// `xs:unsignedShort` — unsigned 16-bit integer.
pub type UnsignedShort = u16;

/// `xs:unsignedByte` — unsigned 8-bit integer.
pub type UnsignedByte = u8;

/// `xs:double` — IEEE 754 double-precision 64-bit floating point.
pub type Double = f64;

/// `xs:float` — IEEE 754 single-precision 32-bit floating point.
pub type Float = f32;

/// `xs:integer` — arbitrary-precision integer.
///
/// Represented as a signed 64-bit integer per **CERT CAL-016024**.
/// Values outside `i64::MIN ..= i64::MAX` are not supported by the CAL API.
pub type Integer = i64;

// ── Time types (OMSC-SPC-001 §5.2.2) ────────────────────────────────────────

/// `xs:duration` — duration in **nanoseconds** (signed).
///
/// **CERT CAL-016027**: represented as a signed 64-bit integer.
/// Positive values indicate forward durations; negative indicate backward.
pub type Duration = i64;

/// `xs:dateTime` — UTC instant wrapping [`chrono::DateTime<chrono::Utc>`].
///
/// **CERT CAL-016028**: serialised as a signed 64-bit integer of nanoseconds
/// since the POSIX epoch (1970-01-01T00:00:00Z).  Negative values represent
/// instants before the epoch.  Use [`DateTime::from`] / [`Into<i64>`] to
/// convert to/from the raw nanosecond representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DateTime(chrono::DateTime<chrono::Utc>);

impl Default for DateTime {
    fn default() -> Self {
        Self(chrono::DateTime::UNIX_EPOCH)
    }
}

impl DateTime {
    /// Current UTC time.
    pub fn now() -> Self {
        Self(chrono::Utc::now())
    }
}

impl From<chrono::DateTime<chrono::Utc>> for DateTime {
    fn from(dt: chrono::DateTime<chrono::Utc>) -> Self {
        Self(dt)
    }
}

impl From<DateTime> for chrono::DateTime<chrono::Utc> {
    fn from(dt: DateTime) -> Self {
        dt.0
    }
}

impl From<i64> for DateTime {
    fn from(ns: i64) -> Self {
        Self(chrono::DateTime::from_timestamp_nanos(ns))
    }
}

impl From<DateTime> for i64 {
    fn from(dt: DateTime) -> Self {
        dt.0.timestamp_nanos_opt()
            .expect("DateTime nanosecond timestamp overflows i64 (representable range ~1678–2261)")
    }
}

impl serde::Serialize for DateTime {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        chrono::serde::ts_nanoseconds::serialize(&self.0, s)
    }
}

impl<'de> serde::Deserialize<'de> for DateTime {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        chrono::serde::ts_nanoseconds::deserialize(d).map(DateTime)
    }
}

/// `xs:time` — nanoseconds since 00:00:00.000000000Z of the current day.
///
/// **CERT CAL-016029**: represented as a signed 64-bit integer.
///
/// Valid range: `0 ..= 86_400_000_000_000` (one day in nanoseconds).
/// This invariant is documented only — the type alias does not enforce it.
/// Callers constructing a `Time` value are responsible for ensuring it falls
/// within the valid range.
pub type Time = i64;

// ── String types (OMSC-SPC-008 Table 9.1-2) ─────────────────────────────────

/// `xs:string` — an owned Unicode string.
///
/// Using an owned `String` (not `&str`) ensures CAL Messages are `Send + Sync`
/// and can be freely passed across thread boundaries.
pub type XsString = String;

/// `xs:string` accessor — same as [`XsString`]; provided for symmetry with
/// the C++ `StringAccessor` class.
pub type StringAccessor = String;

/// `xs:anyURI` — string-valued URI (CERT CXX-004940).
pub type AnyUri = String;

/// `xs:normalizedString` — whitespace-normalized string (CERT CXX-004940).
pub type NormalizedString = String;

/// `xs:token` — whitespace-collapsed string (CERT CXX-004940).
pub type Token = String;

// ── Binary types (OMSC-SPC-008 Table 9.1-3) ─────────────────────────────────

/// `xs:hexBinary` — a binary blob represented as an owned byte vector.
pub type HexBinary = Vec<u8>;

/// `xs:base64Binary` — a binary blob represented as an owned byte vector.
///
/// In-memory representation is identical to [`HexBinary`]; the distinction
/// matters only during serialisation (base64 encoding vs hex encoding).
/// OMSC-SPC-008 Table 9.1-3.
pub type Base64Binary = Vec<u8>;

// ════════════════════════════════════════════════════════════════════════════
// Unit tests
// ════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_is_i64() {
        // CERT CAL-016024
        let _: Integer = i64::MAX;
        let _: Integer = i64::MIN;
    }

    #[test]
    fn duration_and_time_are_i64() {
        // CERT CAL-016027 / CAL-016029
        let _: Duration = 1_000_000_000_i64; // 1 second in ns
        let _: Time = 0_i64; // midnight
    }

    #[test]
    fn datetime_roundtrips_nanoseconds() {
        // CERT CAL-016028: nanosecond integer round-trip
        let epoch = DateTime::from(0_i64);
        assert_eq!(i64::from(epoch), 0_i64);

        let ns: i64 = 1_700_000_000_000_000_000;
        let dt = DateTime::from(ns);
        assert_eq!(i64::from(dt), ns);
    }

    #[test]
    fn datetime_serializes_as_i64() {
        let dt = DateTime::from(42_i64);
        let json = serde_json::to_string(&dt).expect("serialize");
        assert_eq!(json, "42");
        let back: DateTime = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(i64::from(back), 42_i64);
    }

    #[test]
    fn datetime_now_is_positive() {
        let ns = i64::from(DateTime::now());
        assert!(ns > 0, "now() should be after epoch");
    }

    #[test]
    fn string_type_is_owned() {
        // Ensures XsString is owned (not a reference) so CAL Messages are Send.
        fn assert_send<T: Send>(_: T) {}
        let s: XsString = String::from("hello");
        assert_send(s);
    }

    #[test]
    fn binary_types_are_vec_u8() {
        // OMSC-SPC-008 Table 9.1-3: both binary types map to Vec<u8>
        let hex: HexBinary = vec![0xDE, 0xAD];
        let b64: Base64Binary = vec![0xBE, 0xEF];
        assert_eq!(hex.len(), 2);
        assert_eq!(b64.len(), 2);
    }
}
