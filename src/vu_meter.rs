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
}
