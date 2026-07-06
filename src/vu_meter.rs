#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelFrame {
    pub rms_left: u8,
    pub peak_left: u8,
    pub rms_right: u8,
    pub peak_right: u8,
    pub clip_left: bool,
    pub clip_right: bool,
    pub channels: u8,
}

impl LevelFrame {
    pub fn level(&self) -> u8 {
        self.rms_left.max(self.rms_right)
    }
}

pub fn parse_level_frame(bytes: &[u8; 6]) -> LevelFrame {
    LevelFrame {
        rms_left: bytes[0],
        peak_left: bytes[1],
        rms_right: bytes[2],
        peak_right: bytes[3],
        clip_left: bytes[4] & 0b01 != 0,
        clip_right: bytes[4] & 0b10 != 0,
        channels: bytes[5],
    }
}

use crate::config::VuMeterConfig;
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerState {
    Playing,
    Stopped,
}

pub struct PlaybackStateMachine {
    current: PlayerState,
    start_threshold: u8,
    stop_threshold: u8,
    start_debounce: Duration,
    stop_debounce: Duration,
    above_since: Option<Instant>,
    below_since: Option<Instant>,
}

impl PlaybackStateMachine {
    pub fn new(cfg: &VuMeterConfig) -> Self {
        PlaybackStateMachine {
            current: PlayerState::Stopped,
            start_threshold: cfg.start_threshold,
            stop_threshold: cfg.stop_threshold,
            start_debounce: Duration::from_secs(cfg.start_debounce_secs),
            stop_debounce: Duration::from_secs(cfg.stop_debounce_secs),
            above_since: None,
            below_since: None,
        }
    }

    pub fn current(&self) -> PlayerState {
        self.current
    }

    pub fn on_level(&mut self, level: u8, now: Instant) -> Option<PlayerState> {
        match self.current {
            PlayerState::Stopped => {
                if level > self.start_threshold {
                    let since = *self.above_since.get_or_insert(now);
                    if now.duration_since(since) >= self.start_debounce {
                        self.current = PlayerState::Playing;
                        self.above_since = None;
                        return Some(PlayerState::Playing);
                    }
                } else {
                    self.above_since = None;
                }
                None
            }
            PlayerState::Playing => {
                if level <= self.stop_threshold {
                    let since = *self.below_since.get_or_insert(now);
                    if now.duration_since(since) >= self.stop_debounce {
                        self.current = PlayerState::Stopped;
                        self.below_since = None;
                        return Some(PlayerState::Stopped);
                    }
                } else {
                    self.below_since = None;
                }
                None
            }
        }
    }
}

use crate::audiocontrol::AudioControlClient;
use futures_util::StreamExt;
use std::sync::Arc;
use tokio::sync::Notify;

