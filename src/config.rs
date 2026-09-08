use serde::Deserialize;
#[cfg(test)]
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub audiocontrol: AudioControlConfig,
    pub songrec: SongrecConfig,
    pub vu_meter: VuMeterConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub configurator: ConfiguratorConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AudioControlConfig {
    pub base_url: String,
    pub player_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SongrecConfig {
    pub device: String,
    pub request_interval_secs: u64,
    /// Must be songrec itself, or a wrapper that `exec`s it. The stream check
    /// asks PipeWire about the pid of the process spawned here, and a wrapper
    /// that merely *runs* songrec as a child keeps a pid PipeWire never sees.
    pub binary: String,
    /// How often to confirm songrec still holds a capture stream. Zero turns
    /// the check off. Defaulted so existing config files keep working.
    #[serde(default = "default_stream_check_secs")]
    pub stream_check_secs: u64,
    /// How long to let songrec connect and open its device before the first
    /// check. Checking sooner restarts it before it can ever start working.
    #[serde(default = "default_stream_grace_secs")]
    pub stream_grace_secs: u64,
    #[serde(default = "default_pw_dump_binary")]
    pub pw_dump_binary: String,
}

fn default_stream_check_secs() -> u64 {
    30
}
fn default_stream_grace_secs() -> u64 {
    20
}
fn default_pw_dump_binary() -> String {
    "pw-dump".to_string()
}

/// Thresholds and the capture source for the playback-detection level.
///
/// The section is still called `[vu_meter]` for config back-compatibility, but
/// the level now comes from capturing `capture_target` directly (the analog
/// input) rather than the shared output vu-meter — see `input_level`. `ws_url`
/// is retained only so old config files still parse; it is unused.
#[derive(Debug, Clone, Deserialize)]
pub struct VuMeterConfig {
    /// PipeWire node to capture for level detection. Must be the analog input
    /// chain (e.g. "input-processor"), never the output mix, or network
    /// playback would be detected as analog activity.
    #[serde(default = "default_capture_target")]
    pub capture_target: String,
    /// Activation level in dBFS: the analog input must reach this to count as
    /// playing. This is what the Web UI exposes (see players.d/analog.json);
    /// the value there overrides this one at runtime.
    #[serde(default = "default_threshold_dbfs")]
    pub threshold_dbfs: f64,
    /// How many dB below the activation level the input must fall to count as
    /// stopped. A little hysteresis keeps the state from flapping around the
    /// threshold.
    #[serde(default = "default_hysteresis_db")]
    pub hysteresis_db: f64,
    #[serde(default = "default_start_debounce_secs")]
    pub start_debounce_secs: u64,
    #[serde(default = "default_stop_debounce_secs")]
    pub stop_debounce_secs: u64,
    /// Deprecated: the old output-vu-meter WebSocket URL. Ignored.
    #[serde(default)]
    pub ws_url: Option<String>,
}

fn default_capture_target() -> String {
    "input-processor".to_string()
}
fn default_threshold_dbfs() -> f64 {
    crate::level::DEFAULT_ACTIVATION_DBFS
}
fn default_hysteresis_db() -> f64 {
    2.0
}
fn default_start_debounce_secs() -> u64 {
    1
}
fn default_stop_debounce_secs() -> u64 {
    20
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
}

fn default_log_level() -> String {
    "info".to_string()
}

impl Default for LoggingConfig {
    fn default() -> Self {
        LoggingConfig {
            level: default_log_level(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConfiguratorConfig {
    #[serde(default = "default_configurator_base_url")]
    pub base_url: String,
    #[serde(default = "default_songrec_enabled_key")]
    pub songrec_enabled_key: String,
    /// ConfigDB key holding the Web-UI activation level (dBFS). Written by the
    /// player-settings endpoint from players.d/analog.json's "threshold_dbfs".
    #[serde(default = "default_threshold_key")]
    pub threshold_key: String,
    #[serde(default = "default_setting_poll_secs")]
    pub setting_poll_secs: u64,
}

fn default_configurator_base_url() -> String {
    "http://localhost:1081/api/v1".to_string()
}
fn default_songrec_enabled_key() -> String {
    "player.analog-recognition.songrec_enabled".to_string()
}
fn default_threshold_key() -> String {
    "player.analog-recognition.threshold_dbfs".to_string()
}
fn default_setting_poll_secs() -> u64 {
    10
}

impl ConfiguratorConfig {
    /// The settings poll period, never zero.
    ///
    /// `tokio::time::interval` panics outright on a zero period, so a
    /// `setting_poll_secs = 0` would take the daemon down on the first tick --
    /// and 0 reads like a plausible "don't poll" to anyone editing the file,
    /// because for `stream_check_secs` that is exactly what it means.
    pub fn poll_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.setting_poll_secs.max(1))
    }
}

impl Default for ConfiguratorConfig {
    fn default() -> Self {
        ConfiguratorConfig {
            base_url: default_configurator_base_url(),
            songrec_enabled_key: default_songrec_enabled_key(),
            threshold_key: default_threshold_key(),
            setting_poll_secs: default_setting_poll_secs(),
        }
    }
}

impl Config {
    pub fn load(path: &str) -> anyhow::Result<Config> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read config file {path}: {e}"))?;
        let cfg: Config = toml::from_str(&contents)
            .map_err(|e| anyhow::anyhow!("failed to parse config file {path}: {e}"))?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_config() {
        let toml_str = r#"
            [audiocontrol]
            base_url = "http://localhost:1080/api"
            player_name = "analog"

            [songrec]
            device = "input-processor.monitor"
            request_interval_secs = 10
            binary = "songrec"

            [vu_meter]
            ws_url = "ws://localhost:2717/api/v1/levels"
            threshold_dbfs = -50.0
            hysteresis_db = 2.0
            start_debounce_secs = 1
            stop_debounce_secs = 20

            [logging]
            level = "debug"
        "#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.audiocontrol.player_name, "analog");
        assert_eq!(cfg.songrec.device, "input-processor.monitor");
        assert_eq!(cfg.vu_meter.threshold_dbfs, -50.0);
        assert_eq!(cfg.logging.level, "debug");
    }

    #[test]
    fn capture_target_defaults_and_ws_url_is_optional() {
        // A minimal [vu_meter] with nothing set: capture_target, the dBFS
        // activation level, hysteresis and debounce all fall back to defaults,
        // and the deprecated ws_url is simply absent.
        let toml_str = r#"
            [audiocontrol]
            base_url = "http://localhost:1080/api"
            player_name = "analog"

            [songrec]
            device = "input-processor.monitor"
            request_interval_secs = 10
            binary = "songrec"

            [vu_meter]
        "#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.vu_meter.capture_target, "input-processor");
        assert_eq!(cfg.vu_meter.ws_url, None);
        assert_eq!(cfg.vu_meter.threshold_dbfs, -50.0);
        assert_eq!(cfg.vu_meter.hysteresis_db, 2.0);
        assert_eq!(cfg.vu_meter.start_debounce_secs, 1);
        assert_eq!(cfg.vu_meter.stop_debounce_secs, 20);
        assert_eq!(
            cfg.configurator.threshold_key,
            "player.analog-recognition.threshold_dbfs"
        );
    }

    #[test]
    fn capture_target_is_read_and_legacy_ws_url_still_parses() {
        let toml_str = r#"
            [audiocontrol]
            base_url = "http://localhost:1080/api"
            player_name = "analog"

            [songrec]
            device = "input-processor.monitor"
            request_interval_secs = 10
            binary = "songrec"

            [vu_meter]
            capture_target = "some-other-node"
            ws_url = "ws://localhost:2717/api/v1/levels"
            threshold_dbfs = -50.0
            hysteresis_db = 2.0
            start_debounce_secs = 1
            stop_debounce_secs = 20
        "#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.vu_meter.capture_target, "some-other-node");
        // Legacy ws_url is accepted (so old files parse) but carried only so
        // the daemon can warn; it is not used.
        assert_eq!(
            cfg.vu_meter.ws_url.as_deref(),
            Some("ws://localhost:2717/api/v1/levels")
        );
    }

    #[test]
    fn logging_defaults_when_omitted() {
        let toml_str = r#"
            [audiocontrol]
            base_url = "http://localhost:1080/api"
            player_name = "analog"

            [songrec]
            device = "input-processor.monitor"
            request_interval_secs = 10
            binary = "songrec"

            [vu_meter]
            ws_url = "ws://localhost:2717/api/v1/levels"
            threshold_dbfs = -50.0
            hysteresis_db = 2.0
            start_debounce_secs = 1
            stop_debounce_secs = 20
        "#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.logging.level, "info");
    }

    #[test]
    fn configurator_defaults_when_section_omitted() {
        let toml_str = r#"
            [audiocontrol]
            base_url = "http://localhost:1080/api"
            player_name = "analog"

            [songrec]
            device = "input-processor.monitor"
            request_interval_secs = 10
            binary = "songrec"

            [vu_meter]
            ws_url = "ws://localhost:2717/api/v1/levels"
            threshold_dbfs = -50.0
            hysteresis_db = 2.0
            start_debounce_secs = 1
            stop_debounce_secs = 20
        "#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.configurator.base_url, "http://localhost:1081/api/v1");
        assert_eq!(
            cfg.configurator.songrec_enabled_key,
            "player.analog-recognition.songrec_enabled"
        );
        assert_eq!(cfg.configurator.setting_poll_secs, 10);
    }

    #[test]
    fn parses_configurator_section() {
        let toml_str = r#"
            [audiocontrol]
            base_url = "http://localhost:1080/api"
            player_name = "analog"

            [songrec]
            device = "input-processor.monitor"
            request_interval_secs = 10
            binary = "songrec"

            [vu_meter]
            ws_url = "ws://localhost:2717/api/v1/levels"
            threshold_dbfs = -50.0
            hysteresis_db = 2.0
            start_debounce_secs = 1
            stop_debounce_secs = 20

            [configurator]
            base_url = "http://example:9/api/v1"
            songrec_enabled_key = "player.analog-recognition.songrec_enabled"
            setting_poll_secs = 5
        "#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.configurator.base_url, "http://example:9/api/v1");
        assert_eq!(cfg.configurator.setting_poll_secs, 5);
        assert_eq!(cfg.configurator.poll_interval(), Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_zero_poll_interval_is_clamped_rather_than_panicking_a_timer() {
        // tokio::time::interval panics on a zero period, so 0 must never reach
        // one -- see ConfiguratorConfig::poll_interval.
        let cfg = ConfiguratorConfig {
            setting_poll_secs: 0,
            ..ConfiguratorConfig::default()
        };
        assert_eq!(cfg.poll_interval(), Duration::from_secs(1));
        // And the timer it feeds really does accept the clamped value.
        let _ = tokio::time::interval(cfg.poll_interval());
    }

    #[test]
    fn the_config_default_activation_level_is_the_shared_one() {
        // config.toml's default and the state machine's fallback are the same
        // constant, so they cannot drift apart.
        assert_eq!(
            default_threshold_dbfs(),
            crate::level::DEFAULT_ACTIVATION_DBFS
        );
    }
}
