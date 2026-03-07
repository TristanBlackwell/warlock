mod common;

use tokio::time::{Duration, timeout};

#[tokio::test]
async fn liveness_returns_200_with_ok_status() {
    let addr = common::get_server_addr();
    let client = common::get_client();

    let response = timeout(
        Duration::from_secs(5),
        client
            .get(format!("http://{}/internal/health", addr))
            .send(),
    )
    .await
    .expect("request timed out")
    .expect("request failed");

    assert_eq!(response.status().as_u16(), 200);

    let body: serde_json::Value = response.json().await.expect("failed to parse JSON");

    assert_eq!(body["status"], "ok");
    // Liveness is minimal — no capacity, vms, or version fields
    assert!(body.get("version").is_none());
    assert!(body.get("capacity").is_none());
    assert!(body.get("vms").is_none());
}

#[tokio::test]
async fn readiness_returns_200_with_enriched_json() {
    let addr = common::get_server_addr();
    let client = common::get_client();

    let response = timeout(
        Duration::from_secs(5),
        client.get(format!("http://{}/internal/ready", addr)).send(),
    )
    .await
    .expect("request timed out")
    .expect("request failed");

    assert_eq!(response.status().as_u16(), 200);

    let body: serde_json::Value = response.json().await.expect("failed to parse JSON");

    assert_eq!(body["status"], "ready");
    assert!(body["version"].is_string());
    assert!(body["capacity"].is_object());
    assert!(body["capacity"]["total_vcpus"].as_u64().unwrap() > 0);
    assert!(body["capacity"]["total_memory_mb"].as_u64().unwrap() > 0);
    assert_eq!(body["vms"]["count"], 0);
    let copy_strategy = body["copy_strategy"]
        .as_str()
        .expect("copy_strategy should be a string");
    assert!(matches!(copy_strategy, "sparse" | "reflink"));
}
