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
//! | `xs:dateTime`      | `i64` (ns since epoch)  | **CERT CAL-016028**        |
//! | `xs:time`          | `i64` (ns since 00:00Z) | **CERT CAL-016029**        |
//! | `xs:string`        | `String`                | OMSC-SPC-008 Table 9.1-2   |
//! | `xs:hexBinary`     | `Vec<u8>`               | OMSC-SPC-008 Table 9.1-3   |
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

/// `xs:dateTime` — nanoseconds since the POSIX epoch (1970-01-01T00:00:00Z).
///
/// **CERT CAL-016028**: represented as a signed 64-bit integer.
/// Negative values represent dates before the POSIX epoch.
pub type DateTime = i64;

/// `xs:time` — nanoseconds since 00:00:00.000000000Z of the current day.
///
/// **CERT CAL-016029**: represented as a signed 64-bit integer.
/// Values are always in the range `0 ..= 86_400_000_000_000` (one day in ns).
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

// ── Binary types (OMSC-SPC-008 Table 9.1-3) ─────────────────────────────────

/// `xs:hexBinary` — a binary blob represented as an owned byte vector.
pub type HexBinary = Vec<u8>;

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
    fn time_types_are_i64() {
        // CERT CAL-016027 / CAL-016028 / CAL-016029
        let _: Duration = 1_000_000_000_i64; // 1 second in ns
        let _: DateTime = 0_i64; // POSIX epoch
        let _: Time = 0_i64; // midnight
    }

    #[test]
    fn string_type_is_owned() {
        // Ensures XsString is owned (not a reference) so CAL Messages are Send.
        fn assert_send<T: Send>(_: T) {}
        let s: XsString = String::from("hello");
        assert_send(s);
    }
}
