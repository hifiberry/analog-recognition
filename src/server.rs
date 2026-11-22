//! Which audio server the device runs: PipeWire or aas.
//!
//! HiFiBerry OS stores the choice in one file, written by
//! `hifiberry-audio-server set pipewire|aas` and applied at the next boot, so
//! reading it once at start is enough. A missing or unreadable file means
//! PipeWire, as it does for the switch itself.

/// Where the choice is stored.
pub const CHOICE_FILE: &str = "/etc/hifiberry/audio-server";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioServer {
    PipeWire,
    Aas,
}

impl AudioServer {
    /// The server named by the choice file's contents, `None` when there is
    /// no file.
    pub fn from_choice(contents: Option<&str>) -> AudioServer {
        match contents.map(str::trim) {
            Some("aas") => AudioServer::Aas,
            _ => AudioServer::PipeWire,
        }
    }

    /// The server this device runs, from `CHOICE_FILE`.
    pub fn detect() -> AudioServer {
        AudioServer::from_choice(std::fs::read_to_string(CHOICE_FILE).ok().as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_choice_aas_is_aas() {
        assert_eq!(AudioServer::from_choice(Some("aas\n")), AudioServer::Aas);
        assert_eq!(AudioServer::from_choice(Some(" aas ")), AudioServer::Aas);
    }

    #[test]
    fn anything_else_is_pipewire() {
        assert_eq!(
            AudioServer::from_choice(Some("pipewire\n")),
            AudioServer::PipeWire
        );
        assert_eq!(AudioServer::from_choice(Some("")), AudioServer::PipeWire);
        assert_eq!(
            AudioServer::from_choice(Some("aasx")),
            AudioServer::PipeWire
        );
        assert_eq!(AudioServer::from_choice(None), AudioServer::PipeWire);
    }
}
