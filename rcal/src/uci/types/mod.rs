//! Namespace for all generated OMS UCI types.
//!
//! Message element and types are in the form [`SystemStatus_`] but the associated
//! trait [`SystemStatus`] should be used everywhere except when creating a message.
//!
//! Optionally, extension traits can be added which add static, predefined methods
//! to a type. Useful for returning strings that combine multiple elements, etc.

include!(concat!(env!("OUT_DIR"), "/uci_types/mod.rs"));

pub mod security_info_ext;
