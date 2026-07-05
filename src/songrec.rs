#[derive(Debug, Clone, PartialEq)]
pub struct RecognizedTrack {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub genre: Option<String>,
}

pub fn parse_songrec_line(line: &str) -> Option<RecognizedTrack> {
    let trimmed = line.trim();
    if !trimmed.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let track = value.get("track")?;
    let title = track.get("title")?.as_str()?.to_string();
    let artist = track.get("subtitle")?.as_str()?.to_string();

    let genre = track
        .get("genres")
        .and_then(|g| g.get("primary"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let album = track
        .get("sections")
        .and_then(|s| s.as_array())
        .and_then(|sections| {
            sections.iter().find_map(|section| {
                section.get("metadata")?.as_array()?.iter().find_map(|entry| {
                    if entry.get("title")?.as_str()? == "Album" {
                        entry.get("text")?.as_str().map(|s| s.to_string())
                    } else {
                        None
                    }
                })
            })
        });

    Some(RecognizedTrack { title, artist, album, genre })
}

pub struct Deduper {
    last_title: Option<String>,
}

impl Deduper {
    pub fn new() -> Self {
        Deduper { last_title: None }
    }

    pub fn should_emit(&mut self, track: &RecognizedTrack) -> bool {
        if self.last_title.as_deref() == Some(track.title.as_str()) {
            false
        } else {
            self.last_title = Some(track.title.clone());
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str) -> RecognizedTrack {
        RecognizedTrack {
            title: title.to_string(),
            artist: "Artist".to_string(),
            album: None,
            genre: None,
        }
    }

    // Trimmed but structurally real shape, based on an actual songrec match
    // captured against the live analog input (Sophie Hunger - Headlights).
    const REAL_MATCH: &str = r#"{
        "track": {
            "title": "Headlights",
            "subtitle": "Sophie Hunger",
            "genres": { "primary": "Rock" },
            "sections": [
                {
                    "type": "SONG",
                    "metadata": [
                        { "title": "Album", "text": "1983" },
                        { "title": "Label", "text": "Two Gentlemen Records" },
                        { "title": "Released", "text": "2010" }
                    ]
                }
            ]
        }
    }"#;

    const MATCH_WITHOUT_OPTIONALS: &str = r#"{
        "track": {
            "title": "Some Song",
            "subtitle": "Some Artist"
        }
    }"#;

    #[test]
    fn parses_full_match() {
        let track = parse_songrec_line(REAL_MATCH).unwrap();
        assert_eq!(track.title, "Headlights");
        assert_eq!(track.artist, "Sophie Hunger");
        assert_eq!(track.genre.as_deref(), Some("Rock"));
        assert_eq!(track.album.as_deref(), Some("1983"));
    }

    #[test]
    fn parses_match_missing_optional_fields() {
        let track = parse_songrec_line(MATCH_WITHOUT_OPTIONALS).unwrap();
        assert_eq!(track.title, "Some Song");
        assert_eq!(track.artist, "Some Artist");
        assert_eq!(track.genre, None);
        assert_eq!(track.album, None);
    }

    #[test]
    fn ignores_non_json_log_lines() {
        let line = "[2026-07-05T16:21:39Z INFO songrec::cli_main] Recording started!";
        assert_eq!(parse_songrec_line(line), None);
    }

    #[test]
    fn ignores_malformed_json_without_panicking() {
        assert_eq!(parse_songrec_line("{not valid json"), None);
    }

    #[test]
    fn ignores_json_without_a_track_field() {
        assert_eq!(parse_songrec_line(r#"{"location": {}}"#), None);
    }

    #[test]
    fn first_track_is_always_emitted() {
        let mut d = Deduper::new();
        assert!(d.should_emit(&track("A")));
    }

    #[test]
    fn repeated_same_title_is_not_emitted_again() {
        let mut d = Deduper::new();
        assert!(d.should_emit(&track("A")));
        assert!(!d.should_emit(&track("A")));
        assert!(!d.should_emit(&track("A")));
    }

    #[test]
    fn new_title_is_emitted_after_a_change() {
        let mut d = Deduper::new();
        assert!(d.should_emit(&track("A")));
        assert!(!d.should_emit(&track("A")));
        assert!(d.should_emit(&track("B")));
        assert!(!d.should_emit(&track("B")));
        assert!(d.should_emit(&track("A"))); // back to A after B is a real change too
    }
}
