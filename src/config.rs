use serde::Deserialize;

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

#[derive(Debug, Clone, Deserialize)]
pub struct VuMeterConfig {
    pub ws_url: String,
    pub start_threshold: u8,
    pub stop_threshold: u8,
    pub start_debounce_secs: u64,
    pub stop_debounce_secs: u64,
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
    #[serde(default = "default_setting_poll_secs")]
    pub setting_poll_secs: u64,
}

fn default_configurator_base_url() -> String {
    "http://localhost:1081/api/v1".to_string()
}
fn default_songrec_enabled_key() -> String {
    "player.analog-recognition.songrec_enabled".to_string()
}
fn default_setting_poll_secs() -> u64 {
    10
}

impl Default for ConfiguratorConfig {
    fn default() -> Self {
        ConfiguratorConfig {
            base_url: default_configurator_base_url(),
            songrec_enabled_key: default_songrec_enabled_key(),
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
            start_threshold = 40
            stop_threshold = 40
            start_debounce_secs = 1
            stop_debounce_secs = 20

            [logging]
            level = "debug"
        "#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.audiocontrol.player_name, "analog");
        assert_eq!(cfg.songrec.device, "input-processor.monitor");
        assert_eq!(cfg.vu_meter.start_threshold, 40);
        assert_eq!(cfg.logging.level, "debug");
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
            start_threshold = 40
            stop_threshold = 40
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
            start_threshold = 40
            stop_threshold = 40
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
            start_threshold = 40
            stop_threshold = 40
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
    }
}
