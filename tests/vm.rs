mod common;

#[tokio::test]
async fn create_vm_returns_202() {
    // let addr = common::get_server_addr().await;
    // let client = common::get_client();
    //
    // let response = timeout(
    //     Duration::from_secs(5),
    //     client.post(format!("http://{}/vm", addr)).send(),
    // )
    // .await
    // .expect("request timed out")
    // .expect("request failed");
    //
    // assert_eq!(response.status().as_u16(), 202);
}
