//! Opt-in per-frame pacing trace of a native daemon.
//!
//! With `FREEMIXD_PACING_TRACE=1` in its environment, a native daemon keeps
//! how late every Program frame it realized started against its pacer
//! deadline, and how long realizing it took, and prints them once at
//! shutdown as one `FREEMIXD_PACING_TRACE` record. The whole-run telemetry
//! keeps only its newest samples; the trace keeps the startup frames too, so
//! a test can tell startup stalls from steady-state ones, and it marks the
//! frames commands were realized on.
//!
//! Where the host reports per-thread scheduling statistics (Linux
//! `schedstat`), each frame also carries how long the frame loop's own thread
//! ran on a CPU while realizing it, and how long the daemon's threads
//! together waited for a CPU since the frame before it was realized (for the
//! first frame, since the pacer started), which spans both the frame's
//! lateness and its realization. The first is work the loop itself did; the
//! second is a host too loaded to run the daemon when it was ready, which
//! makes a frame late or stretches its realization without the daemon doing
//! anything more.

use std::time::{Duration, Instant};

/// The environment variable that enables the trace.
pub(crate) const PACING_TRACE_ENV: &str = "FREEMIXD_PACING_TRACE";
/// Frames the trace keeps: an hour at 60 fps. Later frames are counted only.
const MAX_TRACED_FRAMES: usize = 60 * 60 * 60;

/// One realized frame: lateness and realization time, in microseconds, the
/// CPU use of its realization where known, and whether a command was
/// realized on it.
#[derive(Clone, Copy)]
struct FrameSample {
    lateness_us: u32,
    realize_us: u32,
    cpu: Option<CpuUse>,
    command: bool,
}

/// CPU use of one frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CpuUse {
    /// Time the frame loop's thread ran on a CPU realizing it.
    loop_on_cpu: Duration,
    /// Time the daemon's threads, summed, were runnable but waited for a CPU
    /// since the frame before it was realized.
    queued: Duration,
}

/// The cumulative scheduling counters [`CpuUse`] is taken from.
#[derive(Clone, Copy)]
pub(crate) struct CpuCounters {
    loop_on_cpu_ns: u64,
    queued_ns: u64,
}

impl CpuCounters {
    /// The counters now, read on the frame loop's thread; `None` where the
    /// host does not report them.
    #[cfg(target_os = "linux")]
    pub(crate) fn read() -> Option<Self> {
        /// A `schedstat` file's first two fields: nanoseconds on a CPU and
        /// nanoseconds runnable while waiting for one.
        fn schedstat(path: &std::path::Path) -> Option<(u64, u64)> {
            let text = std::fs::read_to_string(path).ok()?;
            let mut fields = text.split_ascii_whitespace().map(str::parse::<u64>);
            Some((fields.next()?.ok()?, fields.next()?.ok()?))
        }
        let (loop_on_cpu_ns, _) = schedstat(std::path::Path::new("/proc/thread-self/schedstat"))?;
        let mut queued_ns = 0_u64;
        for task in std::fs::read_dir("/proc/self/task").ok()?.flatten() {
            // A thread that exited meanwhile is simply left out.
            if let Some((_, queued)) = schedstat(&task.path().join("schedstat")) {
                queued_ns = queued_ns.saturating_add(queued);
            }
        }
        Some(Self {
            loop_on_cpu_ns,
            queued_ns,
        })
    }

    /// The counters now; this host does not report them.
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn read() -> Option<Self> {
        None
    }

    /// The loop thread's CPU time and the threads' CPU wait between `self`
    /// and the later `after`. A thread that exited in between takes its
    /// count with it, so the wait saturates at zero rather than going
    /// negative.
    fn until(self, after: Self) -> (Duration, Duration) {
        (
            Duration::from_nanos(after.loop_on_cpu_ns.saturating_sub(self.loop_on_cpu_ns)),
            Duration::from_nanos(after.queued_ns.saturating_sub(self.queued_ns)),
        )
    }
}

pub(crate) struct PacingTrace {
    samples: Vec<FrameSample>,
    frames: u64,
    /// The counters when the last traced frame was realized, or when the
    /// pacer started: where the next frame's CPU wait is counted from.
    span_start: Option<CpuCounters>,
}

fn micros(duration: Duration) -> u32 {
    u32::try_from(duration.as_micros()).unwrap_or(u32::MAX)
}

impl PacingTrace {
    /// The trace, when the environment asks for one.
    pub(crate) fn from_environment() -> Option<Self> {
        (std::env::var(PACING_TRACE_ENV).as_deref() == Ok("1")).then(|| Self {
            samples: Vec::with_capacity(4_096),
            frames: 0,
            span_start: None,
        })
    }

