//! A service which acts as a bridge betwen multiple cal instantiations.
//!
//! It's basic function just copies all messages between busses
//! Configuring individual topics will only forward messages on those topics.
//!
//! # Configuration
//!
//! The service section should point to 1 cal instance. Specify the other cal instances
//! with `service.bridge = ["a", "b"]`
//!
//! # Running
//!
//! Create a suitable `CALContig.toml` (see example in repo) and run from that
//! directory or set RCAL_CONFIG to point to the toml file.
//!
//! ```text
//! cargo run -- cal-bridge
//! ```

use rcal::service::{AbstractService, AbstractServiceImpl};

struct CalConfigService(AbstrctServiceImple) {

}

impl Deref for CalConfigService {
    fn deref(&self) -> &AbstractServiceImpl {
        &self.0
    }
}

impl DerefMut for CalConfigService {
    fn deref_mut(&mut self) -> &mut AbstractServiceImpl {
        &mut self.0
    }
}
