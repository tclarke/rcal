//! XMLSchema (xs:) namespace definitions.
//!
//! Definitions for the XMLSchema types.
//!
//! ## Specification references
//! - OMSC-SPC-001 rev L – CAL Specification
//! - OMSC-SPC-008 rev K – C++ CAL Interface Generation Specification
//!

#![allow(dead_code)]
#![warn(missing_docs)]

//! XMLSchema primitive types.
//! Binary blob represented as octets
type HexBinary = Vec<u8>;

type Time = i64;
type DateTime = i64;
type Duration = i64;

type Boolean = bool;
type Long = i64;
type Int = i32;
type Short = i16;
type Byte = i8;

type UnsignedLong = u64;
type UnsignedInt = u32;
type UnsignedShort = u16;
type UnsignedByte = u8;

type Double = f64;
type Float = f32;

type String<'a> = &'a str;
type StringAccessor<'a> = String<'a>;
