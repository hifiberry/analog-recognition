use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub audiocontrol: AudioControlConfig,
    pub songrec: SongrecConfig,
    pub vu_meter: VuMeterConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
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
            device = "riaa.monitor"
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
        assert_eq!(cfg.songrec.device, "riaa.monitor");
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
            device = "riaa.monitor"
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
}
