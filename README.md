# analog-recognition

Identifies tracks playing on an analog (e.g. turntable/RIAA) input using
[songrec](https://github.com/marin-m/SongRec), and reports accurate
play/stop state from `vu-meter-service`'s real-time signal level —
independent of songrec's sparse recognition cadence. Publishes both into
[AudioControl](https://github.com/hifiberry/acr) as a generic player named
`analog`.

## Required AudioControl configuration

Add the following to the `players` list in
`/etc/audiocontrol/audiocontrol.json` (not applied automatically by this
package, to avoid clobbering an operator-managed config file):

```json
{
  "analog": {
    "type": "generic",
    "name": "analog",
    "display_name": "Analog Input",
    "enable": true,
    "supports_api_events": true,
    "capabilities": [],
    "initial_state": "stopped"
  }
}
```

`capabilities` is intentionally empty: this player reports real-world state,
it isn't something a user can drive via play/pause/next commands.

## Configuration

See `/etc/analog-recognition/config.toml` (installed by the package with
sensible defaults). Key values:

- `songrec.device` — the PipeWire monitor source to listen on (default
  `riaa.monitor`).
- `vu_meter.start_threshold` / `stop_threshold` — level (0-255, mapping
  -60dB..0dB) used to distinguish real playback from idle/surface noise.
  Tune against your actual turntable/cartridge noise floor.
- `vu_meter.stop_debounce_secs` — how long the level must stay below
  threshold before reporting Stopped (default 20s, to survive normal
  inter-track pauses without flapping).

## Building

```bash
cargo build --release
```

## Running

```bash
systemctl --user enable --now analog-recognition
```
