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

/// The pids of every process that currently holds an audio capture stream, or
/// `None` when the dump could not be understood at all.
///
/// `pw-dump` reports the stream and the process that owns it as two separate
/// objects: the Node carries `media.class = Stream/Input/Audio` and a
/// `client.id`, and only the Client that `client.id` points at carries
/// `application.process.id`. So the two have to be joined, which is why this
/// takes the whole dump rather than scanning for one property.
///
/// The `None` matters as much as the pids. An empty list means "pw-dump was
/// read, and nothing is capturing"; a dump this cannot parse means nothing
/// about songrec at all. Returning an empty list for both would make a
/// pw-dump that prints a warning line, gets truncated, or changes shape in a
/// future release look exactly like a songrec that has lost its stream -- and
/// restart it every interval, forever, on a device that is working.
pub fn capture_stream_pids(pw_dump: &str) -> Option<Vec<u32>> {
    let objects = serde_json::from_str::<Vec<serde_json::Value>>(pw_dump).ok()?;

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
    Some(pids)
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
    /// Answers from a fixed script, then `true` once it runs out. Tests only.
    Scripted(std::sync::Mutex<std::collections::VecDeque<bool>>),
}

/// How long `pw-dump` gets before the check is abandoned.
///
/// A wedged PipeWire daemon -- as opposed to an absent one -- leaves `pw-dump`
/// blocked on its core sync with nothing on stdout. Without this the check
/// never returns, so the watchdog quietly stops watching and the service is
/// back to the behaviour this module exists to fix, with no log line saying so.
const PW_DUMP_TIMEOUT: Duration = Duration::from_secs(5);

impl StreamCheck {
    /// A check that answers from `answers` in order. Tests only.
    #[cfg(test)]
    pub fn scripted(answers: impl IntoIterator<Item = bool>) -> Self {
        StreamCheck::Scripted(std::sync::Mutex::new(answers.into_iter().collect()))
    }

    pub async fn holds_capture_stream(&self, pid: u32) -> bool {
        match self {
            StreamCheck::Fixed(answer) => *answer,
            StreamCheck::Scripted(answers) => {
                answers.lock().unwrap().pop_front().unwrap_or(true)
            }
            StreamCheck::PwDump { binary } => {
                // Every branch below that is not "pw-dump was read and songrec
                // is not in it" answers `true`. A check that could not be made
                // says nothing about songrec, and reporting it as "no stream"
                // would turn a broken diagnostic into a restart loop.
                let run = tokio::process::Command::new(binary)
                    .kill_on_drop(true)
                    .output();
                match tokio::time::timeout(PW_DUMP_TIMEOUT, run).await {
                    Ok(Ok(output)) if output.status.success() => {
                        let dump = String::from_utf8_lossy(&output.stdout);
                        match capture_stream_pids(&dump) {
                            Some(pids) => pids.contains(&pid),
                            None => {
                                log::warn!(
                                    "could not parse the output of {binary}; \
                                     skipping the stream check"
                                );
                                true
                            }
                        }
                    }
                    Ok(Ok(output)) => {
                        log::warn!(
                            "{binary} exited with {}; skipping the stream check",
                            output.status
                        );
                        true
                    }
                    Ok(Err(e)) => {
                        log::warn!("could not run {binary}: {e}; skipping the stream check");
                        true
                    }
                    Err(_) => {
                        log::warn!(
                            "{binary} did not finish within {PW_DUMP_TIMEOUT:?} -- PipeWire \
                             may be wedged; skipping the stream check"
                        );
                        true
                    }
                }
            }
        }
    }
}

/// How many checks in a row must come back empty before songrec is restarted.
///
/// `pw-dump` is an instantaneous snapshot of the graph, and a working songrec
/// can be absent from one: PipeWire re-creating the input-processor node, a
/// device profile switch, or songrec reopening its own stream all show up that
/// way. A restart costs the in-flight recognition plus the caller's backoff, so
/// one unlucky sample should not buy one. The failure this watches for lasts
/// hours, so insisting on consecutive misses costs a single interval of
/// detection latency and nothing else.
const REQUIRED_CONSECUTIVE_MISSES: u32 = 2;

