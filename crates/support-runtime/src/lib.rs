pub mod agent;
pub mod audio;
pub mod commands;
pub mod config;
pub mod google;
pub mod http;
pub mod integrations;
pub mod knowledge;
#[cfg(feature = "local-speech")]
pub mod local_speech;
#[cfg(feature = "local-speech")]
pub mod models;
pub mod provider;
pub mod runtime;
pub mod secrets;
pub mod speech;
pub use runtime::Runtime;
pub use support_core::error::{AppError, Result};

pub fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}