    /// Notes that the pacer started: the first frame's CPU wait is counted
    /// from here.
    pub(crate) fn start(&mut self) {
        self.span_start = CpuCounters::read();
    }

    /// Records one frame due at `deadline` and started at `started`, whose
    /// realization ran over `realized` (start and finish) between the
    /// scheduling `counters` read just before and after it, where the host
    /// reports them; `command` marks a command's frame.
    pub(crate) fn frame(
        &mut self,
        deadline: Instant,
        started: Instant,
        realized: (Instant, Instant),
        counters: Option<(CpuCounters, CpuCounters)>,
        command: bool,
    ) {
        let cpu = counters.map(|(before, after)| CpuUse {
            loop_on_cpu: before.until(after).0,
            queued: self.span_start.unwrap_or(before).until(after).1,
        });
        self.span_start = counters.map(|(_, after)| after);
        self.frames = self.frames.saturating_add(1);
        if self.samples.len() < MAX_TRACED_FRAMES {
            self.samples.push(FrameSample {
                lateness_us: micros(started.saturating_duration_since(deadline)),
                realize_us: micros(realized.1.saturating_duration_since(realized.0)),
                cpu,
                command,
            });
        }
    }

    /// The shutdown record: every traced frame as
    /// `lateness:realize:loop_cpu:queued` microseconds, in frame order, with
    /// `-` for CPU use the host does not report and `:c` after a command's
    /// frame.
    pub(crate) fn record(&self) -> String {
        let samples = self
            .samples
            .iter()
            .map(|sample| {
                let (loop_cpu, queued) = sample.cpu.map_or_else(
                    || ("-".to_owned(), "-".to_owned()),
                    |cpu| {
                        (
                            micros(cpu.loop_on_cpu).to_string(),
                            micros(cpu.queued).to_string(),
                        )
                    },
                );
                format!(
                    "{}:{}:{loop_cpu}:{queued}{}",
                    sample.lateness_us,
                    sample.realize_us,
                    if sample.command { ":c" } else { "" }
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "FREEMIXD_PACING_TRACE\tv=2\tframes={}\ttraced={}\tsamples={samples}",
            self.frames,
            self.samples.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_record_lists_every_frame_in_order() {
        let mut trace = PacingTrace {
            samples: Vec::new(),
            frames: 0,
            span_start: None,
        };
        let deadline = Instant::now();
        let started = deadline + Duration::from_micros(250);
        trace.frame(
            deadline,
            started,
            (
                started + Duration::from_micros(50),
                started + Duration::from_micros(1_050),
            ),
            Some((counters(1_000_000, 70_000), counters(1_600_000, 100_000))),
            false,
        );
        trace.frame(deadline, deadline, (deadline, deadline), None, true);
        assert_eq!(
            trace.record(),
            "FREEMIXD_PACING_TRACE\tv=2\tframes=2\ttraced=2\tsamples=250:1000:600:30,0:0:-:-:c"
        );
    }

    fn counters(loop_on_cpu_ns: u64, queued_ns: u64) -> CpuCounters {
        CpuCounters {
            loop_on_cpu_ns,
            queued_ns,
        }
    }

    /// The loop's CPU time is its realization's; the CPU wait runs from the
    /// previous frame's realization, or the pacer's start, to this one's
    /// end, and never goes negative.
    #[test]
    fn a_frames_cpu_wait_spans_the_time_since_the_previous_frame() {
        let mut trace = PacingTrace {
            samples: Vec::new(),
            frames: 0,
            span_start: Some(counters(0, 1_000_000)),
        };
        let now = Instant::now();
        trace.frame(
            now,
            now,
            (now, now),
            Some((
                counters(2_000_000, 4_000_000),
                counters(5_000_000, 9_000_000),
            )),
            false,
        );
        trace.frame(
            now,
            now,
            (now, now),
            Some((
                counters(6_000_000, 12_000_000),
                counters(7_000_000, 8_000_000),
            )),
            false,
        );
        assert!(
            trace.record().ends_with("samples=0:0:3000:8000,0:0:1000:0"),
            "{}",
            trace.record()
        );
    }

    /// On Linux the calling thread's CPU time grows while it spins.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_loop_threads_cpu_time_is_read_on_linux() {
        let before = CpuCounters::read().expect("Linux reports schedstat");
        let spin = Instant::now();
        while spin.elapsed() < Duration::from_millis(20) {
            std::hint::spin_loop();
        }
        let (loop_on_cpu, _) = before.until(CpuCounters::read().unwrap());
        assert!(loop_on_cpu >= Duration::from_millis(5), "{loop_on_cpu:?}");
    }
}
