use reqwest::Client;
use std::net::SocketAddr;
use std::sync::OnceLock;
use warlock::{app, capacity, firecracker};

/// Returns a shared test server address.
///
/// The server is started once on a dedicated thread with its own tokio runtime,
/// ensuring it outlives any individual `#[tokio::test]` runtime. This is
/// necessary because each `#[tokio::test]` creates an independent runtime that
/// shuts down when that test completes — a server spawned inside one test's
/// runtime would die before other tests can reach it.
pub fn get_server_addr() -> SocketAddr {
    static SERVER_ADDR: OnceLock<SocketAddr> = OnceLock::new();

    *SERVER_ADDR.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
            rt.block_on(async {
                // Ensure tests run in development mode
                unsafe {
                    std::env::set_var("WARLOCK_DEV", "true");
                }

                // Run preflight checks (will be skipped in dev mode, returns default config)
                let jailer_config =
                    firecracker::preflight_check().expect("Firecracker preflight check failed");

                let host_capacity = capacity::available_capacity().expect("Failed to get capacity");
                let (app, _state) = app::create_app(host_capacity, jailer_config, None);
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();

                tx.send(addr).expect("failed to send server address");

                axum::serve(listener, app).await.unwrap();
            });
        });

        rx.recv().expect("server thread failed to start")
    })
}

/// Returns a shared HTTP client for making test requests.
///
/// The client is created once and reused across all tests
pub fn get_client() -> &'static Client {
    static CLIENT: once_cell::sync::Lazy<Client> = once_cell::sync::Lazy::new(Client::new);
    &CLIENT
}
