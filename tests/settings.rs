use analog_recognition::settings::SettingsClient;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = "player.analog-recognition.songrec_enabled";
const THRESHOLD_KEY: &str = "player.analog-recognition.threshold_dbfs";

fn client(base: String) -> SettingsClient {
    SettingsClient::new(base, KEY.to_string(), THRESHOLD_KEY.to_string())
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

#[tokio::test]
async fn activation_dbfs_reads_the_stored_value() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/key/{THRESHOLD_KEY}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "success", "data": { "key": THRESHOLD_KEY, "value": "-42" }
        })))
        .mount(&server)
        .await;
    assert_eq!(client(server.uri()).activation_dbfs().await, Some(-42.0));
}

#[tokio::test]
async fn activation_dbfs_is_none_when_key_absent_404() {
    // Caller then keeps its configured default.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/key/{THRESHOLD_KEY}")))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    assert_eq!(client(server.uri()).activation_dbfs().await, None);
}

#[tokio::test]
async fn activation_dbfs_is_none_when_unparseable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/key/{THRESHOLD_KEY}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "success", "data": { "key": THRESHOLD_KEY, "value": "loud" }
        })))
        .mount(&server)
        .await;
    assert_eq!(client(server.uri()).activation_dbfs().await, None);
}

#[tokio::test]
async fn activation_dbfs_rejects_values_that_are_not_real_numbers() {
    // "nan" and "inf" both parse as f64 without complaint. A NaN threshold
    // compares false against every level, which would silently pin the
    // detector to whatever state it was already in.
    for stored in ["nan", "NaN", "inf", "-inf", "infinity"] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/key/{THRESHOLD_KEY}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "success", "data": { "key": THRESHOLD_KEY, "value": stored }
            })))
            .mount(&server)
            .await;
        assert_eq!(
            client(server.uri()).activation_dbfs().await,
            None,
            "{stored} should be rejected"
        );
    }
}
