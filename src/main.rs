use anyhow::Context;
use tracing::{error, info};

mod app;
mod capacity;
mod error;
mod firecracker;
mod handlers;
mod logging;
mod ssh;
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

    // HTTP server
    let http_listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .context("Unable to bind HTTP listener.")?;

    info!("HTTP server listening on {}", http_listener.local_addr()?);

    // SSH server
    let ssh_server = ssh::WarlockSshServer::new(state.clone());
    let ssh_port = 2222;

    info!("SSH server listening on 0.0.0.0:{}", ssh_port);

    // Run both servers concurrently
    let http_task = axum::serve(http_listener, app).with_graceful_shutdown(shutdown_signal());
    let ssh_task = ssh_server.run(ssh_port);

    tokio::select! {
        result = http_task => {
            result.context("HTTP server failed")?;
        }
        result = ssh_task => {
            result.context("SSH server failed")?;
        }
    }

    // Server has stopped — clean up all registered VMs
    let mut vms = state.vms.lock().await;
    let vm_count = vms.len();

    if vm_count > 0 {
        info!("Shutting down {} VM(s)...", vm_count);

        for (id, entry) in vms.drain() {
            // Stop the Firecracker instance if it exists (Running state).
            // Creating entries have no instance — they were mid-setup when
            // shutdown was triggered. Their resources still need cleanup.
            let resources = match entry {
                app::VmEntry::Running {
                    mut instance,
                    resources,
                } => {
                    if let Err(e) = instance.stop().await {
                        error!(vm_id = %id, error = ?e, "Graceful stop failed, force-terminating");
                    }
                    // Instance dropped here — FStack sends SIGTERM + cleans
                    // up socket + jailer workspace
                    drop(instance);
                    resources
                }
                app::VmEntry::Creating(resources) => resources,
            };

            // Clean up networking: tap device, NAT rules, subnet allocation
            if let Some(ref name) = resources.tap_name {
                firecracker::network::delete_tap(name);
            }
            if let Some(ref handles) = resources.nat_handles {
                firecracker::network::remove_nat_rules(handles);
            }
            if let Some(index) = resources.subnet_index {
                state.subnet_pool.lock().await.release(index);
            }

            // Clean up the per-VM rootfs copy (outside the jailer workspace)
            if let Some(ref path) = resources.rootfs_copy
                && let Err(e) = std::fs::remove_file(path)
            {
                error!(vm_id = %id, error = ?e, "Failed to remove rootfs copy");
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
