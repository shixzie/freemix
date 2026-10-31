//! Process CPU utilization (`RC-015`).
//!
//! On Linux the daemon reads its own user and system CPU time from
//! `/proc/self/stat` (fields 14 and 15, in `USER_HZ` clock ticks) and divides
//! the growth between two samples by the wall time between them times the
//! logical CPUs the process may run on. The result is the daemon process's
//! share of the whole machine, `0..=1`. Child processes (`FFmpeg` encoders)
//! are included when `/proc` lists them, so a native daemon's encoders count
//! against the reading. Other platforms report no reading.

use std::time::{Duration, Instant};

/// Clock ticks per second in `/proc` CPU times. `USER_HZ` is fixed at 100 by
/// the Linux user-space ABI on every architecture the daemon supports.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const USER_HZ: u64 = 100;

/// Samples the process's CPU time between calls.
#[derive(Debug)]
pub struct CpuSampler {
    /// The previous reading and when it was taken.
    last: Option<(Instant, Duration)>,
    /// Logical CPUs available to the process, at least one.
    cpus: u32,
}

impl CpuSampler {
    #[must_use]
    pub fn new() -> Self {
        let cpus = std::thread::available_parallelism()
            .ok()
            .and_then(|count| u32::try_from(count.get()).ok())
            .unwrap_or(1);
        Self { last: None, cpus }
    }

    /// The process's CPU utilization since the previous call, `0..=1` of
    /// every logical CPU; `None` on the first call, when no wall time passed,
    /// and where the platform offers no reading.
    pub fn sample(&mut self, now: Instant) -> Option<f64> {
        self.sample_with(now, process_cpu_time())
    }

    fn sample_with(&mut self, now: Instant, cpu: Option<Duration>) -> Option<f64> {
        let Some(cpu) = cpu else {
            self.last = None;
            return None;
        };
        let previous = self.last.replace((now, cpu));
        let (at, used) = previous?;
        let wall = now.saturating_duration_since(at).as_secs_f64() * f64::from(self.cpus);
        if wall <= 0.0 {
            return None;
        }
        Some((cpu.saturating_sub(used).as_secs_f64() / wall).clamp(0.0, 1.0))
    }
}

impl Default for CpuSampler {
    fn default() -> Self {
        Self::new()
    }
}

/// The process's user plus system CPU time so far, with its children's.
#[cfg(target_os = "linux")]
fn process_cpu_time() -> Option<Duration> {
    let mut ticks = 0_u64;
    let mut seen = std::collections::BTreeSet::new();
    collect_cpu_ticks(std::process::id(), 0, &mut seen, &mut ticks);
    Some(Duration::from_millis(ticks.saturating_mul(1_000) / USER_HZ))
}

/// Most processes one sample sums, so a runaway fork cannot make sampling
/// unbounded.
#[cfg(target_os = "linux")]
const MAX_CPU_PROCESSES: usize = 128;
/// Deepest descendant chain one sample follows.
#[cfg(target_os = "linux")]
const MAX_CPU_DEPTH: u32 = 4;

/// Adds `pid`'s CPU ticks and its descendants' to `ticks`.
#[cfg(target_os = "linux")]
fn collect_cpu_ticks(
    pid: u32,
    depth: u32,
    seen: &mut std::collections::BTreeSet<u32>,
    ticks: &mut u64,
) {
    if depth > MAX_CPU_DEPTH || seen.len() >= MAX_CPU_PROCESSES || !seen.insert(pid) {
        return;
    }
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        && let Some(process) = proc_stat_cpu_ticks(&stat)
    {
        *ticks = ticks.saturating_add(process);
    }
    // A child belongs to the thread that spawned it, so every task's
    // children file is read.
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return;
    };
    for task in tasks.flatten() {
        let Ok(children) = std::fs::read_to_string(task.path().join("children")) else {
            continue;
        };
        for child in children.split_ascii_whitespace() {
            if let Ok(child) = child.parse::<u32>() {
                collect_cpu_ticks(child, depth + 1, seen, ticks);
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
const fn process_cpu_time() -> Option<Duration> {
    None
}

/// `utime + stime` from one `/proc/<pid>/stat` line. The command name in
/// field 2 is parenthesized and may itself hold spaces or parentheses, so
/// fields are counted after its last `)`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn proc_stat_cpu_ticks(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let mut fields = rest.split_ascii_whitespace();
    // `rest` starts at field 3 (state); utime and stime are fields 14 and 15.
    let utime: u64 = fields.nth(11)?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(utime.saturating_add(stime))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cpu_ticks_after_an_awkward_command_name() {
        let stat = "4242 (free mix) (d)) S 1 4242 4242 0 -1 4194560 1234 0 0 0 250 50 0 0 20 0 9 0 100 1000 200 18446744073709551615";
        assert_eq!(proc_stat_cpu_ticks(stat), Some(300));
        assert_eq!(proc_stat_cpu_ticks("4242 (freemixd) S 1 2"), None);
        assert_eq!(proc_stat_cpu_ticks("no command"), None);
    }

    #[test]
    fn utilization_is_cpu_time_over_wall_time_on_every_cpu() {
        let mut sampler = CpuSampler {
            last: None,
            cpus: 4,
        };
        let start = Instant::now();
        assert_eq!(
            sampler.sample_with(start, Some(Duration::from_secs(10))),
            None
        );
        // Two seconds of CPU time over one wall second on four CPUs.
        let half = sampler.sample_with(
            start + Duration::from_secs(1),
            Some(Duration::from_secs(12)),
        );
        assert_eq!(half, Some(0.5));
        // Never above one, even across a coarse tick boundary.
        let full = sampler.sample_with(
            start + Duration::from_secs(2),
            Some(Duration::from_secs(20)),
        );
        assert_eq!(full, Some(1.0));
        // No reading forgets the baseline.
        assert_eq!(
            sampler.sample_with(start + Duration::from_secs(3), None),
            None
        );
        assert_eq!(
            sampler.sample_with(
                start + Duration::from_secs(4),
                Some(Duration::from_secs(21))
            ),
            None
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_reports_the_process_cpu_time() {
        let before = process_cpu_time().expect("/proc/self/stat is readable");
        let mut spin = 0_u64;
        let until = Instant::now() + Duration::from_millis(50);
        while Instant::now() < until {
            spin = spin.wrapping_add(1);
        }
        assert!(spin > 0);
        assert!(process_cpu_time().expect("still readable") >= before);
    }
}
