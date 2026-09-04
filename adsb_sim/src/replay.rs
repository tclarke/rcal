use std::time::{Duration, Instant};

/// Compute when to send a snapshot based on replay timing.
///
/// Returns the wall-clock `Instant` at which the snapshot should be emitted.
/// `data_t0` and `wall_t0` mark the start of the replay.
pub fn wall_send_time(
    snapshot_now: f64,
    data_t0: f64,
    wall_t0: Instant,
    speed_multiplier: f64,
) -> Instant {
    let data_elapsed = snapshot_now - data_t0;
    let wall_elapsed_secs = data_elapsed / speed_multiplier.max(f64::MIN_POSITIVE);
    wall_t0 + Duration::from_secs_f64(wall_elapsed_secs.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wall_send_time_1x() {
        let t0 = Instant::now();
        let send = wall_send_time(100.0, 0.0, t0, 1.0);
        let diff = send.duration_since(t0).as_secs_f64();
        assert!((diff - 100.0).abs() < 0.001);
    }

    #[test]
    fn test_wall_send_time_2x() {
        let t0 = Instant::now();
        let send = wall_send_time(100.0, 0.0, t0, 2.0);
        let diff = send.duration_since(t0).as_secs_f64();
        assert!((diff - 50.0).abs() < 0.001);
    }

    #[test]
    fn test_wall_send_time_with_offset() {
        let t0 = Instant::now();
        let send = wall_send_time(200.0, 100.0, t0, 1.0);
        let diff = send.duration_since(t0).as_secs_f64();
        assert!((diff - 100.0).abs() < 0.001);
    }
}
