//! Non-secret host counters. Never enumerate processes, disks or interfaces.
use serde::Serialize;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use sysinfo::System;

#[derive(Clone, Serialize)]
pub struct LiveHost {
    sampled_at: u64,
    cpu_percent: Option<f32>,
    memory_total_bytes: u64,
    memory_used_bytes: u64,
    uptime_seconds: u64,
}

#[derive(Default)]
struct Sampler {
    system: System,
    previous: Option<(Instant, LiveHost)>,
}

impl Sampler {
    fn sample(&mut self) -> LiveHost {
        if let Some((at, cached)) = &self.previous {
            if at.elapsed() < Duration::from_secs(1) {
                return cached.clone();
            }
        }
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        let cpu = self.system.global_cpu_usage();
        let sample = LiveHost {
            sampled_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            // CPU usage needs two samples. Do not present first-sample noise as live usage.
            cpu_percent: (self.previous.is_some() && cpu.is_finite())
                .then_some(cpu.clamp(0.0, 100.0)),
            memory_total_bytes: self.system.total_memory(),
            memory_used_bytes: self.system.used_memory(),
            uptime_seconds: System::uptime(),
        };
        self.previous = Some((Instant::now(), sample.clone()));
        sample
    }
}

pub fn live_host() -> Option<LiveHost> {
    static SAMPLER: OnceLock<Mutex<Sampler>> = OnceLock::new();
    SAMPLER
        .get_or_init(|| Mutex::new(Sampler::default()))
        .lock()
        .ok()
        .map(|mut sampler| sampler.sample())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_cpu_is_unknown_and_repeated_reads_are_cached() {
        let mut sampler = Sampler::default();
        let first = sampler.sample();
        assert!(first.cpu_percent.is_none());
        let second = sampler.sample();
        assert_eq!(first.sampled_at, second.sampled_at);
        assert_eq!(first.memory_used_bytes, second.memory_used_bytes);
        assert!(second.cpu_percent.is_none());
        assert!(second.memory_used_bytes <= second.memory_total_bytes);
    }
}
