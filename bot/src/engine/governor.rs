//! Process-wide CPU governor. A sampler thread measures this process's real
//! CPU time (all threads, 100% = one full core) and adjusts a pause fraction;
//! heavy work calls `checkpoint()` in its inner loop, which sleeps in
//! proportion so the whole bot stays at or below CPU_TARGET_PCT. Trading loops
//! never call checkpoint, so they are never slowed. `checkpoint` also enforces
//! a deadline so a stuck job aborts instead of hanging.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub const CPU_TARGET_PCT: f64 = 85.0;
const SAMPLE_EVERY: Duration = Duration::from_millis(500);
const WORK_SLICE: Duration = Duration::from_millis(20);
const MAX_PAUSE_FRACTION: f64 = 0.95;

pub struct Governor {
    cpu_pct: AtomicU64,
    pause_fraction: AtomicU64,
}

#[derive(Debug)]
pub struct DeadlineExceeded;

impl std::fmt::Display for DeadlineExceeded {
    /// Display text of a missed search deadline.
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "deadline exceeded")
    }
}
impl std::error::Error for DeadlineExceeded {}

/// Reads an f64 stored as bits in an atomic.
fn load(a: &AtomicU64) -> f64 {
    f64::from_bits(a.load(Ordering::Relaxed))
}
/// Stores an f64 as bits in an atomic.
fn store(a: &AtomicU64, v: f64) {
    a.store(v.to_bits(), Ordering::Relaxed)
}

/// User + system CPU seconds consumed by this process so far.
#[cfg(unix)]
pub fn process_cpu_seconds() -> f64 {
    // SAFETY: getrusage only writes into the zeroed struct we pass.
    let ru = unsafe {
        let mut ru: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut ru);
        ru
    };
    let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    tv(ru.ru_utime) + tv(ru.ru_stime)
}

/// User + kernel CPU seconds consumed by this process so far (Windows: GetProcessTimes, 100 ns units).
#[cfg(windows)]
pub fn process_cpu_seconds() -> f64 {
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn GetProcessTimes(
            process: isize,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
    }
    let (mut c, mut e, mut k, mut u) = (
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
    );
    // SAFETY: GetProcessTimes only writes into the four structs we pass for the current process handle.
    let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
    if ok == 0 {
        return 0.0;
    }
    let secs = |t: FileTime| (((t.high as u64) << 32) | t.low as u64) as f64 / 1e7;
    secs(k) + secs(u)
}

thread_local! {
    static LAST_PAUSE: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

impl Governor {
    /// Process CPU use (%) at the last measurement.
    pub fn cpu_pct(&self) -> f64 {
        load(&self.cpu_pct)
    }

    /// Share of time the governor currently pauses work to hold the CPU target.
    pub fn pause_fraction(&self) -> f64 {
        load(&self.pause_fraction)
    }

    /// Integral controller: raise the pause fraction while above target, lower it below.
    fn update(&self, cpu: f64) {
        store(&self.cpu_pct, cpu);
        let p =
            (self.pause_fraction() + 0.004 * (cpu - CPU_TARGET_PCT)).clamp(0.0, MAX_PAUSE_FRACTION);
        store(&self.pause_fraction, p);
    }

    /// Call often from CPU-heavy loops (never from the trading loop).
    pub fn checkpoint(&self, deadline: Instant) -> Result<(), DeadlineExceeded> {
        let now = Instant::now();
        if now >= deadline {
            return Err(DeadlineExceeded);
        }
        if UNTHROTTLED.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(());
        }
        LAST_PAUSE.with(|last| {
            let since = last.get().map_or(WORK_SLICE, |t| now.duration_since(t));
            if since < WORK_SLICE {
                return;
            }
            let p = self.pause_fraction();
            if p > 0.0 {
                std::thread::sleep(since.mul_f64(p / (1.0 - p)).min(Duration::from_millis(500)));
            }
            last.set(Some(Instant::now()));
        });
        Ok(())
    }
}

/// Research tools only: skip the pauses so a study can use every core (the live service never sets it and stays capped).
pub static UNTHROTTLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The global governor; starts its sampler thread on first use.
pub fn global() -> &'static Governor {
    static GOV: OnceLock<Governor> = OnceLock::new();
    GOV.get_or_init(|| {
        std::thread::Builder::new()
            .name("cpu-governor".into())
            .spawn(|| {
                let (mut t0, mut c0) = (Instant::now(), process_cpu_seconds());
                loop {
                    std::thread::sleep(SAMPLE_EVERY);
                    let (t1, c1) = (Instant::now(), process_cpu_seconds());
                    let wall = t1.duration_since(t0).as_secs_f64();
                    if wall > 0.0 {
                        global().update((c1 - c0) / wall * 100.0);
                    }
                    (t0, c0) = (t1, c1);
                }
            })
            .expect("spawn cpu-governor thread");
        Governor {
            cpu_pct: AtomicU64::new(0f64.to_bits()),
            pause_fraction: AtomicU64::new(0f64.to_bits()),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: busy loop is held near target.
    #[test]
    fn busy_loop_is_held_near_target() {
        let g = global();
        let deadline = Instant::now() + Duration::from_secs(60);
        let start = (Instant::now(), process_cpu_seconds());
        let mut x = 0u64;
        while start.0.elapsed() < Duration::from_secs(6) {
            for i in 0..20_000 {
                x = x.wrapping_mul(31).wrapping_add(i);
            }
            g.checkpoint(deadline).unwrap();
        }
        std::hint::black_box(x);
        // Measure the last 3 seconds, after the controller has settled.
        let (t, c) = (Instant::now(), process_cpu_seconds());
        while t.elapsed() < Duration::from_secs(3) {
            for i in 0..20_000 {
                x = x.wrapping_mul(31).wrapping_add(i);
            }
            g.checkpoint(deadline).unwrap();
        }
        std::hint::black_box(x);
        let pct = (process_cpu_seconds() - c) / t.elapsed().as_secs_f64() * 100.0;
        assert!(pct < CPU_TARGET_PCT + 10.0, "cpu {pct:.1}%");
    }

    /// Test: deadline aborts.
    #[test]
    fn deadline_aborts() {
        assert!(global()
            .checkpoint(Instant::now() - Duration::from_millis(1))
            .is_err());
    }
}
