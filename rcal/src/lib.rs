//! OMS Rust Critical Abstraction Layer (CAL)
//!
//! ## Specification references
//! - OMSC-SPC-001 Rev L — CAL Specification
//! - OMSC-SPC-008 Rev K — C++ CAL Interface Generation Specification

pub mod asb;
pub mod calconfig;
pub mod logging;
pub mod qname;
#[cfg(feature = "service")]
pub mod service;
pub mod uci;
pub mod xs;

pub use qname::{NamespaceResolver, QName};
