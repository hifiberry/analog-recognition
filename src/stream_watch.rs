//! Watches whether the songrec child still holds a capture stream.
//!
//! songrec is supervised by watching its stdout: `run_songrec_once` returns
//! when the pipe closes, and the caller restarts it. That covers songrec
//! exiting, and nothing else.
//!
//! It does not cover songrec staying alive with a dead stream, which is what
//! a PipeWire restart leaves behind. songrec reaches PipeWire through
//! pipewire-pulse, and pipewire-pulse survives a restart of the PipeWire
//! daemon and reconnects; the client's *stream* does not come back. songrec
//! neither notices nor exits, so its stdout never closes and no line ever
//! arrives on it. Observed on a device: songrec ran for 20 hours after such a
//! restart, holding no stream, reading silence, while the service reported
//! itself healthy and recognition was simply gone.
//!
//! A liveness check on the process cannot see that, because the process is
//! alive. What has to be checked is whether it is still *doing the work* --
//! here, whether the graph still has a capture stream belonging to that pid.

use std::collections::HashMap;
use std::time::Duration;

/// The pids of every process that currently holds an audio capture stream.
///
/// `pw-dump` reports the stream and the process that owns it as two separate
/// objects: the Node carries `media.class = Stream/Input/Audio` and a
/// `client.id`, and only the Client that `client.id` points at carries
/// `application.process.id`. So the two have to be joined, which is why this
/// takes the whole dump rather than scanning for one property.
pub fn capture_stream_pids(pw_dump: &str) -> Vec<u32> {
    let Ok(objects) = serde_json::from_str::<Vec<serde_json::Value>>(pw_dump) else {
        return Vec::new();
    };

    let mut pid_by_client: HashMap<i64, u32> = HashMap::new();
    for object in &objects {
        if object.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Client") {
            continue;
        }
        let Some(id) = object.get("id").and_then(|i| i.as_i64()) else {
            continue;
        };
        if let Some(pid) = props(object)
            .and_then(|p| p.get("application.process.id"))
            .and_then(as_u32)
        {
            pid_by_client.insert(id, pid);
        }
    }

    let mut pids = Vec::new();
    for object in &objects {
        let Some(props) = props(object) else { continue };
        if props.get("media.class").and_then(|c| c.as_str()) != Some("Stream/Input/Audio") {
            continue;
        }
        // `client.id` is a number in pw-dump's own output, but PipeWire
        // property values are strings at the protocol level and other tools
        // render them that way, so accept both rather than silently matching
        // nothing.
        if let Some(client_id) = props.get("client.id").and_then(as_i64) {
            if let Some(pid) = pid_by_client.get(&client_id) {
                pids.push(*pid);
            }
        }
    }
    pids
}

fn props(object: &serde_json::Value) -> Option<&serde_json::Map<String, serde_json::Value>> {
    object.get("info")?.get("props")?.as_object()
}

fn as_u32(value: &serde_json::Value) -> Option<u32> {
    as_i64(value).and_then(|n| u32::try_from(n).ok())
}

