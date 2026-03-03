use anyhow::Context;
use tracing::{error, info};

mod app;
mod capacity;
mod error;
mod firecracker;
mod handlers;
mod logging;
mod vm;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    logging::init();

    info!("Starting warlock v{}...", env!("CARGO_PKG_VERSION"));

    let jailer_config =
        firecracker::preflight_check().context("Firecracker pre-flight check failed")?;

    // Clean sweep: kill orphaned VMs and remove stale resources from any
    // previous Warlock instance that didn't shut down cleanly.
    firecracker::orphan::cleanup_orphans(&jailer_config.vm_images_dir);

    let host_capacity = capacity::available_capacity()
        .context("Failed to read host capacity during initialisation")?;

    info!(
        "Machine - memory: {} MB, vcpus: {}",
        host_capacity.memory_mb, host_capacity.vcpus
    );

    let (app, state) = app::create_app(host_capacity, jailer_config);

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

        for (id, mut entry) in vms.drain() {
            if let Err(e) = entry.instance.stop().await {
                error!(vm_id = %id, error = ?e, "Graceful stop failed, force-terminating");
            }

            let rootfs_copy = entry.rootfs_copy.clone();
            let tap_name = entry.tap_name.clone();
            let nat_handles = entry.nat_handles;
            let subnet_index = entry.subnet_index;

            // Entry is dropped here — SIGTERM + socket + jailer workspace cleanup via FStack
            drop(entry);

            // Clean up networking: tap device, NAT rules, subnet allocation
            if let Some(ref name) = tap_name {
                firecracker::network::delete_tap(name);
            }
            if let Some(ref handles) = nat_handles {
                firecracker::network::remove_nat_rules(handles);
            }
            if let Some(index) = subnet_index {
                state.subnet_pool.lock().await.release(index);
            }

            // Clean up the per-VM rootfs copy (outside the jailer workspace)
            if let Some(ref path) = rootfs_copy {
                if let Err(e) = std::fs::remove_file(path) {
                    error!(vm_id = %id, error = ?e, "Failed to remove rootfs copy");
                }
            }
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
