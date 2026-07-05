use crate::songrec::RecognizedTrack;
use crate::vu_meter::PlayerState;

pub struct AudioControlClient {
    http: reqwest::Client,
    base_url: String,
    player_name: String,
}

impl AudioControlClient {
    pub fn new(base_url: String, player_name: String) -> Self {
        AudioControlClient {
            http: reqwest::Client::new(),
            base_url,
            player_name,
        }
    }

    fn update_url(&self) -> String {
        format!(
            "{}/player/{}/update",
            self.base_url.trim_end_matches('/'),
            self.player_name
        )
    }

    pub async fn send_state_changed(&self, state: PlayerState) -> anyhow::Result<()> {
        let state_str = match state {
            PlayerState::Playing => "playing",
            PlayerState::Stopped => "stopped",
        };
        let body = serde_json::json!({ "type": "state_changed", "state": state_str });
        self.post_update(&body).await
    }

    pub async fn send_song_changed(&self, track: &RecognizedTrack) -> anyhow::Result<()> {
        let mut song = serde_json::json!({
            "title": track.title,
            "artist": track.artist,
        });
        if let Some(album) = &track.album {
            song["album"] = serde_json::json!(album);
        }
        let body = serde_json::json!({ "type": "song_changed", "song": song });
        self.post_update(&body).await
    }

    async fn post_update(&self, body: &serde_json::Value) -> anyhow::Result<()> {
        let resp = self.http.post(self.update_url()).json(body).send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("AudioControl returned status {}", resp.status());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::songrec::RecognizedTrack;
    use crate::vu_meter::PlayerState;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn sends_state_changed_playing() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .and(body_json(serde_json::json!({
                "type": "state_changed",
                "state": "playing"
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = AudioControlClient::new(server.uri(), "analog".to_string());
        client.send_state_changed(PlayerState::Playing).await.unwrap();
    }

    #[tokio::test]
    async fn sends_song_changed_with_album() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .and(body_json(serde_json::json!({
                "type": "song_changed",
                "song": {
                    "title": "Headlights",
                    "artist": "Sophie Hunger",
                    "album": "1983"
                }
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = AudioControlClient::new(server.uri(), "analog".to_string());
        let track = RecognizedTrack {
            title: "Headlights".to_string(),
            artist: "Sophie Hunger".to_string(),
            album: Some("1983".to_string()),
            genre: None,
        };
        client.send_song_changed(&track).await.unwrap();
    }

    #[tokio::test]
    async fn sends_song_changed_without_album() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .and(body_json(serde_json::json!({
                "type": "song_changed",
                "song": {
                    "title": "Song",
                    "artist": "Artist"
                }
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = AudioControlClient::new(server.uri(), "analog".to_string());
        let track = RecognizedTrack {
            title: "Song".to_string(),
            artist: "Artist".to_string(),
            album: None,
            genre: None,
        };
        client.send_song_changed(&track).await.unwrap();
    }

    #[tokio::test]
    async fn non_success_status_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = AudioControlClient::new(server.uri(), "analog".to_string());
        let result = client.send_state_changed(PlayerState::Stopped).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn connection_failure_is_an_error_not_a_panic() {
        // Port 1 is reserved and nothing listens there.
        let client = AudioControlClient::new("http://127.0.0.1:1".to_string(), "analog".to_string());
        let result = client.send_state_changed(PlayerState::Stopped).await;
        assert!(result.is_err());
    }
}
