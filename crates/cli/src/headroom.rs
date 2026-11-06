//! Owner rule (2026-10-18): at least 16% of CPU, RAM and disk must stay free for normal use of the Mac. A heavy job asks here before it starts.
//! Linux reads `/proc/meminfo`, two `/proc/stat` samples and `df -k -P $HOME`; macOS keeps its tool-based path.
use anyhow::{Result, bail};
use std::process::Command;

pub const MIN_FREE_PERCENT: f64 = 16.0;
/// Swap in use above this share means the machine is already short of RAM.
pub const MAX_SWAP_PERCENT: f64 = 25.0;
/// Old swap pages stay in the swap file after a memory squeeze ends, so swap only counts while RAM is also getting tight.
pub const SWAP_RAM_FLOOR_PERCENT: f64 = 40.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Headroom {
    pub ram_free_percent: f64,
    pub cpu_idle_percent: f64,
    pub disk_free_percent: f64,
    pub ram_total_gb: f64,
    pub disk_total_gb: f64,
    /// Swap in use, as a share of the swap file now allocated.
    pub swap_used_percent: f64,
}

fn out(cmd: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(cmd).args(args).output()?;
    if !output.status.success() {
        bail!(
            "{cmd} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// "System-wide memory free percentage: 44%" -> 44.0
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub fn parse_memory_pressure(text: &str) -> Option<f64> {
    text.lines()
        .find_map(|l| l.split("free percentage:").nth(1))
        .and_then(|v| v.trim().trim_end_matches('%').parse::<f64>().ok())
        .filter(|v| v.is_finite() && (0.0..=100.0).contains(v))
}

/// "CPU usage: 5.2% user, 8.1% sys, 86.6% idle" -> 86.6
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub fn parse_cpu_idle(text: &str) -> Option<f64> {
    let l = text.lines().rfind(|l| l.contains("CPU usage"))?;
    l.split(',')
        .find(|p| p.contains("idle"))?
        .trim()
        .split('%')
        .next()?
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && (0.0..=100.0).contains(v))
}

/// `df -k` line of the data volume -> (free %, total GB), by blocks: used + available = capacity of what is usable.
pub fn parse_df(text: &str) -> Option<(f64, f64)> {
    let f: Vec<&str> = text.lines().nth(1)?.split_whitespace().collect();
    let (total, avail): (f64, f64) = (f.get(1)?.parse().ok()?, f.get(3)?.parse().ok()?);
    (total > 0.0 && avail >= 0.0 && avail <= total)
        .then(|| (100.0 * avail / total, total / 1_048_576.0))
}

/// "total = 4096.00M  used = 2915.69M  free = 1180.31M" -> used / total in percent
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub fn parse_swap(text: &str) -> Option<f64> {
    let num = |key: &str| -> Option<f64> {
        text.split(key)
            .nth(1)?
            .trim_start_matches([' ', '='])
            .split('M')
            .next()?
            .trim()
            .parse()
            .ok()
    };
    let (total, used) = (num("total")?, num("used")?);
    (total > 0.0 && used >= 0.0 && used <= total).then(|| 100.0 * used / total)
}

/// `/proc/meminfo` -> (MemAvailable / MemTotal in percent, MemTotal in GB, swap used percent).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_meminfo(text: &str) -> Option<(f64, f64, f64)> {
    let kb = |key: &str| -> Option<f64> {
        text.lines()
            .find(|l| l.starts_with(key))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    };
    let (total, avail) = (kb("MemTotal:")?, kb("MemAvailable:")?);
    if !(total > 0.0 && (0.0..=total).contains(&avail)) {
        return None;
    }
    let swap = match (kb("SwapTotal:"), kb("SwapFree:")) {
        (Some(t), Some(f)) if t > 0.0 && (0.0..=t).contains(&f) => 100.0 * (t - f) / t,
        _ => 0.0,
    };
    Some((100.0 * avail / total, total / 1_048_576.0, swap))
}

/// First `cpu ` line of `/proc/stat` -> (idle + iowait, total) jiffies.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_proc_stat(text: &str) -> Option<(u64, u64)> {
    let l = text.lines().find(|l| l.starts_with("cpu "))?;
    let v: Vec<u64> = l.split_whitespace().skip(1).map(|x| x.parse().ok()).collect::<Option<_>>()?;
    (v.len() >= 4).then(|| (v[3] + v.get(4).copied().unwrap_or(0), v.iter().sum()))
}

