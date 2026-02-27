mod common;

use tokio::time::{timeout, Duration};

#[tokio::test]
async fn healthcheck_returns_200_with_enriched_json() {
    let addr = common::get_server_addr();
    let client = common::get_client();

    let response = timeout(
        Duration::from_secs(5),
        client.get(format!("http://{}/internal/hc", addr)).send(),
    )
    .await
    .expect("request timed out")
    .expect("request failed");

    assert_eq!(response.status().as_u16(), 200);

    let body: serde_json::Value = response.json().await.expect("failed to parse JSON");

    assert_eq!(body["status"], "healthy");
    assert!(body["version"].is_string());
    assert!(body["capacity"].is_object());
    assert!(body["capacity"]["total_vcpus"].as_u64().unwrap() > 0);
    assert!(body["capacity"]["total_memory_mb"].as_u64().unwrap() > 0);
    assert_eq!(body["vms"]["count"], 0);
    assert_eq!(body["copy_strategy"], "sparse"); // dev mode defaults to sparse
}
