use analog_recognition::settings::SettingsClient;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = "player.analog-recognition.songrec_enabled";

fn client(base: String) -> SettingsClient {
    SettingsClient::new(base, KEY.to_string())
}

#[tokio::test]
async fn false_when_stored_false() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/key/{KEY}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "success", "data": { "key": KEY, "value": "false" }
        })))
        .mount(&server)
        .await;
    assert!(!(client(server.uri()).songrec_enabled().await));
}

#[tokio::test]
async fn true_when_stored_true() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/key/{KEY}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "success", "data": { "key": KEY, "value": "true" }
        })))
        .mount(&server)
        .await;
    assert!(client(server.uri()).songrec_enabled().await);
}

#[tokio::test]
async fn default_true_when_key_absent_404() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/key/{KEY}")))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    assert!(client(server.uri()).songrec_enabled().await);
}

#[tokio::test]
async fn default_true_on_connection_failure() {
    // Port 1 is reserved; nothing listens.
    assert!(
        client("http://127.0.0.1:1".to_string())
            .songrec_enabled()
            .await
    );
}