/// Idle percent between two `/proc/stat` samples.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn cpu_idle_between(a: (u64, u64), b: (u64, u64)) -> Option<f64> {
    let (di, dt) = (b.0.checked_sub(a.0)?, b.1.checked_sub(a.1)?);
    (dt > 0 && di <= dt).then(|| 100.0 * di as f64 / dt as f64)
}

#[cfg(target_os = "linux")]
pub fn measure() -> Result<Headroom> {
    let read = |p: &str| std::fs::read_to_string(p).map_err(|e| anyhow::anyhow!("{p}: {e}"));
    let Some((ram, ram_total, swap)) = parse_meminfo(&read("/proc/meminfo")?) else {
        bail!("could not parse /proc/meminfo")
    };
    let a = parse_proc_stat(&read("/proc/stat")?);
    std::thread::sleep(std::time::Duration::from_secs(1));
    let b = parse_proc_stat(&read("/proc/stat")?);
    let Some(cpu) = a.zip(b).and_then(|(a, b)| cpu_idle_between(a, b)) else {
        bail!("could not read CPU usage from /proc/stat")
    };
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    let (disk, disk_gb) = parse_df(&out("df", &["-k", "-P", &home])?).unwrap_or((0.0, 0.0));
    Ok(Headroom {
        ram_free_percent: ram,
        cpu_idle_percent: cpu,
        disk_free_percent: disk,
        ram_total_gb: ram_total,
        disk_total_gb: disk_gb,
        swap_used_percent: swap,
    })
}

#[cfg(not(target_os = "linux"))]
pub fn measure() -> Result<Headroom> {
    let ram = parse_memory_pressure(&out("memory_pressure", &[])?);
    let cpu = parse_cpu_idle(&out("top", &["-l", "2", "-n", "0", "-s", "1"])?);
    let (disk, disk_gb) =
        parse_df(&out("df", &["-k", "/System/Volumes/Data"])?).unwrap_or((0.0, 0.0));
    let ram_total: f64 = out("sysctl", &["-n", "hw.memsize"])?
        .trim()
        .parse::<f64>()
        .unwrap_or(0.0)
        / 1_073_741_824.0;
    let (Some(ram), Some(cpu)) = (ram, cpu) else {
        bail!("could not read memory pressure or CPU usage")
    };
    Ok(Headroom {
        ram_free_percent: ram,
        cpu_idle_percent: cpu,
        disk_free_percent: disk,
        ram_total_gb: ram_total,
        disk_total_gb: disk_gb,
        swap_used_percent: parse_swap(&out("sysctl", &["-n", "vm.swapusage"])?).unwrap_or(0.0),
    })
}

