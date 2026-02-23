use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// Initialize the logging infrastructure for the application.
///
/// Sets up human-readable logs with timestamps and log levels.
/// Log verbosity can be controlled via the RUST_LOG environment variable.
///
/// Examples:
/// - RUST_LOG=debug cargo run (verbose logging)
/// - RUST_LOG=info cargo run (info and above)
/// - RUST_LOG=warlock=debug,axum=info (fine-grained control)
pub fn init() {
    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warlock=info,tower_http=debug,axum=debug".into()),
        )
        .with(fmt::layer().with_target(true).with_line_number(true))
        .init();
}
