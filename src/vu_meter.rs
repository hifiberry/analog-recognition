#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelFrame {
    pub rms_left: u8,
    pub peak_left: u8,
    pub rms_right: u8,
    pub peak_right: u8,
    pub clip_left: bool,
    pub clip_right: bool,
    pub channels: u8,
}

impl LevelFrame {
    pub fn level(&self) -> u8 {
        self.rms_left.max(self.rms_right)
    }
}

pub fn parse_level_frame(bytes: &[u8; 6]) -> LevelFrame {
    LevelFrame {
        rms_left: bytes[0],
        peak_left: bytes[1],
        rms_right: bytes[2],
        peak_right: bytes[3],
        clip_left: bytes[4] & 0b01 != 0,
        clip_right: bytes[4] & 0b10 != 0,
        channels: bytes[5],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frame_fields() {
        // L rms=100 peak=120, R rms=80 peak=90, flags=0b01 (left clip), channels=2
        let bytes: [u8; 6] = [100, 120, 80, 90, 0b01, 2];
        let frame = parse_level_frame(&bytes);
        assert_eq!(frame.rms_left, 100);
        assert_eq!(frame.peak_left, 120);
        assert_eq!(frame.rms_right, 80);
        assert_eq!(frame.peak_right, 90);
        assert!(frame.clip_left);
        assert!(!frame.clip_right);
        assert_eq!(frame.channels, 2);
    }

    #[test]
    fn level_is_max_of_left_and_right_rms() {
        let bytes: [u8; 6] = [30, 0, 90, 0, 0, 2];
        let frame = parse_level_frame(&bytes);
        assert_eq!(frame.level(), 90);
    }

    #[test]
    fn all_zero_frame_is_silence() {
        let bytes: [u8; 6] = [0, 0, 0, 0, 0, 2];
        let frame = parse_level_frame(&bytes);
        assert_eq!(frame.level(), 0);
    }
}
