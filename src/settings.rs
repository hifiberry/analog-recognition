use serde::Deserialize;

/// Reads plugin settings from the configurator ConfigDB over HTTP.
pub struct SettingsClient {
    http: reqwest::Client,
    base_url: String,
    songrec_enabled_key: String,
}

#[derive(Deserialize)]
struct KeyResponse {
    data: Option<KeyData>,
}

#[derive(Deserialize)]
struct KeyData {
    value: Option<String>,
}

impl SettingsClient {
    pub fn new(base_url: String, songrec_enabled_key: String) -> Self {
        SettingsClient {
            http: reqwest::Client::new(),
            base_url,
            songrec_enabled_key,
        }
    }

    fn key_url(&self) -> String {
        format!(
            "{}/key/{}",
            self.base_url.trim_end_matches('/'),
            self.songrec_enabled_key
        )
    }

    /// Whether songrec recognition is enabled. Fail-open: any error, a missing
    /// key (404), or an unparseable response yields `true` (recognition on).
    pub async fn songrec_enabled(&self) -> bool {
        self.fetch_flag().await.unwrap_or(true)
    }

    async fn fetch_flag(&self) -> Option<bool> {
        let resp = self.http.get(self.key_url()).send().await.ok()?;
        if !resp.status().is_success() {
            return None; // 404 (unset) -> default true
        }
        let body: KeyResponse = resp.json().await.ok()?;
        let value = body.data?.value?;
        Some(matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        ))
    }
}
