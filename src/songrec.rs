use crate::audiocontrol::AudioControlClient;
use crate::config::SongrecConfig;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq)]
pub struct RecognizedTrack {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub genre: Option<String>,
}

pub fn parse_songrec_line(line: &str) -> Option<RecognizedTrack> {
    let trimmed = line.trim();
    if !trimmed.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let track = value.get("track")?;
    let title = track.get("title")?.as_str()?.to_string();
    let artist = track.get("subtitle")?.as_str()?.to_string();

    let genre = track
        .get("genres")
        .and_then(|g| g.get("primary"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let album = track
        .get("sections")
        .and_then(|s| s.as_array())
        .and_then(|sections| {
            sections.iter().find_map(|section| {
                section.get("metadata")?.as_array()?.iter().find_map(|entry| {
                    if entry.get("title")?.as_str()? == "Album" {
                        entry.get("text")?.as_str().map(|s| s.to_string())
                    } else {
                        None
                    }
                })
            })
        });

    Some(RecognizedTrack { title, artist, album, genre })
}

pub struct Deduper {
    last_title: Option<String>,
}

impl Deduper {
    pub fn new() -> Self {
        Deduper { last_title: None }
    }

    pub fn should_emit(&mut self, track: &RecognizedTrack) -> bool {
        if self.last_title.as_deref() == Some(track.title.as_str()) {
            false
        } else {
            self.last_title = Some(track.title.clone());
            true
        }
    }
}

pub async fn run_recognition_task(cfg: &SongrecConfig, client: &AudioControlClient) -> ! {
    let mut backoff = std::time::Duration::from_secs(1);
    loop {
        match run_songrec_once(cfg, client).await {
            Ok(()) => log::warn!("songrec exited, restarting"),
            Err(e) => log::warn!("songrec failed: {e}, restarting"),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(std::time::Duration::from_secs(30));
    }
}

pub async fn run_songrec_once(cfg: &SongrecConfig, client: &AudioControlClient) -> anyhow::Result<()> {
    let mut child = Command::new(&cfg.binary)
        .arg("listen")
        .arg("-d")
        .arg(&cfg.device)
        .arg("--json")
        .arg("--disable-mpris")
        .arg("-i")
        .arg(cfg.request_interval_secs.to_string())
        .stdout(std::process::Stdio::piped())
        .spawn()?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("songrec child has no stdout"))?;
    let mut lines = BufReader::new(stdout).lines();
    let mut dedup = Deduper::new();

    while let Some(line) = lines.next_line().await? {
        if let Some(track) = parse_songrec_line(&line) {
            if dedup.should_emit(&track) {
                if let Err(e) = client.send_song_changed(&track).await {
                    log::warn!("failed to send song_changed: {e}");
                }
            }
        } else {
            log::debug!("songrec: {line}");
        }
    }

    child.wait().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audiocontrol::AudioControlClient;
    use crate::config::SongrecConfig;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fixture_cfg() -> SongrecConfig {
        SongrecConfig {
            device: "unused".to_string(),
            request_interval_secs: 10,
            binary: concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake_songrec.sh")
                .to_string(),
        }
    }

    #[tokio::test]
    async fn emits_song_changed_once_per_distinct_title() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .and(body_json(serde_json::json!({
                "type": "song_changed",
                "song": { "title": "Song A", "artist": "Artist A" }
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1) // NOT 2, even though the fixture prints "Song A" twice
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .and(body_json(serde_json::json!({
                "type": "song_changed",
                "song": { "title": "Song B", "artist": "Artist B" }
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = AudioControlClient::new(server.uri(), "analog".to_string());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_songrec_once(&fixture_cfg(), &client),
        )
        .await;
        assert!(result.is_ok(), "run_songrec_once should return once the fixture script exits");
        result.unwrap().unwrap();
    }

    #[tokio::test]
    async fn can_be_run_again_after_the_child_exits() {
        // This is the property run_recognition_task's restart loop depends on.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let client = AudioControlClient::new(server.uri(), "analog".to_string());

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_songrec_once(&fixture_cfg(), &client),
        )
        .await;
        assert!(result.is_ok(), "run_songrec_once should return once the fixture script exits");
        result.unwrap().unwrap();

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_songrec_once(&fixture_cfg(), &client),
        )
        .await;
        assert!(result.is_ok(), "run_songrec_once should return once the fixture script exits");
        result.unwrap().unwrap();
    }

    fn track(title: &str) -> RecognizedTrack {
        RecognizedTrack {
            title: title.to_string(),
            artist: "Artist".to_string(),
            album: None,
            genre: None,
        }
    }

    // Trimmed but structurally real shape, based on an actual songrec match
    // captured against the live analog input (Sophie Hunger - Headlights).
    const REAL_MATCH: &str = r#"{
        "track": {
            "title": "Headlights",
            "subtitle": "Sophie Hunger",
            "genres": { "primary": "Rock" },
            "sections": [
                {
                    "type": "SONG",
                    "metadata": [
                        { "title": "Album", "text": "1983" },
                        { "title": "Label", "text": "Two Gentlemen Records" },
                        { "title": "Released", "text": "2010" }
                    ]
                }
            ]
        }
    }"#;

    const MATCH_WITHOUT_OPTIONALS: &str = r#"{
        "track": {
            "title": "Some Song",
            "subtitle": "Some Artist"
        }
    }"#;

    #[test]
    fn parses_full_match() {
        let track = parse_songrec_line(REAL_MATCH).unwrap();
        assert_eq!(track.title, "Headlights");
        assert_eq!(track.artist, "Sophie Hunger");
        assert_eq!(track.genre.as_deref(), Some("Rock"));
        assert_eq!(track.album.as_deref(), Some("1983"));
    }

    #[test]
    fn parses_match_missing_optional_fields() {
        let track = parse_songrec_line(MATCH_WITHOUT_OPTIONALS).unwrap();
        assert_eq!(track.title, "Some Song");
        assert_eq!(track.artist, "Some Artist");
        assert_eq!(track.genre, None);
        assert_eq!(track.album, None);
    }

    #[test]
    fn ignores_non_json_log_lines() {
        let line = "[2026-07-05T16:21:39Z INFO songrec::cli_main] Recording started!";
        assert_eq!(parse_songrec_line(line), None);
    }

    #[test]
    fn ignores_malformed_json_without_panicking() {
        assert_eq!(parse_songrec_line("{not valid json"), None);
    }

    #[test]
    fn ignores_json_without_a_track_field() {
        assert_eq!(parse_songrec_line(r#"{"location": {}}"#), None);
    }

    #[test]
    fn first_track_is_always_emitted() {
        let mut d = Deduper::new();
        assert!(d.should_emit(&track("A")));
    }

    #[test]
    fn repeated_same_title_is_not_emitted_again() {
        let mut d = Deduper::new();
        assert!(d.should_emit(&track("A")));
        assert!(!d.should_emit(&track("A")));
        assert!(!d.should_emit(&track("A")));
    }

    #[test]
    fn new_title_is_emitted_after_a_change() {
        let mut d = Deduper::new();
        assert!(d.should_emit(&track("A")));
        assert!(!d.should_emit(&track("A")));
        assert!(d.should_emit(&track("B")));
        assert!(!d.should_emit(&track("B")));
        assert!(d.should_emit(&track("A"))); // back to A after B is a real change too
    }
}
