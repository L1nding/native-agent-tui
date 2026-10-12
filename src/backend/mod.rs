//! The dsh backend and its protocol adapter.
//!
//! The Core talks to one small JSON-RPC envelope seam. The dsh process and the
//! translation to its protocol live below this module so the UI never needs to
//! inspect backend wire messages.

pub(crate) mod acp;
pub mod acp_protocol;
pub mod dsh_config;

/// Name under which choices for this backend are stored in preferences. It
/// predates the bridge plugin and is kept so saved model choices carry over.
pub const PREFERENCE_KEY: &str = "deepseek-acp";
