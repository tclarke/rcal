use chrono::{DateTime, Duration, Utc};

/// Compute when to send a snapshot based on replay timing.
///
/// Returns the wall-clock `DateTime<Utc>` at which the snapshot should be emitted.
/// `data_t0` and `wall_t0` mark the start of the replay.
pub fn wall_send_time(
    data_t0: f64,
    data_t: f64,
    wall_t0: DateTime<Utc>,
    speed_multiplier: f64,
) -> DateTime<Utc> {
    let elapsed_secs = (data_t0 - data_t).max(0.0) / speed_multiplier.max(f64::MIN_POSITIVE);
    wall_t0 + Duration::milliseconds((elapsed_secs * 1000.0) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wall_send_time_1x() {
        let t0 = Utc::now();
        let send = wall_send_time(100.0, 0.0, t0, 1.0);
        let diff = (send - t0).num_milliseconds() as f64 / 1000.0;
        assert!((diff - 100.0).abs() < 0.001);
    }

    #[test]
    fn test_wall_send_time_2x() {
        let t0 = Utc::now();
        let send = wall_send_time(100.0, 0.0, t0, 2.0);
        let diff = (send - t0).num_milliseconds() as f64 / 1000.0;
        assert!((diff - 50.0).abs() < 0.001);
    }

    #[test]
    fn test_wall_send_time_with_offset() {
        let t0 = Utc::now();
        let send = wall_send_time(200.0, 100.0, t0, 1.0);
        let diff = (send - t0).num_milliseconds() as f64 / 1000.0;
        assert!((diff - 100.0).abs() < 0.001);
    }
}
