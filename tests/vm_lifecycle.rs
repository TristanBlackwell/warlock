//! Live integration tests for the full VM lifecycle.
//!
//! These tests create real Firecracker microVMs and require a fully provisioned
//! host (Firecracker, jailer, KVM, kernel, rootfs, `/srv/jailer/` layout).
//!
//! **Gating**: Tests only run when `WARLOCK_LIVE=true` is set. Without it,
//! every test returns early — `cargo test` stays fast and safe on macOS and CI.
//!
//! **Running**: `make test-live` on a provisioned host, or manually:
//! ```sh
//! WARLOCK_LIVE=true cargo test --test vm_lifecycle -- --nocapture
//! ```

use std::net::SocketAddr;
use std::path::Path;
use std::sync::OnceLock;

use reqwest::Client;
use tokio::time::{timeout, Duration};

/// Returns true if the live test environment is available.
///
/// When `WARLOCK_LIVE` is not `"true"`, tests print a skip message and
/// return early. This keeps `cargo test` safe on machines without Firecracker.
fn require_live() -> bool {
    std::env::var("WARLOCK_LIVE")
        .map(|v| v == "true")
        .unwrap_or(false)
}

/// Starts a **real-mode** Warlock server (no `WARLOCK_DEV`).
///
/// This runs the full preflight check, so it will fail on machines without
/// Firecracker/KVM/jailer. The server is started once on a dedicated thread
/// with its own tokio runtime, ensuring it outlives any individual
/// `#[tokio::test]` runtime.
fn get_live_server_addr() -> SocketAddr {
    static SERVER_ADDR: OnceLock<SocketAddr> = OnceLock::new();

    *SERVER_ADDR.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
            rt.block_on(async {
                // Ensure WARLOCK_DEV is NOT set — we want real preflight checks
                unsafe {
                    std::env::remove_var("WARLOCK_DEV");
                }

                let jailer_config = warlock::firecracker::preflight_check()
                    .expect("Firecracker preflight check failed — is this a provisioned host?");

                let host_capacity =
                    warlock::capacity::available_capacity().expect("Failed to get host capacity");

                let (app, _state) = warlock::app::create_app(host_capacity, jailer_config);
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();

                tx.send(addr).expect("failed to send server address");

                axum::serve(listener, app).await.unwrap();
            });
        });

        rx.recv().expect("server thread failed to start")
    })
}

fn get_client() -> &'static Client {
    static CLIENT: once_cell::sync::Lazy<Client> = once_cell::sync::Lazy::new(Client::new);
    &CLIENT
}

/// Helper: send a request and return (status_code, parsed JSON body).
async fn request(
    method: &str,
    url: &str,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let client = get_client();

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

    let response = timeout(Duration::from_secs(30), req.send())
        .await
        .expect("request timed out")
        .expect("request failed");

    let status = response.status().as_u16();
    let json = response.json().await.expect("failed to parse JSON");
    (status, json)
}

/// Helper: delete a VM, ignoring errors. Used for test cleanup.
async fn cleanup_vm(addr: &SocketAddr, id: &str) {
    let client = get_client();
    let _ = timeout(
        Duration::from_secs(10),
        client
            .delete(format!("http://{}/vm/{}", addr, id))
            .send(),
    )
    .await;
}

// ── Full Lifecycle ──

