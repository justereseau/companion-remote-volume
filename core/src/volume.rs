//! Conversions between the protocol's integer percent (0-100) and OS scalar volumes.

/// Percent (clamped to 0-100) to a 0.0-1.0 scalar. 0 maps to exactly 0.0.
#[allow(dead_code)] // used by the Windows and macOS backends only
pub fn percent_to_scalar(percent: u8) -> f32 {
    f32::from(percent.min(100)) / 100.0
}

/// Scalar (clamped to 0.0-1.0) to the nearest percent.
#[allow(dead_code)] // used by the Windows and macOS backends only
pub fn scalar_to_percent(scalar: f32) -> u8 {
    if scalar.is_nan() {
        return 0;
    }
    (scalar.clamp(0.0, 1.0) * 100.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_is_exactly_zero() {
        assert_eq!(percent_to_scalar(0), 0.0);
        assert_eq!(scalar_to_percent(0.0), 0);
    }

    #[test]
    fn full_scale() {
        assert_eq!(percent_to_scalar(100), 1.0);
        assert_eq!(scalar_to_percent(1.0), 100);
    }

    #[test]
    fn round_trips_every_percent() {
        for p in 0..=100u8 {
            assert_eq!(scalar_to_percent(percent_to_scalar(p)), p);
        }
    }

    #[test]
    fn clamps_out_of_range() {
        assert_eq!(percent_to_scalar(250), 1.0);
        assert_eq!(scalar_to_percent(1.7), 100);
        assert_eq!(scalar_to_percent(-0.3), 0);
        assert_eq!(scalar_to_percent(f32::NAN), 0);
    }
}
