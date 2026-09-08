//! Native PipeWire capture of the analog input, turned into the 0-255 level the
//! state machine consumes.
//!
//! Why this exists: the analog player must report Playing/Stopped from the
//! *analog input* alone. Reading the shared output vu-meter is wrong — it also
//! carries whatever network player is running, so Tidal/Spotify playback would
//! be detected as analog activity and preempt itself. So we capture the analog
//! node (`input-processor`) directly.
//!
//! The capture runs on PipeWire's realtime thread. Unlike the platform
//! vu-meter, it needs only one aggregate level, so it computes each buffer's
//! RMS inline and publishes it to a lock-free `AtomicU8` — no shared sample
//! buffer, no mutex in the RT path, no processing thread.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use pipewire as pw;
use pw::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pw::spa::pod::Pod;

use crate::audiocontrol::AudioControlClient;
use crate::config::VuMeterConfig;
use crate::level;
use crate::state::{PlaybackStateMachine, PlayerState};
use tokio::sync::{watch, Notify};

const SAMPLE_RATE: u32 = 48_000;
const NUM_CHANNELS: usize = 2;
pub(crate) const MAX_VALUE_S32: f64 = 2_147_483_648.0;
/// How often the state task samples the level. Finer than the shortest
/// debounce (1s) so debounce timing stays accurate.
const POLL: Duration = Duration::from_millis(100);
/// Delay before retrying a failed PipeWire connection (e.g. capture target not
/// present yet at boot).
const RECONNECT: Duration = Duration::from_secs(5);

/// Level (0-255) of one interleaved S32LE block: the RMS of the louder
/// channel, on the -60..0 dB scale. Pure and allocation-free (a fixed stack
/// array bounds the channel count) so it is both RT-safe and unit-testable
/// without a PipeWire stream. Trailing bytes that do not complete a frame are
/// ignored.
fn block_level_u8(bytes: &[u8], channels: usize) -> u8 {
    /// Upper bound on channels so the accumulator can live on the stack.
    const MAX_CH: usize = 8;
    let channels = channels.min(MAX_CH);
    if channels == 0 {
        return 0;
    }
    let frame_bytes = 4 * channels;
    let frames = bytes.len() / frame_bytes;
    if frames == 0 {
        return 0;
    }
    let mut sum_sq = [0.0f64; MAX_CH];
    for f in 0..frames {
        for (ch, acc) in sum_sq.iter_mut().take(channels).enumerate() {
            let off = f * frame_bytes + ch * 4;
            let s =
                i32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]]);
            let norm = s as f64 / MAX_VALUE_S32;
            *acc += norm * norm;
        }
    }
    // A signal on either channel counts as activity, so report the louder one.
    sum_sq[..channels]
        .iter()
        .map(|&ss| level::rms_to_u8(ss, frames))
        .max()
        .unwrap_or(0)
}

/// Start capturing `target` and return the shared level (0-255). The capture
/// thread reconnects on its own if PipeWire or the target is not ready yet;
/// until it produces samples the level stays 0 (i.e. Stopped), which is the
/// safe default.
pub fn start_capture(target: &str) -> Arc<AtomicU8> {
    let level = Arc::new(AtomicU8::new(0));
    let level_for_thread = level.clone();
    let target = target.to_string();
    thread::Builder::new()
        .name("analog-capture".into())
        .spawn(move || {
            pw::init();
            loop {
                if let Err(e) = run_capture(&target, &level_for_thread) {
                    log::warn!("analog capture ended: {e:?}; retrying");
                }
                // On any exit the input is unknown — do not leave a stale
                // "loud" level latched, or the player could stay Playing.
                level_for_thread.store(0, Ordering::Relaxed);
                thread::sleep(RECONNECT);
            }
        })
        .expect("spawn analog capture thread");
    level
}