/// Resolves once `pid` has been seen without a capture stream
/// `REQUIRED_CONSECUTIVE_MISSES` times running, having waited `grace` first and
/// then checked every `interval`.
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
    let mut misses = 0;
    loop {
        if check.holds_capture_stream(pid).await {
            misses = 0;
        } else {
            misses += 1;
            if misses >= REQUIRED_CONSECUTIVE_MISSES {
                return;
            }
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
        assert_eq!(capture_stream_pids(REAL_DUMP), Some(vec![40012]));
    }

    #[test]
    fn ignores_playback_streams() {
        // Node 125 is a Stream/Output/Audio, and it carries a client.id like
        // any other node -- so it is reachable by the same join and is
        // excluded on media.class alone, not because the fixture happens to
        // leave it unreachable. A watchdog that counted playback streams
        // would think songrec was fine whenever anything at all was playing.
        assert!(!capture_stream_pids(REAL_DUMP).unwrap().contains(&1589));
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
        assert_eq!(capture_stream_pids(dump), Some(Vec::new()));
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
        assert_eq!(capture_stream_pids(dump), Some(vec![40012]));
    }

    #[test]
    fn output_that_cannot_be_parsed_is_reported_as_unknown_not_as_no_streams() {
        // The distinction the watchdog turns on: "parsed, and songrec is not
        // in it" is grounds for a restart, "could not parse it at all" is not.
        // Collapsing the two would restart songrec every interval on any host
        // whose pw-dump prints something unexpected.
        assert_eq!(capture_stream_pids("not json at all"), None);
        assert_eq!(capture_stream_pids(""), None);
        assert_eq!(capture_stream_pids("{}"), None);
    }

    #[tokio::test]
    async fn a_pw_dump_that_cannot_be_parsed_is_not_read_as_a_missing_stream() {
        let check = StreamCheck::PwDump {
            binary: concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/fake_pw_dump_garbage.sh"
            )
            .to_string(),
        };
        assert!(check.holds_capture_stream(1).await);
    }

    #[tokio::test(start_paused = true)]
    async fn a_pw_dump_that_hangs_is_not_read_as_a_missing_stream() {
        // A wedged PipeWire daemon is a plausible variant of the failure this
        // watchdog exists for. Without a timeout the check never returns, the
        // watchdog silently stops checking, and nothing says so.
        let check = StreamCheck::PwDump {
            binary: concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/fake_pw_dump_hangs.sh"
            )
            .to_string(),
        };
        assert!(check.holds_capture_stream(1).await);
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
        assert_eq!(capture_stream_pids(dump), Some(Vec::new()));
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
    #[tokio::test(start_paused = true)]
    async fn a_single_missed_check_does_not_restart_songrec() {
        // pw-dump is an instantaneous snapshot. PipeWire re-creating a node,
        // a profile switch or songrec reopening its stream can all show up as
        // one empty sample on a songrec that is working perfectly well.
        let check = StreamCheck::scripted([false, true, false, true, false, true]);
        let watch = wait_until_stream_lost(
            1,
            &check,
            Duration::from_secs(20),
            Duration::from_secs(30),
        );
        let result = tokio::time::timeout(Duration::from_secs(60 * 60), watch).await;
        assert!(
            result.is_err(),
            "songrec was restarted on a single missed check"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn consecutive_missed_checks_do_restart_songrec() {
        // The other side of it: a genuinely lost stream stays lost, so the
        // misses run consecutively and the watchdog still fires promptly.
        let check = StreamCheck::scripted([true, false, false]);
        wait_until_stream_lost(
            1,
            &check,
            Duration::from_secs(20),
            Duration::from_secs(30),
        )
        .await;
    }

}
