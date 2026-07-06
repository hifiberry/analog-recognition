use crate::audiocontrol::AudioControlClient;
use crate::config::SongrecConfig;
use crate::settings::SettingsClient;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Notify;

#[derive(Debug, Clone, PartialEq)]
pub struct RecognizedTrack {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub genre: Option<String>,
}

/// The placeholder track published when songrec recognition is disabled.
pub fn unknown_track() -> RecognizedTrack {
    RecognizedTrack {
        title: "Unknown song".to_string(),
        artist: "Unknown artist".to_string(),
        album: None,
        genre: None,
    }
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

    /// Forgets the last-seen title, so the next match is treated as new even
    /// if it's the same title as before. Used when AudioControl's displayed
    /// song has been cleared out-of-band (e.g. by a Stopped transition), so
    /// a still-ongoing (or resumed) track gets re-announced instead of
    /// staying suppressed by a title the display no longer reflects.
    pub fn reset(&mut self) {
        self.last_title = None;
    }
}

pub async fn run_recognition_task(
    cfg: &SongrecConfig,
    settings: &SettingsClient,
    client: &AudioControlClient,
    song_reset: &Arc<Notify>,
    poll: Duration,
) -> ! {
    let mut backoff = Duration::from_secs(1);
    loop {
        if !settings.songrec_enabled().await {
            // Disabled: publish the placeholder track and poll until re-enabled.
            if let Err(e) = client.send_song_changed(&unknown_track()).await {
                log::warn!("failed to publish unknown track: {e}");
            }
            tokio::time::sleep(poll).await;
            continue;
        }
        tokio::select! {
            result = run_songrec_once(cfg, client, song_reset) => {
                match result {
                    Ok(()) => log::warn!("songrec exited, restarting"),
                    Err(e) => log::warn!("songrec failed: {e}, restarting"),
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
            _ = wait_until_disabled(settings, poll) => {
                // The run_songrec_once future is dropped here; its child is
                // killed via kill_on_drop. Loop re-checks and enters the
                // disabled branch above.
                log::info!("songrec disabled via setting, stopping recognition");
                backoff = Duration::from_secs(1);
            }
        }
    }
}

/// Resolves once the songrec_enabled setting reads false.
async fn wait_until_disabled(settings: &SettingsClient, poll: Duration) {
    let mut interval = tokio::time::interval(poll);
    interval.tick().await; // consume the immediate first tick
    loop {
        interval.tick().await;
        if !settings.songrec_enabled().await {
            return;
        }
    }
}

pub async fn run_songrec_once(
    cfg: &SongrecConfig,
    client: &AudioControlClient,
    song_reset: &Arc<Notify>,
) -> anyhow::Result<()> {
    let mut child = Command::new(&cfg.binary)
        .arg("listen")
        .arg("-d")
        .arg(&cfg.device)
        .arg("--json")
        .arg("--disable-mpris")
        .arg("-i")
        .arg(cfg.request_interval_secs.to_string())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("songrec child has no stdout"))?;
    let mut lines = BufReader::new(stdout).lines();
    let mut dedup = Deduper::new();

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { break };
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
            _ = song_reset.notified() => {
                log::debug!("song display was cleared externally, resetting dedup state");
                dedup.reset();
            }
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
        let song_reset = std::sync::Arc::new(tokio::sync::Notify::new());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_songrec_once(&fixture_cfg(), &client, &song_reset),
        )
        .await;
        assert!(result.is_ok(), "run_songrec_once should return once the fixture script exits");
        result.unwrap().unwrap();
    }

    #[tokio::test]
    async fn reemits_same_title_after_a_reset_notification() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .and(body_json(serde_json::json!({
                "type": "song_changed",
                "song": { "title": "Song A", "artist": "Artist A" }
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(2) // once before the reset, once again after it
            .mount(&server)
            .await;

        let client = AudioControlClient::new(server.uri(), "analog".to_string());
        let cfg = SongrecConfig {
            device: "unused".to_string(),
            request_interval_secs: 10,
            binary: concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/fake_songrec_repeat_slow.sh"
            )
            .to_string(),
        };
        let song_reset = std::sync::Arc::new(tokio::sync::Notify::new());

        let reset_trigger = song_reset.clone();
        tokio::spawn(async move {
            // Fires between the fixture's 2nd and 3rd identical print
            // (which are 0.2s apart), so only the 3rd print should be
            // re-emitted as "new".
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            reset_trigger.notify_one();
        });

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_songrec_once(&cfg, &client, &song_reset),
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
        let song_reset = std::sync::Arc::new(tokio::sync::Notify::new());

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_songrec_once(&fixture_cfg(), &client, &song_reset),
        )
        .await;
        assert!(result.is_ok(), "run_songrec_once should return once the fixture script exits");
        result.unwrap().unwrap();

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_songrec_once(&fixture_cfg(), &client, &song_reset),
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

    #[test]
    fn reset_allows_the_same_title_to_be_re_emitted() {
        let mut d = Deduper::new();
        assert!(d.should_emit(&track("A")));
        assert!(!d.should_emit(&track("A")));
        d.reset();
        assert!(d.should_emit(&track("A"))); // same title as before reset, but now a "new" match
    }

    #[test]
    fn unknown_track_has_placeholder_fields() {
        let t = unknown_track();
        assert_eq!(t.artist, "Unknown artist");
        assert_eq!(t.title, "Unknown song");
        assert_eq!(t.album, None);
    }
}