fn run_capture(target: &str, level: &Arc<AtomicU8>) -> anyhow::Result<()> {
    let main_loop =
        pw::main_loop::MainLoop::new(None).map_err(|e| anyhow::anyhow!("main loop: {e:?}"))?;
    let context =
        pw::context::Context::new(&main_loop).map_err(|e| anyhow::anyhow!("context: {e:?}"))?;
    let core = context
        .connect(None)
        .map_err(|e| anyhow::anyhow!("connect core: {e:?}"))?;

    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::S32LE);
    audio_info.set_rate(SAMPLE_RATE);
    audio_info.set_channels(NUM_CHANNELS as u32);

    // Target the analog node by name and let the session manager link us to
    // its output/monitor — the same thing `pw-record --target <node>` does,
    // without shelling out to pw-link.
    let stream = pw::stream::Stream::new(
        &core,
        "analog-level-capture",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Music",
            *pw::keys::NODE_NAME => "analog-level-capture",
            // input-processor is an Audio/Sink; its monitor carries the analog
            // input. Capturing a sink's monitor needs this flag, else the
            // session manager links us to the default source instead.
            "stream.capture.sink" => "true",
            "target.object" => target,
        },
    )
    .map_err(|e| anyhow::anyhow!("stream: {e:?}"))?;

    let level_cb = level.clone();
    let _listener = stream
        .add_local_listener_with_user_data(())
        .process(move |stream, _| {
            let Some(mut buf) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buf.datas_mut();
            let Some(data) = datas.first_mut() else {
                return;
            };
            let size = data.chunk().size() as usize;
            let Some(slice) = data.data() else {
                return;
            };
            let frame_bytes = 4 * NUM_CHANNELS;
            let frames = (size / frame_bytes).min(slice.len() / frame_bytes);
            // block_level_u8 does bounded float work over a stack array — no
            // locks, no allocation — so it is safe on the RT thread.
            let loudest = block_level_u8(&slice[..frames * frame_bytes], NUM_CHANNELS);
            level_cb.store(loudest, Ordering::Relaxed);
        })
        .register()
        .map_err(|e| anyhow::anyhow!("listener: {e:?}"))?;

    let obj = pw::spa::pod::Object {
        type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: pw::spa::param::ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow::anyhow!("serialize format: {e:?}"))?
    .0
    .into_inner();
    let mut params = [Pod::from_bytes(&values).unwrap()];

    stream
        .connect(
            pw::spa::utils::Direction::Input,
            None,
            pw::stream::StreamFlags::AUTOCONNECT
                | pw::stream::StreamFlags::MAP_BUFFERS
                | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(|e| anyhow::anyhow!("stream connect: {e:?}"))?;

    log::info!("analog level capture connected to '{target}'");
    main_loop.run(); // blocks until the loop is torn down
    Ok(())
}

/// Poll the captured level and drive AudioControl. Never returns.
pub async fn run_state_task(
    cfg: &VuMeterConfig,
    client: &AudioControlClient,
    song_reset: &Arc<Notify>,
    state_tx: &watch::Sender<PlayerState>,
    level: Arc<AtomicU8>,
) -> ! {
    let mut sm = PlaybackStateMachine::new(cfg);
    // The last state we confirmed AudioControl accepted. Compared every tick,
    // not just on transitions, so a send that failed during a boot-time race
    // is retried on the next tick instead of desyncing until the next real
    // transition (which for a vinyl side may be many minutes away).
    let mut last_synced: Option<PlayerState> = None;
    let mut ticker = tokio::time::interval(POLL);
    loop {
        ticker.tick().await;
        let transition = sm.on_level(level.load(Ordering::Relaxed), Instant::now());
        report(
            client,
            song_reset,
            state_tx,
            transition,
            sm.current(),
            &mut last_synced,
        )
        .await;
    }
}

