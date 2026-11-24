//! Benchmark mode's network switch: once off, no request leaves the machine. A listener
//! stands in for a server and must see no connection at all.

use puzil_zulip::{ApiError, Client, Credentials};

#[tokio::test]
async fn offline_requests_never_connect() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let realm = format!("http://{}", listener.local_addr().unwrap());
    let c = Client::new(Credentials { realm: realm.clone(), email: "a@b".into(), api_key: "k".into(), user_id: Some(1) });

    puzil_zulip::client::set_offline();
    let offline = |r: &Result<_, ApiError>| matches!(r, Err(ApiError::Api { code, .. }) if code == "OFFLINE");
    assert!(offline(&c.get_value("users/me", &[], None).await));
    assert!(offline(&c.post_value("messages", &[("content", "x".into())]).await));
    assert!(offline(&c.public_json(&format!("{realm}/x.json")).await));
    assert!(matches!(c.get_bytes(&format!("{realm}/user_uploads/x")).await, Err(ApiError::Api { code, .. }) if code == "OFFLINE"));
    // Nothing reached the stand-in server.
    assert!(listener.accept().is_err());
}
