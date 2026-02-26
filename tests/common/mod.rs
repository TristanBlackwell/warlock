use reqwest::Client;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use warlock::{app, capacity, firecracker};

/// Returns a shared test server address.
///
/// The server is started once and reused across all tests for efficiency.
/// Tests always run in development mode (Firecracker checks are skipped).
pub async fn get_server_addr() -> SocketAddr {
    static SERVER: once_cell::sync::Lazy<tokio::sync::Mutex<Option<SocketAddr>>> =
        once_cell::sync::Lazy::new(|| tokio::sync::Mutex::new(None));

    let mut guard = SERVER.lock().await;
    if let Some(addr) = *guard {
        return addr;
    }

    // Ensure tests run in development mode
    unsafe {
        std::env::set_var("WARLOCK_DEV", "true");
    }

    // Run preflight checks (will be skipped in dev mode, returns default config)
    let jailer_config =
        firecracker::preflight_check().expect("Firecracker preflight check failed");

    let host_capacity = capacity::available_capacity().expect("Failed to get capacity");
    let (app, _state) = app::create_app(host_capacity, jailer_config);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    *guard = Some(addr);
    addr
}

/// Returns a shared HTTP client for making test requests.
///
/// The client is created once and reused across all tests
pub fn get_client() -> &'static Client {
    static CLIENT: once_cell::sync::Lazy<Client> = once_cell::sync::Lazy::new(Client::new);
    &CLIENT
}
