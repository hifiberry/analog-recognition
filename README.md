# analog-recognition

Identifies tracks playing on an analog (e.g. turntable/RIAA) input using
[songrec](https://github.com/marin-m/SongRec), and reports accurate
play/stop state from `vu-meter-service`'s real-time signal level —
independent of songrec's sparse recognition cadence. Publishes both into
[AudioControl](https://github.com/hifiberry/acr) as a generic player named
`analog`.

## HiFiBerryOS plugin registration

Registration with AudioControl is automatic: the package's `postinst`
installs an ACR `players.d` drop-in at
`/etc/audiocontrol/players.d/analog.json` (removed again by `postrm` on
uninstall), so there's no manual edit of `audiocontrol.json` needed anymore.

The same `postinst` also registers the plugin with the HiFiBerryOS Web UI
(`/etc/hifiberry/players.d/analog.json` + icon) and grants the plugin's
systemd service the necessary permissions via a `configserver` drop-in
(`/etc/configserver/conf.d/analog-recognition.json`).

### "Recognize tracks" setting

The Web UI exposes a "Recognize tracks" toggle for this player
(ConfigDB key `player.analog-recognition.songrec_enabled`, default `on`).
When switched off, the service stops invoking `songrec` and instead
publishes `"Unknown artist"` / `"Unknown song"` for the currently playing
track, while play/stop state (from the VU meter) keeps working as normal.
The service polls this setting from the configurator (see
`[configurator]` below) rather than requiring a restart.

## Configuration

See `/etc/analog-recognition/config.toml` (installed by the package with
sensible defaults). Key values:

- `songrec.device` — the PipeWire monitor source to listen on (default
  `input-processor.monitor`).
- `vu_meter.start_threshold` / `stop_threshold` — level (0-255, mapping
  -60dB..0dB) used to distinguish real playback from idle/surface noise.
  Tune against your actual turntable/cartridge noise floor.
- `vu_meter.stop_debounce_secs` — how long the level must stay below
  threshold before reporting Stopped (default 20s, to survive normal
  inter-track pauses without flapping).
- `configurator.base_url` — base URL of the configurator ConfigDB API used
  to read the "Recognize tracks" setting (default
  `http://localhost:1081/api/v1`).
- `configurator.songrec_enabled_key` — ConfigDB key backing the toggle
  (default `player.analog-recognition.songrec_enabled`).
- `configurator.setting_poll_secs` — how often the setting is re-read from
  the configurator (default 10s).

## Building

```bash
cargo build --release
```

## Running

```bash
systemctl --user enable --now analog-recognition
```
