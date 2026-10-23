use std::time::{Duration, Instant};

const STALE_AFTER: Duration = Duration::from_secs(1);
const SMOOTHING_WINDOW: f64 = 0.45;

#[derive(Default)]
pub struct TransferSpeed {
    previous: Option<(Instant, u64)>,
    bytes_per_second: u64,
}

impl TransferSpeed {
    pub fn update(&mut self, at: Instant, transferred: u64) {
        let Some((previous_at, previous_bytes)) = self.previous else {
            self.previous = Some((at, transferred));
            return;
        };
        if at <= previous_at || transferred < previous_bytes {
            self.bytes_per_second = 0;
            self.previous = Some((at, transferred));
            return;
        }

        let elapsed = at.duration_since(previous_at);
        let delta = transferred - previous_bytes;
        if elapsed > STALE_AFTER {
            self.bytes_per_second = 0;
        } else if !elapsed.is_zero() {
            let measured = delta as f64 / elapsed.as_secs_f64();
            let alpha = 1.0 - (-elapsed.as_secs_f64() / SMOOTHING_WINDOW).exp();
            let smoothed =
                self.bytes_per_second as f64 + alpha * (measured - self.bytes_per_second as f64);
            self.bytes_per_second = smoothed.max(0.0).round() as u64;
        }
        self.previous = Some((at, transferred));
    }

    pub fn expire(&mut self, now: Instant) -> bool {
        if self
            .previous
            .is_some_and(|(last_at, _)| now.saturating_duration_since(last_at) > STALE_AFTER)
            && self.bytes_per_second != 0
        {
            self.bytes_per_second = 0;
            self.previous = None;
            return true;
        }
        false
    }

    pub fn bytes_per_second(&self) -> u64 {
        self.bytes_per_second
    }

    pub fn available(&self) -> bool {
        self.previous.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_use_monotonic_byte_deltas_and_expire_after_a_stall() {
        let start = Instant::now();
        let mut speed = TransferSpeed::default();
        speed.update(start, 0);
        speed.update(start + Duration::from_millis(500), 500);
        assert!(speed.bytes_per_second() > 0 && speed.bytes_per_second() < 1000);
        assert!(speed.expire(start + Duration::from_millis(1600)));
        assert_eq!(speed.bytes_per_second(), 0);
    }

    #[test]
    fn counter_reset_and_long_gaps_start_a_fresh_sample() {
        let start = Instant::now();
        let mut speed = TransferSpeed::default();
        speed.update(start, 1000);
        speed.update(start + Duration::from_millis(100), 1100);
        assert!(speed.bytes_per_second() > 0);
        speed.update(start + Duration::from_millis(200), 10);
        assert_eq!(speed.bytes_per_second(), 0);
        speed.update(start + Duration::from_secs(2), 1000);
        assert_eq!(speed.bytes_per_second(), 0);
    }
}
