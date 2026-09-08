//! Turning captured audio samples into the 0-255 level the playback state
//! machine thresholds against.
//!
//! The scale is fixed at -60 dB..0 dB → 0..255, matching what the platform's
//! vu-meter uses, so a threshold value means the same dB whichever source
//! produced it.

pub const MIN_DB: f64 = -60.0;
pub const MAX_DB: f64 = 0.0;

/// RMS of one buffer of interleaved-then-deinterleaved samples, already scaled
/// to [-1.0, 1.0]. Returns the level as 0-255 (MIN_DB → 0, MAX_DB → 255).
pub fn rms_to_u8(sum_squares: f64, count: usize) -> u8 {
    if count == 0 {
        return 0;
    }
    let rms = (sum_squares / count as f64).sqrt();
    db_to_u8(amplitude_to_db(rms))
}

/// Amplitude ratio (0.0..1.0) to dBFS, floored at MIN_DB so digital silence
/// maps to the bottom of the scale rather than -inf.
pub fn amplitude_to_db(amplitude: f64) -> f64 {
    if amplitude <= 0.0 {
        return MIN_DB;
    }
    (20.0 * amplitude.log10()).clamp(MIN_DB, MAX_DB)
}

/// Map a dBFS value onto 0-255 across [MIN_DB, MAX_DB].
pub fn db_to_u8(db: f64) -> u8 {
    let range = MAX_DB - MIN_DB;
    let normalized = ((db - MIN_DB) / range).clamp(0.0, 1.0);
    (normalized * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_is_zero() {
        assert_eq!(rms_to_u8(0.0, 1024), 0);
        assert_eq!(db_to_u8(MIN_DB), 0);
        assert_eq!(rms_to_u8(1.0, 0), 0); // no samples
    }

    #[test]
    fn full_scale_is_max() {
        // A constant full-scale signal: every sample = 1.0, sum_squares = n.
        assert_eq!(rms_to_u8(1024.0, 1024), 255);
        assert_eq!(db_to_u8(MAX_DB), 255);
    }

    #[test]
    fn mid_scale_maps_linearly_in_db() {
        // -30 dB is the midpoint of -60..0, so ~127/128.
        let u = db_to_u8(-30.0);
        assert!((126..=129).contains(&u), "got {u}");
    }

    #[test]
    fn a_minus_47db_tone_sits_just_below_a_55_threshold() {
        // Sanity-check the tuning: the -47 dB detection point we chose is
        // value ~55 on this scale.
        assert_eq!(db_to_u8(-47.0), 55);
    }

    #[test]
    fn clamps_out_of_range_db() {
        assert_eq!(db_to_u8(10.0), 255);
        assert_eq!(db_to_u8(-120.0), 0);
    }
}
