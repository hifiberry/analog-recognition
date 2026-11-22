//! The analog input's level under aas, from the daemon's `/levels` WebSocket.
//!
//! Under aas there is no PipeWire node to capture. The daemon measures the
//! analog input itself, as captured and before its chain, and sends the RMS
//! and peak of every meter in a binary frame every 100 ms. Meter 16 is the
//! analog input; it is never the output mix, so a network player is not
//! taken for analog activity.
//!
//! aas's scale runs from 0 at -60 dBFS or less to 255 at 0 dBFS. It is turned
//! into this crate's -80..0 dB scale, so the activation level means the same
//! under both servers. Below -60 dBFS aas reads 0, which maps to the bottom of
//! the scale: an activation level under -60 dBFS cannot be told apart from
//! silence under aas.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::input_level::LevelSource;
use crate::level;

/// The analog input's meter in a levels frame.
const ANALOG_METER: u8 = 16;
/// The bottom of aas's scale, in dBFS.
const AAS_MIN_DB: f64 = -60.0;
/// Delay before connecting again after the daemon closed or refused.
const RECONNECT: Duration = Duration::from_secs(5);

/// The analog input's level in a levels frame, on this crate's scale: the
/// RMS of the louder channel. `None` when the frame has no analog meter or
/// is cut short.
pub fn analog_level(frame: &[u8]) -> Option<u8> {
    let mut at = 0;
    while at + 2 <= frame.len() {
        let (id, channels) = (frame[at], usize::from(frame[at + 1]));
        // The id, the count, RMS and peak per channel, the clip bits.
        let end = at + 2 + 2 * channels + 1;
        let meter = frame.get(at..end)?;
        if id == ANALOG_METER {
            let rms = (0..channels).map(|c| meter[2 + 2 * c]).max()?;
            return Some(level::db_to_u8(aas_to_db(rms)));
        }
        at = end;
    }
    None
}

/// A level on aas's scale in dBFS; 0 is anything at or below -60.
fn aas_to_db(value: u8) -> f64 {
    if value == 0 {
        return level::MIN_DB;
    }
    AAS_MIN_DB + f64::from(value) / 255.0 * -AAS_MIN_DB
}

/// Starts following the analog meter at `url` and returns the shared level.
/// The level stays 0, Stopped, until the first frame arrives, and reads as 0
/// again once frames stop (see `LevelSource`).
pub fn start(url: String, stale_after: Duration) -> Arc<LevelSource> {
    let level = Arc::new(LevelSource::new(stale_after));
    let published = level.clone();
    tokio::spawn(async move {
        loop {
            if let Err(e) = follow(&url, &published).await {
                log::warn!("aas levels at {url}: {e}; retrying");
            }
            published.publish(0, Instant::now());
            tokio::time::sleep(RECONNECT).await;
        }
    });
    level
}

/// Reads frames from `url` until the daemon closes the connection.
async fn follow(url: &str, level: &LevelSource) -> anyhow::Result<()> {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await?;
    log::info!("following the analog input's level at {url}");
    while let Some(message) = ws.next().await {
        if let Message::Binary(frame) = message? {
            if let Some(value) = analog_level(&frame) {
                level.publish(value, Instant::now());
            }
        }
    }
    anyhow::bail!("the daemon closed the connection")
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::SinkExt;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    /// A stereo master and the analog input, as aas sends them.
    fn frame(analog: [u8; 4]) -> Vec<u8> {
        let [l, lp, r, rp] = analog;
        vec![1, 2, 200, 220, 200, 220, 0, 16, 2, l, lp, r, rp, 0]
    }

    #[test]
    fn the_analog_meter_is_found_after_the_outputs() {
        // aas 255 is 0 dBFS, the top of both scales.
        assert_eq!(analog_level(&frame([255, 255, 10, 20])), Some(255));
    }

    #[test]
    fn the_louder_channel_counts() {
        let quiet = analog_level(&frame([10, 20, 10, 20])).unwrap();
        let right = analog_level(&frame([10, 20, 128, 140])).unwrap();
        assert_eq!(right, level::db_to_u8(aas_to_db(128)));
        assert!(right > quiet);
    }

    #[test]
    fn the_scale_is_converted_to_minus_80() {
        assert_eq!(aas_to_db(0), level::MIN_DB);
        assert_eq!(aas_to_db(255), 0.0);
        // 51 is a fifth of the way up aas's 60 dB: -48 dBFS.
        assert!((aas_to_db(51) - -48.0).abs() < 1e-9);
        assert_eq!(analog_level(&frame([0, 0, 0, 0])), Some(0));
    }

    #[test]
    fn a_frame_without_the_analog_meter_gives_nothing() {
        assert_eq!(analog_level(&[1, 2, 200, 220, 200, 220, 0]), None);
        assert_eq!(analog_level(&[]), None);
    }

    #[test]
    fn a_frame_cut_short_gives_nothing() {
        let f = frame([100, 100, 100, 100]);
        assert_eq!(analog_level(&f[..f.len() - 1]), None);
        assert_eq!(analog_level(&f[..9]), None);
    }

    #[test]
    fn a_mono_analog_meter_is_read() {
        assert_eq!(analog_level(&[16, 1, 255, 255, 0]), Some(255));
    }

    #[tokio::test]
    async fn frames_from_the_daemon_set_the_level() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/levels", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            loop {
                let sent = ws.send(Message::Binary(frame([255, 255, 0, 0])));
                if sent.await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });
        let level = start(url, Duration::from_secs(2));
        let end = Instant::now() + Duration::from_secs(3);
        while level.current(Instant::now()) != 255 && Instant::now() < end {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(level.current(Instant::now()), 255);
    }

    #[tokio::test]
    async fn a_daemon_that_is_not_there_yet_is_tried_again() {
        // Bind and drop, so the port refuses at first, then listen on it.
        let addr = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap();
        let level = start(format!("ws://{addr}/levels"), Duration::from_secs(2));
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(level.current(Instant::now()), 0);
        let listener = TcpListener::bind(addr).await.unwrap();
        let (stream, _) = tokio::time::timeout(Duration::from_secs(8), listener.accept())
            .await
            .expect("no second try")
            .unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        ws.send(Message::Binary(frame([255, 255, 0, 0])))
            .await
            .unwrap();
        let end = Instant::now() + Duration::from_secs(2);
        while level.current(Instant::now()) != 255 && Instant::now() < end {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(level.current(Instant::now()), 255);
    }
}