#[tokio::test]
async fn full_lifecycle() {
    if !require_live() {
        eprintln!("skipped (WARLOCK_LIVE not set)");
        return;
    }

    let addr = get_live_server_addr();

    // ── Create ──
    let (create_status, create_body) = request(
        "POST",
        &format!("http://{}/vm", addr),
        None, // defaults: 1 vCPU, 128 MB
    )
    .await;

    let vm_id = create_body["id"].as_str().unwrap_or("").to_string();

    // ── Get ──
    let (get_status, get_body) =
        request("GET", &format!("http://{}/vm/{}", addr, vm_id), None).await;

    // ── List ──
    let (list_status, list_body) = request("GET", &format!("http://{}/vm", addr), None).await;

    let list_has_vm = list_body["vms"]
        .as_array()
        .map(|vms| vms.iter().any(|v| v["id"].as_str() == Some(&vm_id)))
        .unwrap_or(false);

    // ── Delete ──
    let (delete_status, delete_body) =
        request("DELETE", &format!("http://{}/vm/{}", addr, vm_id), None).await;

    // ── Verify gone ──
    let (gone_status, _) =
        request("GET", &format!("http://{}/vm/{}", addr, vm_id), None).await;

    // ── Verify rootfs cleaned up ──
    let rootfs_path = format!("/srv/jailer/vm-images/{}.ext4", vm_id);
    let rootfs_exists = Path::new(&rootfs_path).exists();

    // ── Assert everything ──
    // (assertions are after cleanup so the VM doesn't leak on failure)

    // Create
    assert_eq!(create_status, 202, "expected 202 Accepted for create");
    assert!(
        uuid::Uuid::parse_str(&vm_id).is_ok(),
        "response id should be a valid UUID"
    );
    assert_eq!(create_body["vcpus"], 1);
    assert_eq!(create_body["memory_mb"], 128);
    assert_eq!(create_body["state"], "Running");
    assert!(create_body["vmm_version"].is_string());
    assert!(
        create_body["guest_ip"].is_string(),
        "response should include guest_ip"
    );

    // Get
    assert_eq!(get_status, 200, "expected 200 for get");
    assert_eq!(get_body["state"], "Running");

    // List
    assert_eq!(list_status, 200, "expected 200 for list");
    assert!(
        list_body["count"].as_u64().unwrap() >= 1,
        "list should contain at least 1 VM"
    );
    assert!(list_has_vm, "list should contain the created VM");

    // Delete
    assert_eq!(delete_status, 200, "expected 200 for delete");
    assert_eq!(delete_body["deleted"], true);

    // Gone
    assert_eq!(gone_status, 404, "expected 404 after deletion");

    // Rootfs cleanup
    assert!(
        !rootfs_exists,
        "rootfs copy should be removed after deletion: {}",
        rootfs_path
    );
}

// ── Custom Configuration ──

#[tokio::test]
async fn create_vm_with_custom_config() {
    if !require_live() {
        eprintln!("skipped (WARLOCK_LIVE not set)");
        return;
    }

    let addr = get_live_server_addr();

    // Create with non-default config
    let (create_status, create_body) = request(
        "POST",
        &format!("http://{}/vm", addr),
        Some(serde_json::json!({ "vcpus": 2, "memory_mb": 256 })),
    )
    .await;

    let vm_id = create_body["id"].as_str().unwrap_or("").to_string();

    // Clean up before asserting
    cleanup_vm(&addr, &vm_id).await;

    assert_eq!(create_status, 202, "expected 202 Accepted for create");
    assert_eq!(create_body["vcpus"], 2);
    assert_eq!(create_body["memory_mb"], 256);
    assert_eq!(create_body["state"], "Running");
    assert!(
        create_body["guest_ip"].is_string(),
        "response should include guest_ip"
    );
}

// ── Healthcheck with running VMs ──

#[tokio::test]
async fn healthcheck_reflects_allocated_resources() {
    if !require_live() {
        eprintln!("skipped (WARLOCK_LIVE not set)");
        return;
    }

    let addr = get_live_server_addr();

    // Create a VM so the healthcheck has something to report
    let (_, create_body) = request("POST", &format!("http://{}/vm", addr), None).await;

    let vm_id = create_body["id"].as_str().unwrap_or("").to_string();

    // Check healthcheck
    let (hc_status, hc_body) =
        request("GET", &format!("http://{}/internal/hc", addr), None).await;

    // Clean up before asserting
    cleanup_vm(&addr, &vm_id).await;

    assert_eq!(hc_status, 200);
    assert_eq!(hc_body["status"], "healthy");
    assert!(
        hc_body["vms"]["count"].as_u64().unwrap() >= 1,
        "healthcheck should show at least 1 VM"
    );
    assert!(
        hc_body["vms"]["allocated_vcpus"].as_u64().unwrap() >= 1,
        "healthcheck should show allocated vCPUs"
    );
    assert!(
        hc_body["vms"]["allocated_memory_mb"].as_u64().unwrap() >= 128,
        "healthcheck should show allocated memory"
    );
}
