use crate::audiocontrol::AudioControlClient;
use crate::config::SongrecConfig;
use crate::settings::SettingsClient;
use crate::stream_watch::{wait_until_stream_lost, StreamCheck};
use crate::vu_meter::PlayerState;
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

/// Decide whether to publish the Unknown placeholder while recognition is
/// disabled. `playing` = VU reports playback; `already_published` = we already
/// published Unknown for the current playing stretch. Returns
/// (should_publish_now, new_already_published_flag).
pub fn unknown_publish_decision(playing: bool, already_published: bool) -> (bool, bool) {
    if playing {
        if already_published { (false, true) } else { (true, true) }
    } else {
        (false, false)
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

/// Why `run_songrec_once` returned.
///
/// `Exited` is songrec closing its stdout -- it crashed, was killed, or ended
/// on its own. `StreamLost` is songrec still running but no longer holding a
/// capture stream, which is what a PipeWire restart leaves behind and which no
/// amount of watching the process can detect. Both mean "restart it", but they
/// are logged apart because they point at different causes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Exited,
    StreamLost,
}

const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// A run this long counts as songrec having worked, rather than having failed
/// on startup.
const HEALTHY_RUN: Duration = Duration::from_secs(60);

/// The delay before the next restart attempt.
///
/// The backoff exists for a songrec that dies immediately and would otherwise
/// be respawned in a tight loop, so it keeps doubling for those. A run that
/// lasted was not that, and the watchdog makes those routine: without the
/// reset, a device that has lost its stream a few times sits permanently at
/// the cap, including on the first restart after hours of healthy operation --
/// the case where recovering quickly matters most.
fn next_backoff(current: Duration, ran_for: Duration) -> Duration {
    if ran_for >= HEALTHY_RUN {
        INITIAL_BACKOFF
    } else {
        (current * 2).min(MAX_BACKOFF)
    }
}

/// `run_songrec_once` alongside how long it ran, for the backoff.
async fn run_songrec_once_timed(
    cfg: &SongrecConfig,
    client: &AudioControlClient,
    song_reset: &Arc<Notify>,
) -> (anyhow::Result<Outcome>, Duration) {
    let started = tokio::time::Instant::now();
    let result = run_songrec_once(cfg, client, song_reset).await;
    (result, started.elapsed())
}

pub async fn run_recognition_task(
    cfg: &SongrecConfig,
    settings: &SettingsClient,
    client: &AudioControlClient,
    song_reset: &Arc<Notify>,
    poll: Duration,
    state_rx: &tokio::sync::watch::Receiver<PlayerState>,
) -> ! {
    let mut backoff = INITIAL_BACKOFF;
    let mut unknown_published = false;
    loop {
        if !settings.songrec_enabled().await {
            // Disabled: publish the placeholder track only while the VU
            // meter reports playback, and only once per playing stretch.
            let playing = *state_rx.borrow() == PlayerState::Playing;
            let (should_publish, new_flag) =
                unknown_publish_decision(playing, unknown_published);
            if should_publish {
                if let Err(e) = client.send_song_changed(&unknown_track()).await {
                    log::warn!("failed to publish unknown track: {e}");
                }
            }
            unknown_published = new_flag;
            tokio::time::sleep(poll).await;
            continue;
        }
        unknown_published = false;
        tokio::select! {
            (result, ran_for) = run_songrec_once_timed(cfg, client, song_reset) => {
                match result {
                    Ok(Outcome::Exited) => log::warn!("songrec exited, restarting"),
                    Ok(Outcome::StreamLost) => log::warn!(
                        "songrec is still running but holds no capture stream on {} \
                         -- PipeWire most likely restarted under it; restarting it",
                        cfg.device
                    ),
                    Err(e) => log::warn!("songrec failed: {e}, restarting"),
                }
                tokio::time::sleep(backoff).await;
                backoff = next_backoff(backoff, ran_for);
            }
            _ = wait_until_disabled(settings, poll) => {
                // The run_songrec_once future is dropped here; its child is
                // killed via kill_on_drop. Loop re-checks and enters the
                // disabled branch above.
                log::info!("songrec disabled via setting, stopping recognition");
                backoff = INITIAL_BACKOFF;
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
) -> anyhow::Result<Outcome> {
    run_songrec_once_with(
        cfg,
        client,
        song_reset,
        &StreamCheck::PwDump { binary: cfg.pw_dump_binary.clone() },
    )
    .await
}

/// The body of `run_songrec_once`, with the stream check injected so tests can
/// drive the watchdog without a running PipeWire.
pub async fn run_songrec_once_with(
    cfg: &SongrecConfig,
    client: &AudioControlClient,
    song_reset: &Arc<Notify>,
    stream_check: &StreamCheck,
) -> anyhow::Result<Outcome> {
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

    // A child with no pid has already been reaped; there is nothing to watch,
    // and the stdout loop below will see the pipe close.
    let watched_pid = child.id();
    let stream_watch = async {
        match watched_pid {
            Some(pid) => {
                wait_until_stream_lost(
                    pid,
                    stream_check,
                    Duration::from_secs(cfg.stream_grace_secs),
                    Duration::from_secs(cfg.stream_check_secs),
                )
                .await
            }
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(stream_watch);

    loop {
        tokio::select! {
            _ = &mut stream_watch => {
                // Dropping the child kills it (kill_on_drop), so the caller
                // gets a clean restart rather than a second songrec competing
                // for the same device.
                return Ok(Outcome::StreamLost);
            }
            line = lines.next_line() => {
                let Some(line) = line? else { break };
                if let Some(track) = parse_songrec_line(&line) {
                    if dedup.should_emit(&track) {
                        // A recognized song is itself strong evidence that
                        // playback is active, independent of whether the
                        // VU-meter's level threshold has (yet) reported
                        // Playing - so assert it here too rather than
                        // relying solely on the other task's debounce timing.
                        if let Err(e) = client.send_state_changed(PlayerState::Playing).await {
                            log::warn!("failed to send state_changed: {e}");
                        }
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
    Ok(Outcome::Exited)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audiocontrol::AudioControlClient;
    use crate::config::SongrecConfig;
    use crate::stream_watch::StreamCheck;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fixture_cfg() -> SongrecConfig {
        SongrecConfig {
            device: "unused".to_string(),
            request_interval_secs: 10,
            binary: concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake_songrec.sh")
                .to_string(),
            // The fixtures are shell scripts and never open an audio device,
            // so the watchdog is off unless a test turns it on deliberately.
            stream_check_secs: 0,
            stream_grace_secs: 0,
            pw_dump_binary: "pw-dump".to_string(),
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
    async fn also_sends_playing_state_when_a_song_is_detected() {
        // A recognized song is itself strong evidence that playback is
        // active, independent of whether the VU-meter's level threshold has
        // (yet) flipped its own state to Playing. Each distinct detection
        // should assert Playing, not just publish the song metadata.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .and(body_json(serde_json::json!({
                "type": "state_changed",
                "state": "playing"
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(2) // once for Song A, once for Song B - NOT for the repeated Song A line
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .and(body_json(serde_json::json!({
                "type": "song_changed",
                "song": { "title": "Song A", "artist": "Artist A" }
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
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
            stream_check_secs: 0,
            stream_grace_secs: 0,
            pw_dump_binary: "pw-dump".to_string(),
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

    /// A songrec that starts, prints nothing and never exits: exactly the
    /// state a PipeWire restart leaves behind, and the one the stdout-only
    /// supervision cannot see. The intervals are short because these tests
    /// spend real time -- they run a real child process -- and the property
    /// under test is the decision, not the duration.
    fn silent_forever_cfg() -> SongrecConfig {
        SongrecConfig {
            device: "input-processor.monitor".to_string(),
            request_interval_secs: 10,
            binary: concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/fake_songrec_silent.sh"
            )
            .to_string(),
            stream_check_secs: 0,
            stream_grace_secs: 0,
            pw_dump_binary: "pw-dump".to_string(),
        }
    }

    #[test]
    fn the_backoff_resets_after_a_run_that_lasted() {
        // The watchdog turns restarts into a routine event, so without a reset
        // a device that has lost its stream a few times sits permanently at
        // the 30s cap -- including on the first restart after twenty healthy
        // hours, which is exactly when a fast recovery matters.
        let capped = Duration::from_secs(30);
        assert_eq!(
            next_backoff(capped, Duration::from_secs(60 * 60 * 20)),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn the_backoff_still_grows_while_songrec_keeps_failing_immediately() {
        // A songrec that dies on startup must not be respawned in a tight
        // loop, which is what the backoff was for in the first place.
        assert_eq!(
            next_backoff(Duration::from_secs(1), Duration::from_millis(50)),
            Duration::from_secs(2)
        );
        assert_eq!(
            next_backoff(Duration::from_secs(30), Duration::from_millis(50)),
            Duration::from_secs(30)
        );
    }

    #[tokio::test]
    async fn a_silent_songrec_with_no_stream_is_restarted() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let client = AudioControlClient::new(server.uri(), "analog".to_string());
        let song_reset = std::sync::Arc::new(tokio::sync::Notify::new());
        let mut cfg = silent_forever_cfg();
        cfg.stream_check_secs = 1;

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_songrec_once_with(&cfg, &client, &song_reset, &StreamCheck::Fixed(false)),
        )
        .await
        .expect("the watchdog should have fired rather than blocking forever")
        .unwrap();

        assert_eq!(outcome, Outcome::StreamLost);
    }

    #[tokio::test]
    async fn a_silent_songrec_that_still_holds_its_stream_is_left_alone() {
        // The other half of the property: no output does not by itself mean
        // broken. songrec prints only when it recognises something, so a
        // watchdog that fired on silence would restart it through every
        // unrecognised track.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let client = AudioControlClient::new(server.uri(), "analog".to_string());
        let song_reset = std::sync::Arc::new(tokio::sync::Notify::new());
        let mut cfg = silent_forever_cfg();
        cfg.stream_check_secs = 1;

        // Deliberately brief: this asserts a non-event, and holding the
        // runtime for seconds starves the timing-sensitive tests alongside it.
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(600),
            run_songrec_once_with(&cfg, &client, &song_reset, &StreamCheck::Fixed(true)),
        )
        .await;

        assert!(result.is_err(), "songrec was restarted while it still held its stream");
    }

    #[tokio::test]
    async fn the_watchdog_does_not_preempt_a_songrec_that_is_working() {
        // With the watchdog on and the stream present, the normal path still
        // wins: the fixture's songs are published and the run ends because
        // the child exits, not because the watchdog fired.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let client = AudioControlClient::new(server.uri(), "analog".to_string());
        let song_reset = std::sync::Arc::new(tokio::sync::Notify::new());
        let mut cfg = fixture_cfg();
        cfg.stream_check_secs = 1;

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_songrec_once_with(&cfg, &client, &song_reset, &StreamCheck::Fixed(true)),
        )
        .await
        .expect("the fixture should have exited")
        .unwrap();

        assert_eq!(outcome, Outcome::Exited);
    }

    #[test]
    fn unknown_track_has_placeholder_fields() {
        let t = unknown_track();
        assert_eq!(t.artist, "Unknown artist");
        assert_eq!(t.title, "Unknown song");
        assert_eq!(t.album, None);
    }

    #[test]
    fn unknown_publish_decision_publishes_once_while_playing() {
        assert_eq!(unknown_publish_decision(true, false), (true, true));
    }

    #[test]
    fn unknown_publish_decision_does_not_repeat_while_still_playing() {
        assert_eq!(unknown_publish_decision(true, true), (false, true));
    }

    #[test]
    fn unknown_publish_decision_clears_flag_once_stopped() {
        assert_eq!(unknown_publish_decision(false, true), (false, false));
    }

    #[test]
    fn unknown_publish_decision_stays_quiet_while_stopped() {
        assert_eq!(unknown_publish_decision(false, false), (false, false));
    }
}
