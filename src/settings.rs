use serde::Deserialize;

/// Reads plugin settings from the configurator ConfigDB over HTTP.
pub struct SettingsClient {
    http: reqwest::Client,
    base_url: String,
    songrec_enabled_key: String,
    threshold_key: String,
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
    pub fn new(base_url: String, songrec_enabled_key: String, threshold_key: String) -> Self {
        // A short timeout so a wedged configurator cannot stall the caller's
        // loop (the state task awaits this between level ticks). On timeout the
        // fetch just fails and the caller keeps its current value.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        SettingsClient {
            http,
            base_url,
            songrec_enabled_key,
            threshold_key,
        }
    }

    fn key_url(&self, key: &str) -> String {
        format!("{}/key/{}", self.base_url.trim_end_matches('/'), key)
    }

    /// Raw string value of a ConfigDB key, or `None` if unset (404) or on any
    /// transport/parse error.
    async fn fetch_value(&self, key: &str) -> Option<String> {
        let resp = self.http.get(self.key_url(key)).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: KeyResponse = resp.json().await.ok()?;
        body.data?.value
    }

    /// Whether songrec recognition is enabled. Fail-open: any error, a missing
    /// key (404), or an unparseable response yields `true` (recognition on).
    pub async fn songrec_enabled(&self) -> bool {
        match self.fetch_value(&self.songrec_enabled_key).await {
            Some(v) => matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "true" | "1" | "yes" | "on"
            ),
            None => true,
        }
    }

    /// The Web-UI activation level in dBFS, or `None` when unset or unparseable
    /// (the caller then keeps its configured default).
    pub async fn activation_dbfs(&self) -> Option<f64> {
        self.fetch_value(&self.threshold_key)
            .await?
            .trim()
            .parse::<f64>()
            .ok()
    }
}