/// Err with the reason when a job needing `ram_gb` / `disk_gb` more would leave less than 16% of anything free.
pub fn allow(h: &Headroom, ram_gb: f64, disk_gb: f64) -> Result<()> {
    anyhow::ensure!(
        ram_gb.is_finite() && disk_gb.is_finite() && ram_gb >= 0.0 && disk_gb >= 0.0,
        "job resource requirements must be finite and non-negative"
    );
    anyhow::ensure!(
        h.ram_total_gb.is_finite()
            && h.ram_total_gb > 0.0
            && h.disk_total_gb.is_finite()
            && h.disk_total_gb > 0.0,
        "could not read positive RAM and disk capacities"
    );
    let ram_after = h.ram_free_percent - 100.0 * ram_gb / h.ram_total_gb.max(1.0);
    let disk_after = h.disk_free_percent - 100.0 * disk_gb / h.disk_total_gb.max(1.0);
    let mut why = Vec::new();
    if ram_after < MIN_FREE_PERCENT {
        why.push(format!(
            "RAM would be {ram_after:.0}% free (now {:.0}%, job needs {ram_gb} GB)",
            h.ram_free_percent
        ));
    }
    if disk_after < MIN_FREE_PERCENT {
        why.push(format!(
            "disk would be {disk_after:.0}% free (now {:.0}%, job needs {disk_gb} GB)",
            h.disk_free_percent
        ));
    }
    if h.swap_used_percent > MAX_SWAP_PERCENT && ram_after < SWAP_RAM_FLOOR_PERCENT {
        why.push(format!(
            "swap is already {:.0}% used: the machine is paging and RAM is short",
            h.swap_used_percent
        ));
    }
    if h.cpu_idle_percent < MIN_FREE_PERCENT {
        why.push(format!("CPU is only {:.0}% idle", h.cpu_idle_percent));
    }
    if why.is_empty() {
        Ok(())
    } else {
        bail!(
            "not enough headroom (keep {MIN_FREE_PERCENT}% free): {}",
            why.join("; ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_macos_tool_outputs() {
        assert_eq!(
            parse_memory_pressure(
                "The system has 17179869184 (1048576 pages)\nSystem-wide memory free percentage: 44%\n"
            ),
            Some(44.0)
        );
        assert_eq!(
            parse_cpu_idle("Processes: 1\nCPU usage: 5.2% user, 8.1% sys, 86.6% idle\n"),
            Some(86.6)
        );
        let (free, gb) = parse_df("Filesystem 1024-blocks Used Available Capacity\n/dev/disk3s1 238418576 133000000 82800000 62% /\n").unwrap();
        assert!(
            (free - 34.7).abs() < 0.1 && (gb - 227.4).abs() < 0.1,
            "{free} {gb}"
        );
        assert_eq!(parse_memory_pressure("nothing"), None);
        assert_eq!(parse_memory_pressure("free percentage: NaN%"), None);
        assert_eq!(
            parse_df("Filesystem blocks Used Available Capacity\n/dev 0 0 0 0% /\n"),
            None
        );
        let sw = parse_swap(
            "vm.swapusage: total = 4096.00M  used = 2915.69M  free = 1180.31M  (encrypted)",
        )
        .unwrap();
        assert!((sw - 71.2).abs() < 0.1, "{sw}");
    }

    #[test]
    fn parses_the_linux_proc_files() {
        let mi = "MemTotal:       24408328 kB\nMemFree:        16079760 kB\nMemAvailable:   12204164 kB\nSwapTotal:       2097216 kB\nSwapFree:        1572912 kB\n";
        let (free, gb, swap) = parse_meminfo(mi).unwrap();
        assert!((free - 50.0).abs() < 0.01, "{free}");
        assert!((gb - 23.28).abs() < 0.01, "{gb}");
        assert!((swap - 25.0).abs() < 0.01, "{swap}");
        let no_swap = "MemTotal: 100 kB\nMemAvailable: 30 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n";
        assert_eq!(parse_meminfo(no_swap).map(|m| m.2), Some(0.0));
        assert_eq!(parse_meminfo("MemTotal: 100 kB\nMemAvailable: 200 kB\n"), None);
        assert_eq!(parse_meminfo("nothing"), None);

        let a = parse_proc_stat("cpu  100 0 100 700 100 0 0 0 0 0\ncpu0 1 2 3 4\n").unwrap();
        assert_eq!(a, (800, 1000));
        let b = parse_proc_stat("cpu  200 0 200 1300 200 0 0 0 0 0\n").unwrap();
        assert!((cpu_idle_between(a, b).unwrap() - 100.0 * 700.0 / 900.0).abs() < 1e-9);
        assert_eq!(cpu_idle_between(b, a), None, "counters never go backwards");
        assert_eq!(parse_proc_stat("cpu0 1 2 3 4\n"), None);

        let (free, gb) = parse_df("Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/sda2 1953514584 24117248 1887436800 2% /home\n").unwrap();
        assert!(free > 96.0 && (gb - 1863.0).abs() < 1.0, "{free} {gb}");
    }

    #[test]
    fn a_job_is_refused_when_it_would_eat_into_the_reserve() {
        let h = Headroom {
            ram_free_percent: 41.0,
            cpu_idle_percent: 80.0,
            disk_free_percent: 34.0,
            ram_total_gb: 16.0,
            disk_total_gb: 228.0,
            swap_used_percent: 0.0,
        };
        assert!(allow(&h, 2.0, 5.0).is_ok());
        assert!(
            allow(&h, 5.0, 1.0)
                .unwrap_err()
                .to_string()
                .contains("RAM would be 10% free"),
            "a 5 GB model leaves too little RAM"
        );
        assert!(
            allow(&h, 1.0, 50.0)
                .unwrap_err()
                .to_string()
                .contains("disk would be")
        );
        let paging = Headroom {
            swap_used_percent: 71.0,
            ram_free_percent: 30.0,
            ..h
        };
        assert!(
            allow(&paging, 0.0, 0.0)
                .unwrap_err()
                .to_string()
                .contains("paging")
        );
        let calm = Headroom {
            swap_used_percent: 71.0,
            ram_free_percent: 80.0,
            ..h
        };
        assert!(
            allow(&calm, 1.0, 1.0).is_ok(),
            "old swap pages alone are not a reason to refuse"
        );
        let busy = Headroom {
            cpu_idle_percent: 10.0,
            ..h
        };
        assert!(
            allow(&busy, 0.0, 0.0)
                .unwrap_err()
                .to_string()
                .contains("CPU")
        );
        assert!(allow(&h, -1.0, 0.0).is_err());
    }
}
