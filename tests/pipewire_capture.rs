//! End-to-end check of the native PipeWire capture: play a known tone into a
//! throwaway null sink, point `start_capture` at it, and confirm the shared
//! level rises while the tone plays and falls when it stops.
//!
//! This needs a running PipeWire session (daemon + session manager) plus
//! `pactl` and `pw-play`, so it is `#[ignore]`d — normal `cargo test` and the
//! package build do not require an audio server. Run it where one exists:
//!
//!     cargo test --test pipewire_capture -- --ignored --nocapture
//!
//! If PipeWire or the tools are missing it skips (passes) rather than failing,
//! so it is safe to run anywhere.

use std::process::Command;
use std::time::{Duration, Instant};

use analog_recognition::input_level::{start_capture, LevelSource};

const SINK: &str = "ar_test_sink";
const RATE: u32 = 48_000;
const CHANNELS: usize = 2;

/// True if a PipeWire session and the tools we need are available.
fn pipewire_available() -> bool {
    for tool in ["pactl", "pw-play"] {
        if Command::new(tool).arg("--version").output().is_err() {
            eprintln!("skipping: {tool} not found");
            return false;
        }
    }
    match Command::new("pactl").arg("info").output() {
        Ok(o) if o.status.success() => true,
        _ => {
            eprintln!("skipping: no PipeWire/Pulse server reachable");
            false
        }
    }
}

/// A few seconds of a -6 dBFS stereo sine, S32LE, as a WAV file.
fn write_tone_wav(path: &std::path::Path, secs: u32) {
    let frames = RATE * secs;
    let data_len = frames * CHANNELS as u32 * 4;
    let mut w: Vec<u8> = Vec::with_capacity(44 + data_len as usize);
    let byte_rate = RATE * CHANNELS as u32 * 4;
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVE");
    w.extend_from_slice(b"fmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&(CHANNELS as u16).to_le_bytes());
    w.extend_from_slice(&RATE.to_le_bytes());
    w.extend_from_slice(&byte_rate.to_le_bytes());
    w.extend_from_slice(&((CHANNELS as u16) * 4).to_le_bytes()); // block align
    w.extend_from_slice(&32u16.to_le_bytes()); // bits/sample
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    let amp = 0.5 * i32::MAX as f64; // -6 dBFS
    for n in 0..frames {
        let t = n as f64 / RATE as f64;
        let s = (amp * (2.0 * std::f64::consts::PI * 440.0 * t).sin()) as i32;
        for _ in 0..CHANNELS {
            w.extend_from_slice(&s.to_le_bytes());
        }
    }
    std::fs::write(path, w).expect("write tone wav");
}

fn max_level_over(level: &std::sync::Arc<LevelSource>, dur: Duration) -> u8 {
    let deadline = Instant::now() + dur;
    let mut peak = 0u8;
    while Instant::now() < deadline {
        peak = peak.max(level.current(Instant::now()));
        std::thread::sleep(Duration::from_millis(50));
    }
    peak
}

#[test]
#[ignore = "needs a running PipeWire session; run with --ignored"]
fn captures_a_tone_played_into_a_null_sink() {
    if !pipewire_available() {
        return;
    }

    // A throwaway sink whose monitor carries whatever we play into it.
    let load = Command::new("pactl")
        .args([
            "load-module",
            "module-null-sink",
            &format!("sink_name={SINK}"),
            "sink_properties=media.class=Audio/Sink",
        ])
        .output()
        .expect("load null sink");
    assert!(load.status.success(), "could not create null sink");
    let module_id = String::from_utf8_lossy(&load.stdout).trim().to_string();
    // Always tear the module down, even on assertion failure.
    let _guard = ModuleGuard(module_id);

    // Give the session manager a moment to register the sink.
    std::thread::sleep(Duration::from_millis(500));

    let level = start_capture(SINK);

    // Baseline: silence into the sink -> level stays low.
    let idle = max_level_over(&level, Duration::from_secs(1));
    assert!(
        idle < 20,
        "expected near-silence before the tone, got {idle}"
    );

    // Play the tone into the sink for a few seconds.
    let dir = std::env::temp_dir();
    let wav = dir.join("ar_test_tone.wav");
    write_tone_wav(&wav, 5);
    let mut player = Command::new("pw-play")
        .args([&format!("--target={SINK}"), wav.to_str().unwrap()])
        .spawn()
        .expect("spawn pw-play");

    // A -6 dBFS tone is ~level 230; require it to clearly cross into "loud".
    let peak = max_level_over(&level, Duration::from_secs(3));
    assert!(
        peak > 150,
        "tone should raise the level well above threshold, got {peak}"
    );

    let _ = player.kill();
    let _ = player.wait();

    // After the tone stops the captured level must fall back down.
    std::thread::sleep(Duration::from_millis(500));
    let quiet = max_level_over(&level, Duration::from_secs(1));
    assert!(
        quiet < 40,
        "level should fall after the tone stops, got {quiet}"
    );

    let _ = std::fs::remove_file(&wav);
}

struct ModuleGuard(String);
impl Drop for ModuleGuard {
    fn drop(&mut self) {
        if !self.0.is_empty() {
            let _ = Command::new("pactl")
                .args(["unload-module", &self.0])
                .output();
        }
    }
}
