use std::time::{Duration, Instant};

pub struct Rate {
    at: Instant,
    bytes: u64,
    value: Option<u64>,
}
impl Rate {
    pub fn new(at: Instant) -> Self {
        Self {
            at,
            bytes: 0,
            value: None,
        }
    }
    pub fn sample(&mut self, at: Instant, bytes: u64) -> Option<u64> {
        let elapsed = at.duration_since(self.at);
        if elapsed >= Duration::from_secs(1) {
            self.value = Some(
                (bytes.saturating_sub(self.bytes) as f64 / elapsed.as_secs_f64()).round() as u64,
            );
            self.at = at;
            self.bytes = bytes;
        }
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn samples_elapsed_time_and_idle_zero() {
        let start = Instant::now();
        let mut rate = Rate::new(start);
        assert_eq!(rate.sample(start + Duration::from_millis(500), 400), None);
        assert_eq!(rate.sample(start + Duration::from_secs(2), 400), Some(200));
        assert_eq!(rate.sample(start + Duration::from_secs(3), 400), Some(0));
    }
}
