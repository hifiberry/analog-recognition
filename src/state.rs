//! The playback detection state machine: turns a stream of 0-255 levels into
//! Playing/Stopped transitions, with independent start/stop thresholds and
//! debounce so a transient does not flap the reported state.
//!
//! Pure logic, no I/O — the level source (`input_level`) and the AudioControl
//! reporting are layered on top.

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

    /// Feed one level sample taken at `now`. Returns `Some(state)` only on a
    /// transition, `None` while unchanged (including during debounce).
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

    fn test_cfg() -> VuMeterConfig {
        VuMeterConfig {
            capture_target: "input-processor".to_string(),
            start_threshold: 40,
            stop_threshold: 40,
            start_debounce_secs: 1,
            stop_debounce_secs: 20,
            ws_url: None,
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
        assert_eq!(sm.on_level(90, t0), None);
        assert_eq!(sm.on_level(90, t0 + Duration::from_millis(500)), None);
        assert_eq!(
            sm.on_level(90, t0 + Duration::from_millis(1001)),
            Some(PlayerState::Playing)
        );
        assert_eq!(sm.current(), PlayerState::Playing);
    }

    #[test]
    fn brief_dip_does_not_flap_to_stopped() {
        let mut sm = PlaybackStateMachine::new(&test_cfg());
        let t0 = Instant::now();
        sm.on_level(90, t0);
        sm.on_level(90, t0 + Duration::from_secs(2));
        assert_eq!(sm.current(), PlayerState::Playing);
        assert_eq!(sm.on_level(5, t0 + Duration::from_secs(3)), None);
        assert_eq!(sm.on_level(5, t0 + Duration::from_secs(10)), None);
        assert_eq!(sm.on_level(90, t0 + Duration::from_secs(11)), None);
        assert_eq!(sm.current(), PlayerState::Playing);
    }

    #[test]
    fn transitions_to_stopped_after_sustained_silence() {
        let mut sm = PlaybackStateMachine::new(&test_cfg());
        let t0 = Instant::now();
        sm.on_level(90, t0);
        sm.on_level(90, t0 + Duration::from_secs(2));
        sm.on_level(5, t0 + Duration::from_secs(3));
        assert_eq!(
            sm.on_level(5, t0 + Duration::from_secs(24)),
            Some(PlayerState::Stopped)
        );
        assert_eq!(sm.current(), PlayerState::Stopped);
    }

    #[test]
    fn stop_debounce_timer_restarts_on_reversal_before_it_elapses() {
        let mut sm = PlaybackStateMachine::new(&test_cfg());
        let t0 = Instant::now();
        sm.on_level(90, t0);
        sm.on_level(90, t0 + Duration::from_secs(2));
        assert_eq!(sm.on_level(5, t0 + Duration::from_secs(3)), None);
        assert_eq!(sm.on_level(90, t0 + Duration::from_secs(15)), None);
        assert_eq!(sm.on_level(5, t0 + Duration::from_secs(16)), None);
        assert_eq!(sm.on_level(5, t0 + Duration::from_secs(35)), None);
        assert_eq!(
            sm.on_level(5, t0 + Duration::from_secs(37)),
            Some(PlayerState::Stopped)
        );
    }

    #[test]
    fn start_debounce_timer_restarts_on_reversal_before_it_elapses() {
        let mut sm = PlaybackStateMachine::new(&test_cfg());
        let t0 = Instant::now();
        assert_eq!(sm.on_level(90, t0), None);
        assert_eq!(sm.on_level(5, t0 + Duration::from_millis(300)), None);
        assert_eq!(sm.on_level(90, t0 + Duration::from_millis(400)), None);
        assert_eq!(sm.on_level(90, t0 + Duration::from_millis(1000)), None);
        assert_eq!(
            sm.on_level(90, t0 + Duration::from_millis(1401)),
            Some(PlayerState::Playing)
        );
        assert_eq!(sm.current(), PlayerState::Playing);
    }
}
