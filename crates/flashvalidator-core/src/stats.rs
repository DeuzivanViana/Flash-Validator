//! Timing statistics for read and write operations.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Collected latency samples for one kind of operation.
#[derive(Debug, Clone, Default)]
pub struct Samples {
    micros: Vec<f64>,
}

impl Samples {
    /// Records one operation's duration.
    pub fn push(&mut self, d: Duration) {
        self.micros.push(d.as_secs_f64() * 1e6);
    }

    /// Number of samples.
    pub fn len(&self) -> usize {
        self.micros.len()
    }

    /// Whether no sample was recorded.
    pub fn is_empty(&self) -> bool {
        self.micros.is_empty()
    }

    /// Removes the most recent samples so only `len` remain.
    pub fn truncate(&mut self, len: usize) {
        self.micros.truncate(len);
    }

    /// Computes the summary, or `None` if there are no samples.
    pub fn summary(&self) -> Option<Summary> {
        if self.micros.is_empty() {
            return None;
        }
        let n = self.micros.len() as f64;
        let total: f64 = self.micros.iter().sum();
        let mean = total / n;
        let variance = self.micros.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
        let mut sorted = self.micros.clone();
        sorted.sort_by(f64::total_cmp);
        let mid = sorted.len() / 2;
        let median = if sorted.len() % 2 == 0 { (sorted[mid - 1] + sorted[mid]) / 2.0 } else { sorted[mid] };
        Some(Summary {
            samples: self.micros.len(),
            min_us: sorted[0],
            max_us: *sorted.last().expect("non-empty"),
            mean_us: mean,
            median_us: median,
            std_dev_us: variance.sqrt(),
            variance_us2: variance,
            total_us: total,
        })
    }
}

/// Summary statistics, all in microseconds (variance in microseconds squared).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// Number of timed operations.
    pub samples: usize,
    /// Fastest operation.
    pub min_us: f64,
    /// Slowest operation.
    pub max_us: f64,
    /// Arithmetic mean.
    pub mean_us: f64,
    /// Median.
    pub median_us: f64,
    /// Population standard deviation.
    pub std_dev_us: f64,
    /// Population variance.
    pub variance_us2: f64,
    /// Sum of all durations.
    pub total_us: f64,
}

impl Summary {
    /// Throughput in bytes per second for transfers of `block_size` bytes.
    pub fn throughput(&self, block_size: usize) -> f64 {
        if self.total_us <= 0.0 { 0.0 } else { self.samples as f64 * block_size as f64 / (self.total_us / 1e6) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_values() {
        let mut s = Samples::default();
        for us in [10u64, 20, 30, 40] {
            s.push(Duration::from_micros(us));
        }
        let sum = s.summary().unwrap();
        assert_eq!(sum.samples, 4);
        assert!((sum.min_us - 10.0).abs() < 1e-6);
        assert!((sum.max_us - 40.0).abs() < 1e-6);
        assert!((sum.mean_us - 25.0).abs() < 1e-6);
        assert!((sum.median_us - 25.0).abs() < 1e-6);
        assert!((sum.variance_us2 - 125.0).abs() < 1e-6);
        assert!((sum.total_us - 100.0).abs() < 1e-6);
    }

    #[test]
    fn empty_has_no_summary() {
        assert!(Samples::default().summary().is_none());
    }
}
