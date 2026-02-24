use anyhow::Context;
use tracing::info;

mod app;
mod capacity;
mod error;
mod firecracker;
mod handlers;
mod logging;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    logging::init();

    info!("Starting warlock v{}...", env!("CARGO_PKG_VERSION"));

    firecracker::preflight_check().context("Firecracker pre-flight check failed")?;

    let host_capacity = capacity::available_capacity()
        .context("Failed to read host capacity during initialisation")?;

    info!(
        "Machine - memory: {} MB, vcpus: {}",
        host_capacity.memory_mb, host_capacity.vcpus
    );

    let app = app::create_app(host_capacity);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .context("Unable to bind TCP listener.")?;

    info!("Listening on {}", listener.local_addr()?);

    axum::serve(listener, app)
        .await
        .context("Failed to serve axum.")?;

    Ok(())
}
