use anyhow::Result;
use std::time::{Duration, Instant};

/// Download progress monitor for detecting stalled downloads
///
/// Monitors download progress and detects when download speed falls below acceptable threshold.
/// - First 10 seconds: No timeout checking (grace period for connection establishment)
/// - After 10 seconds: Check every 5 seconds, require at least 5KB progress in each 5-second window
pub struct DownloadMonitor {
    start_time: Instant,
    last_check: Instant,
    last_bytes: usize,

    // Hard-coded thresholds
    grace_period: Duration,     // 10 seconds before starting checks
    check_interval: Duration,   // Check every 5 seconds
    min_bytes_per_check: usize, // Minimum 5KB per 5-second window
}

impl Default for DownloadMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl DownloadMonitor {
    /// Create a new download monitor
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            start_time: now,
            last_check: now,
            last_bytes: 0,
            grace_period: Duration::from_secs(10),
            check_interval: Duration::from_secs(5),
            min_bytes_per_check: 5 * 1024, // 5KB
        }
    }

    /// Check if download has stalled based on current bytes transferred
    ///
    /// # Arguments
    /// * `current_bytes` - Total bytes downloaded so far
    ///
    /// # Returns
    /// * `Ok(())` - Download is progressing normally
    /// * `Err(anyhow::Error)` - Download has stalled and should be aborted
    pub fn check_stall(&mut self, current_bytes: usize) -> Result<()> {
        let now = Instant::now();

        // Grace period: don't check for first 10 seconds
        if now.duration_since(self.start_time) < self.grace_period {
            return Ok(());
        }

        // Check every 5 seconds
        if now.duration_since(self.last_check) >= self.check_interval {
            let bytes_transferred = current_bytes - self.last_bytes;

            if bytes_transferred < self.min_bytes_per_check {
                return Err(anyhow::anyhow!(
                    "Download stalled: only {} bytes transferred in last {} seconds",
                    bytes_transferred,
                    self.check_interval.as_secs()
                )
                .context("DOWNLOAD_STALL_ERR"));
            }

            // Update check point
            self.last_check = now;
            self.last_bytes = current_bytes;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grace_period() {
        let mut monitor = DownloadMonitor::new();

        // Should not fail during grace period even with 0 progress
        assert!(monitor.check_stall(0).is_ok());
        assert!(monitor.check_stall(100).is_ok());
    }

    #[test]
    fn test_normal_progress() {
        let mut monitor = DownloadMonitor::new();

        // Simulate time passing beyond grace period
        monitor.start_time = Instant::now() - Duration::from_secs(15);
        monitor.last_check = Instant::now() - Duration::from_secs(6);
        monitor.last_bytes = 0;

        // Should succeed with adequate progress (>5KB in 5+ seconds)
        assert!(monitor.check_stall(10 * 1024).is_ok());
    }

    #[test]
    fn test_stalled_download() {
        let mut monitor = DownloadMonitor::new();

        // Simulate time passing beyond grace period
        monitor.start_time = Instant::now() - Duration::from_secs(15);
        monitor.last_check = Instant::now() - Duration::from_secs(6);
        monitor.last_bytes = 1000;

        // Should fail with inadequate progress (<5KB in 5+ seconds)
        assert!(monitor.check_stall(1100).is_err());
    }
}
