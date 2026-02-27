mod common;

use tokio::time::{timeout, Duration};

#[tokio::test]
async fn list_vms_returns_empty_list() {
    let addr = common::get_server_addr().await;
    let client = common::get_client();

    let response = timeout(
        Duration::from_secs(5),
        client.get(format!("http://{}/vm", addr)).send(),
    )
    .await
    .expect("request timed out")
    .expect("request failed");

    assert_eq!(response.status().as_u16(), 200);

    let text = response.text().await.expect("failed to read body");
    let body: serde_json::Value = serde_json::from_str(&text).expect("failed to parse JSON");
    assert_eq!(body["count"], 0);
    assert_eq!(body["vms"], serde_json::json!([]));
}
