//! Rolling sample buffers behind the TUI history graph.

use std::{collections::VecDeque, time::Instant};

use crate::sensors::{Fan, Temps};

/// A capped series of `(seconds since start, value)` samples.
#[derive(Debug, Default)]
pub struct Series {
    samples: VecDeque<(f64, f64)>,
    capacity: usize,
}

impl Series {
    fn new(capacity: usize) -> Self {
        Self { samples: VecDeque::with_capacity(capacity), capacity }
    }

    fn push(&mut self, at: f64, value: f64) {
        while self.samples.len() >= self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back((at, value));
    }

    /// The samples as a contiguous buffer, ready for a chart dataset.
    pub fn points(&self) -> Vec<(f64, f64)> {
        self.samples.iter().copied().collect()
    }
}

#[derive(Debug)]
pub struct History {
    start: Instant,
    pub cpu: Series,
    pub gpu: Series,
    pub rpm: Series,
}

impl History {
    pub fn new(capacity: usize) -> Self {
        // Two points are the minimum a line chart can draw.
        let capacity = capacity.max(2);
        Self {
            start: Instant::now(),
            cpu: Series::new(capacity),
            gpu: Series::new(capacity),
            rpm: Series::new(capacity),
        }
    }

    /// Records one sample per available reading. Missing readings simply leave
    /// their series untouched.
    pub fn push(&mut self, temps: Temps, fans: &[Fan]) {
        let at = self.start.elapsed().as_secs_f64();
        if let Some(cpu) = temps.cpu {
            self.cpu.push(at, cpu.into());
        }
        if let Some(gpu) = temps.gpu {
            self.gpu.push(at, gpu.into());
        }
        if let Some(rpm) = fans.iter().map(|fan| fan.rpm).max() {
            self.rpm.push(at, rpm.into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_keep_only_the_most_recent_samples() {
        let mut history = History::new(3);
        for i in 0..5 {
            history.push(Temps { cpu: Some(i as f32), gpu: None }, &[]);
        }
        let cpu = history.cpu.points();
        assert_eq!(cpu.len(), 3);
        assert_eq!(cpu.iter().map(|&(_, v)| v).collect::<Vec<_>>(), vec![2.0, 3.0, 4.0]);
        assert!(history.gpu.points().is_empty());
        assert!(history.rpm.points().is_empty());
    }

    #[test]
    fn capacity_is_never_below_two() {
        let mut history = History::new(0);
        for _ in 0..4 {
            history.push(Temps { cpu: Some(1.0), gpu: None }, &[]);
        }
        assert_eq!(history.cpu.points().len(), 2);
    }

    #[test]
    fn fan_series_tracks_the_fastest_fan() {
        let mut history = History::new(4);
        let fans = [
            Fan { name: "Fan 1".into(), rpm: 1200 },
            Fan { name: "Fan 2".into(), rpm: 3000 },
        ];
        history.push(Temps::default(), &fans);
        assert_eq!(history.rpm.points()[0].1, 3000.0);
    }

    #[test]
    fn samples_advance_in_time() {
        let mut history = History::new(4);
        history.push(Temps { cpu: Some(40.0), gpu: None }, &[]);
        history.push(Temps { cpu: Some(41.0), gpu: None }, &[]);
        let points = history.cpu.points();
        assert!(points[1].0 >= points[0].0);
    }
}