pub async fn run_state_task(
    cfg: &VuMeterConfig,
    client: &AudioControlClient,
    song_reset: &Arc<Notify>,
) -> ! {
    loop {
        if let Err(e) = connect_and_run(cfg, client, song_reset).await {
            log::warn!("vu-meter connection lost: {e}");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

pub async fn connect_and_run(
    cfg: &VuMeterConfig,
    client: &AudioControlClient,
    song_reset: &Arc<Notify>,
) -> anyhow::Result<()> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(&cfg.ws_url).await?;
    let (_, mut read) = ws_stream.split();
    let mut sm = PlaybackStateMachine::new(cfg);

    while let Some(msg) = read.next().await {
        let msg = msg?;
        if let Message::Binary(data) = msg {
            if data.len() >= 6 {
                let mut arr = [0u8; 6];
                arr.copy_from_slice(&data[0..6]);
                let frame = parse_level_frame(&arr);
                if let Some(new_state) = sm.on_level(frame.level(), Instant::now()) {
                    if let Err(e) = client.send_state_changed(new_state).await {
                        log::warn!("failed to send state_changed: {e}");
                    }
                    if new_state == PlayerState::Stopped {
                        if let Err(e) = client.send_song_cleared().await {
                            log::warn!("failed to clear song: {e}");
                        }
                        // Tell the recognition task to forget its last-seen
                        // title: the display is now blank, so the next
                        // match should be re-announced even if it's the
                        // same song that was playing before (e.g. a brief
                        // pause-and-resume, or a quiet passage that dipped
                        // below the level threshold).
                        song_reset.notify_one();
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frame_fields() {
        // L rms=100 peak=120, R rms=80 peak=90, flags=0b01 (left clip), channels=2
        let bytes: [u8; 6] = [100, 120, 80, 90, 0b01, 2];
        let frame = parse_level_frame(&bytes);
        assert_eq!(frame.rms_left, 100);
        assert_eq!(frame.peak_left, 120);
        assert_eq!(frame.rms_right, 80);
        assert_eq!(frame.peak_right, 90);
        assert!(frame.clip_left);
        assert!(!frame.clip_right);
        assert_eq!(frame.channels, 2);
    }

    #[test]
    fn level_is_max_of_left_and_right_rms() {
        let bytes: [u8; 6] = [30, 0, 90, 0, 0, 2];
        let frame = parse_level_frame(&bytes);
        assert_eq!(frame.level(), 90);
    }

    #[test]
    fn all_zero_frame_is_silence() {
        let bytes: [u8; 6] = [0, 0, 0, 0, 0, 2];
        let frame = parse_level_frame(&bytes);
        assert_eq!(frame.level(), 0);
    }

    fn test_cfg() -> VuMeterConfig {
        VuMeterConfig {
            ws_url: "ws://unused".to_string(),
            start_threshold: 40,
            stop_threshold: 40,
            start_debounce_secs: 1,
            stop_debounce_secs: 20,
        }
    }

    #[test]
    fn starts_stopped() {
        let sm = PlaybackStateMachine::new(&test_cfg());
        assert_eq!(sm.current(), PlayerState::Stopped);
    }

    #[test]
    fn transitions_to_playing_after_start_debounce() {
        let mut sm = PlaybackStateMachine::new(&test_cfg());
        let t0 = Instant::now();
        // Level above threshold, but debounce not elapsed yet
        assert_eq!(sm.on_level(90, t0), None);
        assert_eq!(sm.on_level(90, t0 + Duration::from_millis(500)), None);
        // Debounce elapsed (>= 1s since level first rose)
        assert_eq!(sm.on_level(90, t0 + Duration::from_millis(1001)), Some(PlayerState::Playing));
        assert_eq!(sm.current(), PlayerState::Playing);
    }

    #[test]
    fn brief_dip_does_not_flap_to_stopped() {
        let mut sm = PlaybackStateMachine::new(&test_cfg());
        let t0 = Instant::now();
        sm.on_level(90, t0);
        sm.on_level(90, t0 + Duration::from_secs(2)); // now Playing
        assert_eq!(sm.current(), PlayerState::Playing);

        // Level dips below threshold for less than stop_debounce_secs (20s)
        assert_eq!(sm.on_level(5, t0 + Duration::from_secs(3)), None);
        assert_eq!(sm.on_level(5, t0 + Duration::from_secs(10)), None);
        assert_eq!(sm.current(), PlayerState::Playing);

        // Level recovers before debounce elapses: no Stopped transition should ever fire
        assert_eq!(sm.on_level(90, t0 + Duration::from_secs(11)), None);
        assert_eq!(sm.current(), PlayerState::Playing);
    }

    #[test]
    fn transitions_to_stopped_after_sustained_silence() {
        let mut sm = PlaybackStateMachine::new(&test_cfg());
        let t0 = Instant::now();
        sm.on_level(90, t0);
        sm.on_level(90, t0 + Duration::from_secs(2)); // now Playing

        sm.on_level(5, t0 + Duration::from_secs(3)); // dip starts
        assert_eq!(
            sm.on_level(5, t0 + Duration::from_secs(24)), // 21s after dip started >= 20s debounce
            Some(PlayerState::Stopped)
        );
        assert_eq!(sm.current(), PlayerState::Stopped);
    }

    #[test]
    fn stop_debounce_timer_restarts_on_reversal_before_it_elapses() {
        let mut sm = PlaybackStateMachine::new(&test_cfg());
        let t0 = Instant::now();
        sm.on_level(90, t0);
        sm.on_level(90, t0 + Duration::from_secs(2)); // now Playing
        assert_eq!(sm.current(), PlayerState::Playing);

        // Original dip starts at t0+3s.
        assert_eq!(sm.on_level(5, t0 + Duration::from_secs(3)), None);

        // Recovers above stop_threshold at t0+15s, well before the 20s stop_debounce
        // for the original dip would elapse (t0+23s). This must cancel the timer.
        assert_eq!(sm.on_level(90, t0 + Duration::from_secs(15)), None);
        assert_eq!(sm.current(), PlayerState::Playing);

        // Second dip starts at t0+16s.
        assert_eq!(sm.on_level(5, t0 + Duration::from_secs(16)), None);
        assert_eq!(sm.current(), PlayerState::Playing);

        // t0+35s is 19s after the SECOND dip (< 20s debounce, so still Playing) but
        // 32s after the ORIGINAL dip (>= 20s debounce). If below_since were not reset
        // during the t0+15s recovery, the stale t0+3s timestamp would have caused a
        // Stopped transition here already. Correct behavior: still None/Playing.
        assert_eq!(sm.on_level(5, t0 + Duration::from_secs(35)), None);
        assert_eq!(sm.current(), PlayerState::Playing);

        // t0+37s is 21s after the second dip (>= 20s debounce): now it should fire.
        assert_eq!(
            sm.on_level(5, t0 + Duration::from_secs(37)),
            Some(PlayerState::Stopped)
        );
        assert_eq!(sm.current(), PlayerState::Stopped);
    }

    #[test]
    fn start_debounce_timer_restarts_on_reversal_before_it_elapses() {
        let mut sm = PlaybackStateMachine::new(&test_cfg());
        let t0 = Instant::now();
        assert_eq!(sm.current(), PlayerState::Stopped);

        // Original rise starts at t0.
        assert_eq!(sm.on_level(90, t0), None);

        // Falls back below start_threshold at t0+300ms, well before the 1s
        // start_debounce for the original rise would elapse (t0+1000ms). This must
        // cancel the timer.
        assert_eq!(sm.on_level(5, t0 + Duration::from_millis(300)), None);
        assert_eq!(sm.current(), PlayerState::Stopped);

        // Second rise starts at t0+400ms.
        assert_eq!(sm.on_level(90, t0 + Duration::from_millis(400)), None);
        assert_eq!(sm.current(), PlayerState::Stopped);

        // t0+1000ms is 600ms after the SECOND rise (< 1s debounce, so still Stopped)
        // but 1000ms after the ORIGINAL rise (>= 1s debounce). If above_since were
        // not reset during the t0+300ms reversal, the stale t0 timestamp would have
        // caused a Playing transition here already. Correct behavior: still None/Stopped.
        assert_eq!(sm.on_level(90, t0 + Duration::from_millis(1000)), None);
        assert_eq!(sm.current(), PlayerState::Stopped);

        // t0+1401ms is 1001ms after the second rise (>= 1s debounce): now it should fire.
        assert_eq!(
            sm.on_level(90, t0 + Duration::from_millis(1401)),
            Some(PlayerState::Playing)
        );
        assert_eq!(sm.current(), PlayerState::Playing);
    }

    use crate::audiocontrol::AudioControlClient;
    use futures_util::SinkExt;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn reports_playing_then_stopped_from_real_frames() {
        let ac_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .and(body_json(serde_json::json!({ "type": "state_changed", "state": "playing" })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&ac_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .and(body_json(serde_json::json!({ "type": "state_changed", "state": "stopped" })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&ac_server)
            .await;
        let client = AudioControlClient::new(ac_server.uri(), "analog".to_string());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // Fast debounces so the test runs in well under a second of wall-clock time.
        let cfg = VuMeterConfig {
            ws_url: format!("ws://{addr}"),
            start_threshold: 40,
            stop_threshold: 40,
            start_debounce_secs: 0, // any level above threshold flips immediately
            stop_debounce_secs: 0,
        };

        let server_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            // Loud frame -> Playing
            ws.send(Message::Binary(vec![90, 0, 90, 0, 0, 2])).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            // Silent frame -> Stopped
            ws.send(Message::Binary(vec![0, 0, 0, 0, 0, 2])).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });

        let song_reset = std::sync::Arc::new(tokio::sync::Notify::new());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            connect_and_run(&cfg, &client, &song_reset),
        )
        .await;
        assert!(result.is_ok(), "connect_and_run should finish once the server closes");
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn clears_song_when_transitioning_to_stopped() {
        let ac_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .and(body_json(serde_json::json!({ "type": "state_changed", "state": "playing" })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&ac_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .and(body_json(serde_json::json!({ "type": "state_changed", "state": "stopped" })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&ac_server)
            .await;
        // The critical assertion: a song_changed with no "song" field must be
        // sent alongside the Stopped transition, clearing AudioControl's
        // last-displayed track rather than leaving it to linger indefinitely
        // (AudioControl's own active-player tracking never does this itself).
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .and(body_json(serde_json::json!({ "type": "song_changed" })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&ac_server)
            .await;
        let client = AudioControlClient::new(ac_server.uri(), "analog".to_string());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cfg = VuMeterConfig {
            ws_url: format!("ws://{addr}"),
            start_threshold: 40,
            stop_threshold: 40,
            start_debounce_secs: 0,
            stop_debounce_secs: 0,
        };

        let server_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            ws.send(Message::Binary(vec![90, 0, 90, 0, 0, 2])).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            ws.send(Message::Binary(vec![0, 0, 0, 0, 0, 2])).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });

        let song_reset = std::sync::Arc::new(tokio::sync::Notify::new());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            connect_and_run(&cfg, &client, &song_reset),
        )
        .await;
        assert!(result.is_ok(), "connect_and_run should finish once the server closes");
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn notifies_song_reset_when_transitioning_to_stopped() {
        let ac_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&ac_server)
            .await;
        let client = AudioControlClient::new(ac_server.uri(), "analog".to_string());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cfg = VuMeterConfig {
            ws_url: format!("ws://{addr}"),
            start_threshold: 40,
            stop_threshold: 40,
            start_debounce_secs: 0,
            stop_debounce_secs: 0,
        };
        let song_reset = std::sync::Arc::new(tokio::sync::Notify::new());

        let server_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            ws.send(Message::Binary(vec![90, 0, 90, 0, 0, 2])).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            ws.send(Message::Binary(vec![0, 0, 0, 0, 0, 2])).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });

        let notified = song_reset.notified();
        tokio::pin!(notified);
        let run = connect_and_run(&cfg, &client, &song_reset);
        tokio::pin!(run);

        let saw_notification = tokio::select! {
            _ = &mut notified => true,
            _ = &mut run => false,
        };
        assert!(saw_notification, "expected a song-reset notification on the Stopped transition");

        server_task.await.unwrap();
    }
}
