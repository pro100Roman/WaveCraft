//! Process CPU load and resident memory sampling for the status bar.

use crate::CPU_LOAD_SAMPLE_INTERVAL;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub(crate) struct CpuLoadMonitor {
    pub(crate) last_wall_time: Instant,
    pub(crate) last_process_time: Option<Duration>,
    pub(crate) load_percent: Option<f64>,
}

#[derive(Debug)]
pub(crate) struct MemoryUsageMonitor {
    pub(crate) last_sample_time: Instant,
    pub(crate) resident_bytes: Option<u64>,
}

impl MemoryUsageMonitor {
    pub(crate) fn new() -> Self {
        Self {
            last_sample_time: Instant::now(),
            resident_bytes: process_resident_memory_bytes(),
        }
    }

    pub(crate) fn update(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_sample_time) < CPU_LOAD_SAMPLE_INTERVAL {
            return;
        }
        self.resident_bytes = process_resident_memory_bytes();
        self.last_sample_time = now;
    }

    pub(crate) fn label(&self) -> String {
        const KIBIBYTE: f64 = 1024.0;
        const MEBIBYTE: f64 = KIBIBYTE * 1024.0;
        const GIBIBYTE: f64 = MEBIBYTE * 1024.0;

        match self.resident_bytes {
            Some(bytes) if bytes as f64 >= GIBIBYTE => {
                format!("RAM {:.2} GiB", bytes as f64 / GIBIBYTE)
            }
            Some(bytes) if bytes as f64 >= MEBIBYTE => {
                format!("RAM {:.1} MiB", bytes as f64 / MEBIBYTE)
            }
            Some(bytes) => format!("RAM {:.0} KiB", bytes as f64 / KIBIBYTE),
            None => "RAM —".to_owned(),
        }
    }
}

impl CpuLoadMonitor {
    pub(crate) fn new() -> Self {
        Self {
            last_wall_time: Instant::now(),
            last_process_time: process_cpu_time(),
            load_percent: None,
        }
    }

    pub(crate) fn update(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_wall_time) < CPU_LOAD_SAMPLE_INTERVAL {
            return;
        }
        self.update_at(now, process_cpu_time());
    }

    fn update_at(&mut self, now: Instant, process_time: Option<Duration>) {
        let wall_delta = now.duration_since(self.last_wall_time);
        self.load_percent = self
            .last_process_time
            .zip(process_time)
            .and_then(|(previous, current)| current.checked_sub(previous))
            .filter(|_| !wall_delta.is_zero())
            .map(|cpu_delta| cpu_delta.as_secs_f64() / wall_delta.as_secs_f64() * 100.0);
        self.last_wall_time = now;
        self.last_process_time = process_time;
    }

    pub(crate) fn label(&self) -> String {
        match self.load_percent {
            Some(percent) if percent >= 100.0 => format!("CPU {percent:.0}%"),
            Some(percent) => format!("CPU {percent:.1}%"),
            None => "CPU —".to_owned(),
        }
    }
}

#[cfg(unix)]
fn process_cpu_time() -> Option<Duration> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return None;
    }
    let usage = unsafe { usage.assume_init() };
    let timeval_duration = |timeval: libc::timeval| {
        let seconds = u64::try_from(timeval.tv_sec).ok()?;
        let microseconds = u64::try_from(timeval.tv_usec).ok()?;
        Duration::from_secs(seconds).checked_add(Duration::from_micros(microseconds))
    };
    timeval_duration(usage.ru_utime)?.checked_add(timeval_duration(usage.ru_stime)?)
}

#[cfg(windows)]
fn process_cpu_time() -> Option<Duration> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    if unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    } == 0
    {
        return None;
    }
    let ticks =
        |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    let hundred_nanoseconds = ticks(kernel).checked_add(ticks(user))?;
    Some(Duration::from_nanos(
        hundred_nanoseconds.saturating_mul(100),
    ))
}

#[cfg(not(any(unix, windows)))]
fn process_cpu_time() -> Option<Duration> {
    None
}

#[cfg(target_os = "macos")]
#[allow(deprecated)] // libc exposes the Mach task port without another platform dependency.
fn process_resident_memory_bytes() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::mach_task_basic_info_data_t>::zeroed();
    let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
    let result = unsafe {
        libc::task_info(
            libc::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            info.as_mut_ptr().cast(),
            &mut count,
        )
    };
    if result != libc::KERN_SUCCESS {
        return None;
    }
    Some(unsafe { info.assume_init() }.resident_size)
}

#[cfg(target_os = "linux")]
fn process_resident_memory_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let resident_pages = statm.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    let page_size = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
    resident_pages.checked_mul(page_size)
}

#[cfg(windows)]
fn process_resident_memory_bytes() -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS>()).ok()?,
        ..PROCESS_MEMORY_COUNTERS::default()
    };
    if unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) } == 0 {
        return None;
    }
    u64::try_from(counters.WorkingSetSize).ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn process_resident_memory_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_load_monitor_compares_process_time_with_wall_time() {
        let start = Instant::now();
        let mut monitor = CpuLoadMonitor {
            last_wall_time: start,
            last_process_time: Some(Duration::from_secs(1)),
            load_percent: None,
        };

        monitor.update_at(
            start + Duration::from_secs(2),
            Some(Duration::from_millis(3_500)),
        );

        assert_eq!(monitor.load_percent, Some(125.0));
        assert_eq!(monitor.label(), "CPU 125%");
    }

    #[test]
    fn memory_usage_monitor_formats_resident_memory() {
        let monitor = MemoryUsageMonitor {
            last_sample_time: Instant::now(),
            resident_bytes: Some(384 * 1024 * 1024),
        };

        assert_eq!(monitor.label(), "RAM 384.0 MiB");
    }
}