fn as_i64(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

/// How the watchdog answers "does this pid hold a capture stream?".
///
/// An enum rather than a trait so the test forms need no async trait
/// machinery, and so a test can assert on how many times it was asked.
pub enum StreamCheck {
    /// Ask PipeWire, via the named `pw-dump` binary.
    PwDump { binary: String },
    /// Always answers the same way. Tests only.
    Fixed(bool),
}

impl StreamCheck {
    pub async fn holds_capture_stream(&self, pid: u32) -> bool {
        match self {
            StreamCheck::Fixed(answer) => *answer,
            StreamCheck::PwDump { binary } => {
                let output = tokio::process::Command::new(binary).output().await;
                match output {
                    Ok(output) if output.status.success() => {
                        let dump = String::from_utf8_lossy(&output.stdout);
                        capture_stream_pids(&dump).contains(&pid)
                    }
                    Ok(output) => {
                        // A pw-dump that fails says nothing about songrec, so
                        // it must not be read as "no stream" -- that would
                        // restart songrec in a loop on a host where pw-dump
                        // is missing or broken.
                        log::warn!(
                            "{binary} exited with {}; skipping the stream check",
                            output.status
                        );
                        true
                    }
                    Err(e) => {
                        log::warn!("could not run {binary}: {e}; skipping the stream check");
                        true
                    }
                }
            }
        }
    }
}

/// Resolves once `pid` has been seen without a capture stream, having waited
/// `grace` first and then checked every `interval`.
///
/// Never resolves when `interval` is zero: that is how the watchdog is turned
/// off in config.
pub async fn wait_until_stream_lost(
    pid: u32,
    check: &StreamCheck,
    grace: Duration,
    interval: Duration,
) {
    if interval.is_zero() {
        std::future::pending::<()>().await;
    }
    // songrec does not have its stream the instant it is spawned -- it
    // connects, enumerates devices and then opens one. Checking before it has
    // had a chance to would restart it forever without it ever getting to
    // work.
    tokio::time::sleep(grace).await;
    loop {
        if !check.holds_capture_stream(pid).await {
            return;
        }
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from a real `pw-dump` on a device, with songrec capturing from
    /// the analog input processor's monitor. The shape that matters is the
    /// split: Node 116 carries the media.class and a client.id, and only
    /// Client 136 carries the pid.
    const REAL_DUMP: &str = r#"[
      {
        "id": 136,
        "type": "PipeWire:Interface:Client",
        "info": { "props": {
          "application.name": "PipeWire ALSA [songrec]",
          "application.process.binary": "songrec",
          "application.process.id": 40012,
          "object.id": 136
        } }
      },
      {
        "id": 116,
        "type": "PipeWire:Interface:Node",
        "info": { "props": {
          "application.name": "PipeWire ALSA [songrec]",
          "client.id": 136,
          "media.class": "Stream/Input/Audio",
          "node.name": "alsa_capture.songrec",
          "object.id": 116
        } }
      },
      {
        "id": 125,
        "type": "PipeWire:Interface:Node",
        "info": { "props": {
          "client.id": 135,
          "media.class": "Stream/Output/Audio",
          "node.name": "output.loopback-1589-13"
        } }
      },
      {
        "id": 135,
        "type": "PipeWire:Interface:Client",
        "info": { "props": {
          "application.name": "pipewire",
          "application.process.id": 1589
        } }
      }
    ]"#;

    #[test]
    fn finds_the_pid_behind_a_capture_stream() {
        assert_eq!(capture_stream_pids(REAL_DUMP), vec![40012]);
    }

    #[test]
    fn ignores_playback_streams() {
        // Node 125 is a Stream/Output/Audio, and it carries a client.id like
        // any other node -- so it is reachable by the same join and is
        // excluded on media.class alone, not because the fixture happens to
        // leave it unreachable. A watchdog that counted playback streams
        // would think songrec was fine whenever anything at all was playing.
        assert!(!capture_stream_pids(REAL_DUMP).contains(&1589));
    }

    #[test]
    fn a_client_with_no_capture_stream_is_not_reported() {
        // Client 136 on its own, with its Node removed: this is exactly the
        // state a PipeWire restart leaves songrec in -- still a connected
        // client, no stream.
        let dump = r#"[
          {
            "id": 136,
            "type": "PipeWire:Interface:Client",
            "info": { "props": { "application.process.id": 40012 } }
          }
        ]"#;
        assert!(capture_stream_pids(dump).is_empty());
    }

    #[test]
    fn accepts_a_client_id_rendered_as_a_string() {
        let dump = r#"[
          {
            "id": 136,
            "type": "PipeWire:Interface:Client",
            "info": { "props": { "application.process.id": "40012" } }
          },
          {
            "id": 116,
            "type": "PipeWire:Interface:Node",
            "info": { "props": {
              "client.id": "136",
              "media.class": "Stream/Input/Audio"
            } }
          }
        ]"#;
        assert_eq!(capture_stream_pids(dump), vec![40012]);
    }

    #[test]
    fn malformed_output_yields_no_pids_rather_than_panicking() {
        assert!(capture_stream_pids("not json at all").is_empty());
        assert!(capture_stream_pids("").is_empty());
        assert!(capture_stream_pids("{}").is_empty());
    }

    #[test]
    fn a_node_whose_client_is_missing_is_skipped() {
        let dump = r#"[
          {
            "id": 116,
            "type": "PipeWire:Interface:Node",
            "info": { "props": { "client.id": 999, "media.class": "Stream/Input/Audio" } }
          }
        ]"#;
        assert!(capture_stream_pids(dump).is_empty());
    }

    #[tokio::test]
    async fn a_pw_dump_that_cannot_be_run_is_not_read_as_a_missing_stream() {
        // Answering "no stream" when the check itself failed would restart
        // songrec every interval, forever, on any host where pw-dump is
        // missing -- turning a diagnostic into an outage.
        let check = StreamCheck::PwDump {
            binary: "/nonexistent/pw-dump".to_string(),
        };
        assert!(check.holds_capture_stream(1).await);
    }

    #[tokio::test(start_paused = true)]
    async fn resolves_once_the_stream_is_gone() {
        wait_until_stream_lost(
            1,
            &StreamCheck::Fixed(false),
            Duration::from_secs(20),
            Duration::from_secs(30),
        )
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn does_not_resolve_while_the_stream_is_there() {
        let watch = wait_until_stream_lost(
            1,
            &StreamCheck::Fixed(true),
            Duration::from_secs(20),
            Duration::from_secs(30),
        );
        let result = tokio::time::timeout(Duration::from_secs(60 * 60), watch).await;
        assert!(
            result.is_err(),
            "the watchdog fired while the stream was present"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn waits_out_the_grace_period_before_the_first_check() {
        // Without this, a songrec that has not yet opened its device is
        // restarted immediately, forever.
        let started = tokio::time::Instant::now();
        wait_until_stream_lost(
            1,
            &StreamCheck::Fixed(false),
            Duration::from_secs(20),
            Duration::from_secs(30),
        )
        .await;
        assert!(started.elapsed() >= Duration::from_secs(20));
    }

    #[tokio::test(start_paused = true)]
    async fn a_zero_interval_disables_the_watchdog() {
        let watch = wait_until_stream_lost(
            1,
            &StreamCheck::Fixed(false),
            Duration::from_secs(20),
            Duration::ZERO,
        );
        let result = tokio::time::timeout(Duration::from_secs(60 * 60 * 24), watch).await;
        assert!(
            result.is_err(),
            "the watchdog fired even though it was disabled"
        );
    }
}
