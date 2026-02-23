use reqwest::Client;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio::time::{timeout, Duration};
use warlock::app;

async fn get_server_addr() -> SocketAddr {
    static SERVER: once_cell::sync::Lazy<tokio::sync::Mutex<Option<SocketAddr>>> =
        once_cell::sync::Lazy::new(|| tokio::sync::Mutex::new(None));

    {
        let mut guard = SERVER.lock().await;
        if let Some(addr) = *guard {
            return addr;
        }

        let app = app::create_app();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        *guard = Some(addr);
        addr
    }
}

fn get_client() -> &'static Client {
    static CLIENT: once_cell::sync::Lazy<Client> = once_cell::sync::Lazy::new(|| Client::new());
    &CLIENT
}

#[tokio::test]
async fn healthcheck_returns_200_and_expected_message() {
    let addr = get_server_addr().await;
    let client = get_client();

    let response = timeout(
        Duration::from_secs(5),
        client.get(format!("http://{}/internal/hc", addr)).send(),
    )
    .await
    .expect("request timed out")
    .expect("request failed");

    assert_eq!(response.status().as_u16(), 200);

    let body = response.text().await.expect("failed to read body");
    assert_eq!(body, "Alive and kickin");
}
