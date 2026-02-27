mod common;

use tokio::time::{timeout, Duration};
use uuid::Uuid;

/// Helper: send a request and return (status_code, parsed JSON body).
async fn request(
    method: &str,
    url: &str,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let client = common::get_client();

    let req = match method {
        "GET" => client.get(url),
        "POST" => {
            let r = client.post(url);
            match body {
                Some(b) => r.json(&b),
                None => r,
            }
        }
        "DELETE" => client.delete(url),
        _ => panic!("unsupported method: {}", method),
    };

    let response = timeout(Duration::from_secs(5), req.send())
        .await
        .expect("request timed out")
        .expect("request failed");

    let status = response.status().as_u16();
    let json = response.json().await.expect("failed to parse JSON");
    (status, json)
}

// ── List ──

#[tokio::test]
async fn list_vms_returns_empty_list() {
    let addr = common::get_server_addr().await;
    let (status, body) = request("GET", &format!("http://{}/vm", addr), None).await;

    assert_eq!(status, 200);
    assert_eq!(body["count"], 0);
    assert_eq!(body["vms"], serde_json::json!([]));
}

// ── Validation errors (422) ──

#[tokio::test]
async fn create_vm_rejects_zero_vcpus() {
    let addr = common::get_server_addr().await;
    let (status, body) = request(
        "POST",
        &format!("http://{}/vm", addr),
        Some(serde_json::json!({ "vcpus": 0 })),
    )
    .await;

    assert_eq!(status, 422);
    assert!(body["error"].as_str().unwrap().contains("vcpus"));
}

#[tokio::test]
async fn create_vm_rejects_odd_vcpus() {
    let addr = common::get_server_addr().await;
    let (status, body) = request(
        "POST",
        &format!("http://{}/vm", addr),
        Some(serde_json::json!({ "vcpus": 3 })),
    )
    .await;

    assert_eq!(status, 422);
    assert!(body["error"].as_str().unwrap().contains("vcpus"));
}

#[tokio::test]
async fn create_vm_rejects_vcpus_over_max() {
    let addr = common::get_server_addr().await;
    let (status, body) = request(
        "POST",
        &format!("http://{}/vm", addr),
        Some(serde_json::json!({ "vcpus": 33 })),
    )
    .await;

    assert_eq!(status, 422);
    assert!(body["error"].as_str().unwrap().contains("vcpus"));
}

#[tokio::test]
async fn create_vm_rejects_memory_below_minimum() {
    let addr = common::get_server_addr().await;
    let (status, body) = request(
        "POST",
        &format!("http://{}/vm", addr),
        Some(serde_json::json!({ "memory_mb": 64 })),
    )
    .await;

    assert_eq!(status, 422);
    assert!(body["error"].as_str().unwrap().contains("memory_mb"));
}

// ── Not found (404) ──

#[tokio::test]
async fn get_nonexistent_vm_returns_404() {
    let addr = common::get_server_addr().await;
    let id = Uuid::new_v4();
    let (status, body) = request("GET", &format!("http://{}/vm/{}", addr, id), None).await;

    assert_eq!(status, 404);
    assert_eq!(body["error"], "VM not found");
}

#[tokio::test]
async fn delete_nonexistent_vm_returns_404() {
    let addr = common::get_server_addr().await;
    let id = Uuid::new_v4();
    let (status, body) = request("DELETE", &format!("http://{}/vm/{}", addr, id), None).await;

    assert_eq!(status, 404);
    assert_eq!(body["error"], "VM not found");
}

// ── Bad path parameter (400) ──

#[tokio::test]
async fn get_vm_with_invalid_uuid_returns_400() {
    let addr = common::get_server_addr().await;
    let client = common::get_client();

    let response = timeout(
        Duration::from_secs(5),
        client
            .get(format!("http://{}/vm/not-a-uuid", addr))
            .send(),
    )
    .await
    .expect("request timed out")
    .expect("request failed");

    assert_eq!(response.status().as_u16(), 400);
}

#[tokio::test]
async fn delete_vm_with_invalid_uuid_returns_400() {
    let addr = common::get_server_addr().await;
    let client = common::get_client();

    let response = timeout(
        Duration::from_secs(5),
        client
            .delete(format!("http://{}/vm/not-a-uuid", addr))
            .send(),
    )
    .await
    .expect("request timed out")
    .expect("request failed");

    assert_eq!(response.status().as_u16(), 400);
}

// ── Create with valid config but no Firecracker (500) ──

#[tokio::test]
async fn create_vm_with_valid_config_returns_500_in_dev_mode() {
    // In dev mode, validation passes but canonicalize("/opt/firecracker/vmlinux")
    // fails because the path doesn't exist. The error goes through
    // From<anyhow::Error> so the client sees a generic message.
    let addr = common::get_server_addr().await;
    let (status, body) = request(
        "POST",
        &format!("http://{}/vm", addr),
        Some(serde_json::json!({ "vcpus": 1, "memory_mb": 128 })),
    )
    .await;

    assert_eq!(status, 500);
    assert_eq!(body["error"], "Internal server error");
}

#[tokio::test]
async fn create_vm_with_empty_body_returns_500_in_dev_mode() {
    // Empty JSON body means defaults (1 vCPU, 128 MB) — validation passes, but
    // filesystem operations fail in dev mode. Same obfuscation applies.
    let addr = common::get_server_addr().await;
    let (status, body) = request(
        "POST",
        &format!("http://{}/vm", addr),
        Some(serde_json::json!({})),
    )
    .await;

    assert_eq!(status, 500);
    assert_eq!(body["error"], "Internal server error");
}
