use analog_recognition::{audiocontrol, config, settings, songrec, vu_meter};

use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/etc/analog-recognition/config.toml".to_string());
    let cfg = config::Config::load(&config_path)?;

    // RUST_LOG, if set, always wins; otherwise fall back to the level from
    // config.toml so operators can control verbosity without env vars.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(&cfg.logging.level))
        .init();
    log::info!("loaded config from {config_path}");

    let client = Arc::new(audiocontrol::AudioControlClient::new(
        cfg.audiocontrol.base_url.clone(),
        cfg.audiocontrol.player_name.clone(),
    ));
    let song_reset = Arc::new(tokio::sync::Notify::new());
    let (state_tx, state_rx) = tokio::sync::watch::channel(vu_meter::PlayerState::Stopped);

    let state_client = client.clone();
    let vu_cfg = cfg.vu_meter.clone();
    let state_song_reset = song_reset.clone();
    let state_handle = tokio::spawn(async move {
        vu_meter::run_state_task(&vu_cfg, &state_client, &state_song_reset, &state_tx).await
    });

    let settings = Arc::new(settings::SettingsClient::new(
        cfg.configurator.base_url.clone(),
        cfg.configurator.songrec_enabled_key.clone(),
    ));
    let poll = std::time::Duration::from_secs(cfg.configurator.setting_poll_secs);

    let rec_client = client.clone();
    let rec_settings = settings.clone();
    let songrec_cfg = cfg.songrec.clone();
    let rec_handle = tokio::spawn(async move {
        songrec::run_recognition_task(
            &songrec_cfg,
            &rec_settings,
            &rec_client,
            &song_reset,
            poll,
            &state_rx,
        )
        .await
    });

    let _ = tokio::join!(state_handle, rec_handle);
    Ok(())
}
