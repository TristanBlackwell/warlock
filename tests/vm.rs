mod common;

use std::path::PathBuf;

use tokio::time::{Duration, timeout};
use uuid::Uuid;
use warlock::{
    app::{self, VmEntry, VmResources},
    capacity::Capacity,
    firecracker::{CopyStrategy, JailerConfig},
};

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

/// Start an isolated in-process app instance for state-driven handler tests.
async fn spawn_isolated_server() -> (std::net::SocketAddr, std::sync::Arc<app::AppState>) {
    let capacity = Capacity {
        memory_mb: 4096,
        vcpus: 8,
    };

    let jailer = JailerConfig {
        cgroup_version: 2,
        firecracker_path: PathBuf::from("firecracker"),
        jailer_path: PathBuf::from("jailer"),
        kernel_path: PathBuf::from("/tmp/fake-kernel"),
        rootfs_path: PathBuf::from("/tmp/fake-rootfs"),
        vm_images_dir: PathBuf::from("/tmp/fake-vm-images"),
        copy_strategy: CopyStrategy::Sparse,
        host_interface: "eth0".into(),
    };

    let (app, state) = app::create_app(capacity, jailer, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind isolated test listener");
    let addr = listener
        .local_addr()
        .expect("failed to get isolated test listener addr");

    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("isolated test server failed");
    });

    (addr, state)
}

// ── List ──

#[tokio::test]
async fn list_vms_returns_empty_list() {
    let addr = common::get_server_addr();
    let (status, body) = request("GET", &format!("http://{}/vm", addr), None).await;

    assert_eq!(status, 200);
    assert_eq!(body["count"], 0);
    assert_eq!(body["vms"], serde_json::json!([]));
}

// ── Validation errors (422) ──

#[tokio::test]
async fn create_vm_rejects_zero_vcpus() {
    let addr = common::get_server_addr();
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
    let addr = common::get_server_addr();
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
    let addr = common::get_server_addr();
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
    let addr = common::get_server_addr();
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
    let addr = common::get_server_addr();
    let id = Uuid::new_v4();
    let (status, body) = request("GET", &format!("http://{}/vm/{}", addr, id), None).await;

    assert_eq!(status, 404);
    assert_eq!(body["error"], "VM not found");
}

#[tokio::test]
async fn delete_nonexistent_vm_returns_404() {
    let addr = common::get_server_addr();
    let id = Uuid::new_v4();
    let (status, body) = request("DELETE", &format!("http://{}/vm/{}", addr, id), None).await;

    assert_eq!(status, 404);
    assert_eq!(body["error"], "VM not found");
}

#[tokio::test]
async fn unknown_route_returns_not_found_json() {
    let addr = common::get_server_addr();
    let (status, body) = request("GET", &format!("http://{}/does-not-exist", addr), None).await;

    assert_eq!(status, 404);
    assert_eq!(body["error"], "Not found");
}

#[tokio::test]
async fn get_vm_in_creating_state_returns_409() {
    let (addr, state) = spawn_isolated_server().await;
    let vm_id = Uuid::new_v4();

    {
        let mut vms = state.vms.lock().await;
        vms.insert(
            vm_id,
            VmEntry::Creating(VmResources {
                vcpus: 1,
                memory_mb: 128,
                rootfs_copy: None,
                tap_name: None,
                subnet_index: None,
                nat_handles: None,
                guest_ip: None,
                vsock_uds_path: None,
                ssh_keys: vec![],
            }),
        );
    }

    let (status, body) = request("GET", &format!("http://{}/vm/{}", addr, vm_id), None).await;

    assert_eq!(status, 409);
    assert_eq!(body["error"], "VM is still being created");
}

#[tokio::test]
async fn delete_vm_in_creating_state_returns_409_and_preserves_entry() {
    let (addr, state) = spawn_isolated_server().await;
    let vm_id = Uuid::new_v4();

    {
        let mut vms = state.vms.lock().await;
        vms.insert(
            vm_id,
            VmEntry::Creating(VmResources {
                vcpus: 1,
                memory_mb: 128,
                rootfs_copy: None,
                tap_name: None,
                subnet_index: None,
                nat_handles: None,
                guest_ip: None,
                vsock_uds_path: None,
                ssh_keys: vec![],
            }),
        );
    }

    let (status, body) = request("DELETE", &format!("http://{}/vm/{}", addr, vm_id), None).await;

    assert_eq!(status, 409);
    assert_eq!(
        body["error"],
        "VM is still being created, try again shortly"
    );

    let vms = state.vms.lock().await;
    assert!(vms.contains_key(&vm_id));
}

#[tokio::test]
async fn get_vm_with_invalid_uuid_returns_400() {
    let addr = common::get_server_addr();
    let client = common::get_client();

    let response = timeout(
        Duration::from_secs(5),
        client.get(format!("http://{}/vm/not-a-uuid", addr)).send(),
    )
    .await
    .expect("request timed out")
    .expect("request failed");

    assert_eq!(response.status().as_u16(), 400);
}

#[tokio::test]
async fn delete_vm_with_invalid_uuid_returns_400() {
    let addr = common::get_server_addr();
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
async fn create_vm_with_valid_config_returns_obfuscated_internal_error() {
    let addr = common::get_server_addr();
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
async fn create_vm_with_empty_body_returns_obfuscated_internal_error() {
    let addr = common::get_server_addr();
    let (status, body) = request(
        "POST",
        &format!("http://{}/vm", addr),
        Some(serde_json::json!({})),
    )
    .await;

    assert_eq!(status, 500);
    assert_eq!(body["error"], "Internal server error");
}
