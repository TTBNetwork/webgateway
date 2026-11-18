//! Tracing initialization shared by all binaries.

use tracing_subscriber::EnvFilter;

/// Initialize the global tracing subscriber.
///
/// The default filter is used when RUST_LOG is not set. Repeated calls are
/// ignored so tests and embedding binaries cannot panic on double init.
pub fn init(default_filter: &str) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));

    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}
