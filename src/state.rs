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
        let mut sm = PlaybackStateMachine::with_thresholds(
            0,
            0,
            Duration::from_secs(cfg.start_debounce_secs),
            Duration::from_secs(cfg.stop_debounce_secs),
        );
        sm.set_activation_dbfs(cfg.threshold_dbfs, cfg.hysteresis_db);
        sm
    }

    /// Build with explicit 0-255 thresholds. `new` derives these from the
    /// configured dBFS; tests use it to exercise the transition logic directly.
    fn with_thresholds(
        start_threshold: u8,
        stop_threshold: u8,
        start_debounce: Duration,
        stop_debounce: Duration,
    ) -> Self {
        PlaybackStateMachine {
            current: PlayerState::Stopped,
            start_threshold,
            stop_threshold,
            start_debounce,
            stop_debounce,
            above_since: None,
            below_since: None,
        }
    }

    /// Set the activation level (dBFS) live — used when the Web UI changes it.
    /// The stop threshold sits `hysteresis_db` below the start threshold so the
    /// state does not flap around the boundary.
    ///
    /// Both arguments come from a config file or the ConfigDB, so neither can
    /// be trusted to be sane. A non-finite level falls back to the default
    /// rather than mapping to an arbitrary threshold, and the hysteresis is
    /// floored at zero: a negative one would put the stop threshold *above*
    /// the start threshold, leaving a band of levels simultaneously "loud
    /// enough to start" and "quiet enough to stop" that would flap the player
    /// once per debounce cycle for as long as the input sat there. With a
    /// non-negative hysteresis that cannot happen, because `db_to_u8` is
    /// monotonic.
    pub fn set_activation_dbfs(&mut self, dbfs: f64, hysteresis_db: f64) {
        let dbfs = if dbfs.is_finite() {
            dbfs
        } else {
            crate::level::DEFAULT_ACTIVATION_DBFS
        };
        let hysteresis = if hysteresis_db.is_finite() {
            hysteresis_db.max(0.0)
        } else {
            0.0
        };
        self.start_threshold = crate::level::db_to_u8(dbfs);
        self.stop_threshold = crate::level::db_to_u8(dbfs - hysteresis);
    }

    /// Record evidence of playback that did not come from the level: a track
    /// was recognized.
    ///
    /// Recognition proves the input is live even when its level sits under the
    /// activation threshold — a quiet pressing, or a threshold set too high —
    /// so it forces Playing and restarts the stop debounce. Routing it through
    /// the state machine rather than reporting it directly keeps this task the
    /// only writer of the player's state; a second writer would leave the two
    /// disagreeing with nothing to reconcile them.
    ///
    /// Returns `Some(Playing)` only when this actually changed the state.
    pub fn note_activity(&mut self) -> Option<PlayerState> {
        self.above_since = None;
        self.below_since = None;
        if self.current == PlayerState::Stopped {
            self.current = PlayerState::Playing;
            Some(PlayerState::Playing)
        } else {
            None
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

    fn sm() -> PlaybackStateMachine {
        // Equal start/stop at 40 keeps these transition tests simple; the
        // dBFS→threshold mapping is covered separately.
        PlaybackStateMachine::with_thresholds(
            40,
            40,
            Duration::from_secs(1),
            Duration::from_secs(20),
        )
    }

    #[test]
    fn starts_stopped() {
        let sm = sm();
        assert_eq!(sm.current(), PlayerState::Stopped);
    }

    #[test]
    fn transitions_to_playing_after_start_debounce() {
        let mut sm = sm();
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
        let mut sm = sm();
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
        let mut sm = sm();
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
        let mut sm = sm();
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
        let mut sm = sm();
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

    #[test]
    fn activation_dbfs_maps_onto_thresholds_with_hysteresis() {
        let mut m = sm();
        m.set_activation_dbfs(-50.0, 2.0);
        // -50 dBFS is 96 on the -80..0 scale; the stop point is 2 dB lower.
        assert_eq!(m.start_threshold, crate::level::db_to_u8(-50.0));
        assert_eq!(m.stop_threshold, crate::level::db_to_u8(-52.0));
        assert!(m.stop_threshold < m.start_threshold);
    }

    #[test]
    fn new_derives_thresholds_from_configured_dbfs() {
        let cfg = VuMeterConfig {
            capture_target: "input-processor".to_string(),
            threshold_dbfs: -40.0,
            hysteresis_db: 2.0,
            start_debounce_secs: 1,
            stop_debounce_secs: 20,
            ws_url: None,
        };
        let m = PlaybackStateMachine::new(&cfg);
        assert_eq!(m.start_threshold, crate::level::db_to_u8(-40.0));
        assert_eq!(m.stop_threshold, crate::level::db_to_u8(-42.0));
    }

    #[test]
    fn a_non_positive_hysteresis_cannot_invert_the_thresholds() {
        // stop_threshold above start_threshold would make a level between them
        // both start and stop playback, flapping once per debounce cycle.
        for hysteresis in [0.0, -5.0, -100.0] {
            let mut m = sm();
            m.set_activation_dbfs(-50.0, hysteresis);
            assert!(
                m.stop_threshold <= m.start_threshold,
                "hysteresis {hysteresis}: stop {} > start {}",
                m.stop_threshold,
                m.start_threshold
            );
        }
    }

    #[test]
    fn a_non_finite_activation_level_falls_back_to_the_default() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut m = sm();
            m.set_activation_dbfs(bad, 2.0);
            assert_eq!(
                m.start_threshold,
                crate::level::db_to_u8(crate::level::DEFAULT_ACTIVATION_DBFS),
                "{bad} should have fallen back to the default"
            );
            assert!(m.stop_threshold <= m.start_threshold);
        }
    }

    #[test]
    fn a_non_finite_hysteresis_does_not_poison_the_stop_threshold() {
        let mut m = sm();
        m.set_activation_dbfs(-50.0, f64::NAN);
        assert_eq!(m.start_threshold, crate::level::db_to_u8(-50.0));
        assert!(m.stop_threshold <= m.start_threshold);
    }

    #[test]
    fn recognition_starts_playback_even_below_the_activation_level() {
        // A recognized track is proof of playback that the level alone missed.
        let mut m = sm();
        assert_eq!(m.note_activity(), Some(PlayerState::Playing));
        assert_eq!(m.current(), PlayerState::Playing);
    }

    #[test]
    fn recognition_while_already_playing_is_not_a_transition() {
        let mut m = sm();
        m.note_activity();
        assert_eq!(m.note_activity(), None, "no state change, so no transition");
        assert_eq!(m.current(), PlayerState::Playing);
    }

    #[test]
    fn recognition_restarts_the_stop_debounce() {
        let mut m = sm();
        let t0 = Instant::now();
        m.note_activity();
        // Silence starts counting down towards Stopped...
        assert_eq!(m.on_level(5, t0), None);
        assert_eq!(m.on_level(5, t0 + Duration::from_secs(15)), None);
        // ...but a fresh recognition means the input is live after all.
        assert_eq!(m.note_activity(), None);
        assert_eq!(m.on_level(5, t0 + Duration::from_secs(16)), None);
        // The old 20s deadline (t0+20) must no longer apply.
        assert_eq!(m.on_level(5, t0 + Duration::from_secs(21)), None);
        assert_eq!(
            m.on_level(5, t0 + Duration::from_secs(37)),
            Some(PlayerState::Stopped)
        );
    }

    #[test]
    fn a_level_above_the_threshold_still_stops_after_recognition_ends() {
        // Recognition does not latch Playing: the ordinary stop debounce
        // still applies once the input goes quiet.
        let mut m = sm();
        let t0 = Instant::now();
        m.note_activity();
        assert_eq!(m.on_level(5, t0), None);
        assert_eq!(
            m.on_level(5, t0 + Duration::from_secs(21)),
            Some(PlayerState::Stopped)
        );
    }
}