/// Apply one tick's outcome: broadcast a transition, clear the song and reset
/// recognition on Stop, and keep AudioControl's reported state in sync
/// (retrying a previously failed send). Factored out so it is testable without
/// PipeWire or real timers.
async fn report(
    client: &AudioControlClient,
    song_reset: &Arc<Notify>,
    state_tx: &watch::Sender<PlayerState>,
    transition: Option<PlayerState>,
    current: PlayerState,
    last_synced: &mut Option<PlayerState>,
) {
    if let Some(new_state) = transition {
        let _ = state_tx.send(new_state);
        if new_state == PlayerState::Stopped {
            if let Err(e) = client.send_song_cleared().await {
                log::warn!("failed to clear song: {e}");
            }
            // The display is now blank, so the next match must be
            // re-announced even if it is the same track (a brief pause, or a
            // quiet passage that dipped below threshold).
            song_reset.notify_one();
        }
    }
    if *last_synced != Some(current) {
        match client.send_state_changed(current).await {
            Ok(()) => *last_synced = Some(current),
            Err(e) => log::warn!("failed to send state_changed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(uri: String) -> AudioControlClient {
        AudioControlClient::new(uri, "analog".to_string())
    }

    /// Build an interleaved S32LE block with a constant value per channel.
    fn block(per_channel: &[i32], frames: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(frames * per_channel.len() * 4);
        for _ in 0..frames {
            for &s in per_channel {
                out.extend_from_slice(&s.to_le_bytes());
            }
        }
        out
    }

    #[test]
    fn block_level_silence_is_zero() {
        assert_eq!(block_level_u8(&block(&[0, 0], 480), 2), 0);
    }

    #[test]
    fn block_level_full_scale_is_max() {
        let fs = block(&[i32::MAX, i32::MAX], 480);
        assert_eq!(block_level_u8(&fs, 2), 255);
    }

    #[test]
    fn block_level_reports_the_louder_channel() {
        // Left near full scale, right silent -> the loud channel wins.
        let b = block(&[i32::MAX, 0], 480);
        assert_eq!(block_level_u8(&b, 2), 255);
        let b = block(&[0, i32::MAX], 480);
        assert_eq!(block_level_u8(&b, 2), 255);
    }

    #[test]
    fn block_level_matches_the_db_scale() {
        // A constant amplitude of 10^(-30/20) * full-scale is -30 dBFS, which
        // is the midpoint of -60..0 -> ~127.
        let amp = (10f64.powf(-30.0 / 20.0) * crate::input_level::MAX_VALUE_S32) as i32;
        let b = block(&[amp, amp], 1024);
        let u = block_level_u8(&b, 2);
        assert!((125..=129).contains(&u), "got {u}");
    }

    #[test]
    fn block_level_ignores_a_trailing_partial_frame() {
        // 2 full stereo frames (16 bytes) plus 3 stray bytes must not panic
        // and must measure only the complete frames.
        let mut b = block(&[i32::MAX, i32::MAX], 2);
        b.extend_from_slice(&[1, 2, 3]);
        assert_eq!(block_level_u8(&b, 2), 255);
    }

    #[test]
    fn block_level_empty_is_zero() {
        assert_eq!(block_level_u8(&[], 2), 0);
    }

    #[tokio::test]
    async fn a_stop_transition_clears_the_song_and_resets_recognition() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .and(body_json(serde_json::json!({ "type": "song_changed" })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .and(body_json(
                serde_json::json!({ "type": "state_changed", "state": "stopped" }),
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let c = client(server.uri());
        let song_reset = Arc::new(Notify::new());
        let (tx, _rx) = watch::channel(PlayerState::Playing);
        let mut last_synced = Some(PlayerState::Playing);

        let notified = song_reset.notified();
        tokio::pin!(notified);
        report(
            &c,
            &song_reset,
            &tx,
            Some(PlayerState::Stopped),
            PlayerState::Stopped,
            &mut last_synced,
        )
        .await;

        assert_eq!(last_synced, Some(PlayerState::Stopped));
        assert_eq!(*tx.borrow(), PlayerState::Stopped);
        // song_reset was notified exactly once by the Stop.
        assert!(futures_util::poll!(notified).is_ready());
    }

    #[tokio::test]
    async fn a_failed_state_send_is_retried_next_tick() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .and(body_json(
                serde_json::json!({ "type": "state_changed", "state": "playing" }),
            ))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .and(body_json(
                serde_json::json!({ "type": "state_changed", "state": "playing" }),
            ))
            .respond_with(ResponseTemplate::new(200))
            .with_priority(2)
            .expect(1)
            .mount(&server)
            .await;

        let c = client(server.uri());
        let song_reset = Arc::new(Notify::new());
        let (tx, _rx) = watch::channel(PlayerState::Stopped);
        let mut last_synced: Option<PlayerState> = None;

        // First tick: transition to Playing, send fails -> not synced.
        report(
            &c,
            &song_reset,
            &tx,
            Some(PlayerState::Playing),
            PlayerState::Playing,
            &mut last_synced,
        )
        .await;
        assert_eq!(last_synced, None, "failed send must not mark as synced");
        // Next tick: no new transition, but current still Playing -> retried.
        report(
            &c,
            &song_reset,
            &tx,
            None,
            PlayerState::Playing,
            &mut last_synced,
        )
        .await;
        assert_eq!(last_synced, Some(PlayerState::Playing));
    }

    #[tokio::test]
    async fn a_steady_state_is_not_resent_every_tick() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/player/analog/update"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1) // exactly one send despite many ticks
            .mount(&server)
            .await;

        let c = client(server.uri());
        let song_reset = Arc::new(Notify::new());
        let (tx, _rx) = watch::channel(PlayerState::Stopped);
        let mut last_synced: Option<PlayerState> = None;

        report(
            &c,
            &song_reset,
            &tx,
            Some(PlayerState::Playing),
            PlayerState::Playing,
            &mut last_synced,
        )
        .await;
        for _ in 0..5 {
            report(
                &c,
                &song_reset,
                &tx,
                None,
                PlayerState::Playing,
                &mut last_synced,
            )
            .await;
        }
        assert_eq!(last_synced, Some(PlayerState::Playing));
    }
}
