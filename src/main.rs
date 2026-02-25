use anyhow::Context;
use tracing::{error, info};

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

    let (app, state) = app::create_app(host_capacity);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .context("Unable to bind TCP listener.")?;

    info!("Listening on {}", listener.local_addr()?);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("Failed to serve axum.")?;

    // Server has stopped — clean up all running VMs
    let mut vms = state.vms.lock().await;
    let vm_count = vms.len();

    if vm_count > 0 {
        info!("Shutting down {} running VM(s)...", vm_count);

        for (id, mut instance) in vms.drain() {
            if let Err(e) = instance.stop().await {
                error!(vm_id = %id, error = ?e, "Graceful stop failed, force-terminating");
            }
            // Instance is dropped here — SIGTERM + socket cleanup via FStack
            drop(instance);
            info!(vm_id = %id, "VM terminated");
        }
    }

    info!("Warlock shutdown complete");

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("Failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("Received Ctrl+C, initiating shutdown..."),
        _ = terminate => info!("Received SIGTERM, initiating shutdown..."),
    }
}
