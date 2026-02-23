use reqwest::Client;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use warlock::{app, capacity};

/// Returns a shared test server address.
///
/// The server is started once and reused across all tests
pub async fn get_server_addr() -> SocketAddr {
    static SERVER: once_cell::sync::Lazy<tokio::sync::Mutex<Option<SocketAddr>>> =
        once_cell::sync::Lazy::new(|| tokio::sync::Mutex::new(None));

    let mut guard = SERVER.lock().await;
    if let Some(addr) = *guard {
        return addr;
    }

    let host_capacity = capacity::available_capacity().expect("Failed to get capacity");
    let app = app::create_app(host_capacity);
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
