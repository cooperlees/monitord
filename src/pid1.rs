//! # pid1 module
//!
//! `pid1` uses procfs to get some statistics on Linux's more important
//! process pid1. These metrics can help ensure newer systemds don't regress
//! or show stange behavior. E.g. more file descriptors without more units.

use std::sync::Arc;

#[cfg(target_os = "linux")]
use procfs::process::Process;
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::error;

use crate::MachineStats;

#[derive(Error, Debug)]
pub enum MonitordPid1Error {
    #[cfg(target_os = "linux")]
    #[error("Procfs error: {0}")]
    ProcfsError(#[from] procfs::ProcError),
    #[error("Integer conversion error: {0}")]
    IntConversion(#[from] std::num::TryFromIntError),
}

/// Process-level statistics for PID 1 (systemd) read from procfs.
/// These metrics help detect regressions or anomalies in the init process itself.
/// Ref: <https://manpages.debian.org/buster/manpages/procfs.5.en.html>
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq)]
pub struct Pid1Stats {
    /// CPU time spent in kernel mode by PID 1, in seconds (from /proc/1/stat stime, converted from ticks)
    pub cpu_time_kernel: u64,
    /// CPU time spent in user mode by PID 1, in seconds (from /proc/1/stat utime, converted from ticks)
    pub cpu_time_user: u64,
    /// Resident set size of PID 1 in bytes (from /proc/1/stat rss, converted from pages)
    pub memory_usage_bytes: u64,
    /// Number of open file descriptors held by PID 1 (from /proc/1/fd/)
    pub fd_count: u64,
    /// Number of threads/tasks belonging to PID 1 (from /proc/1/task/)
    pub tasks: u64,
}

/// Get procfs info on pid 1 - <https://manpages.debian.org/buster/manpages/procfs.5.en.html>
#[cfg(target_os = "linux")]
pub fn get_pid_stats(pid: i32) -> Result<Pid1Stats, MonitordPid1Error> {
    let bytes_per_page = procfs::page_size();
    let ticks_per_second = procfs::ticks_per_second();

    let pid1_proc = Process::new(pid)?;
    let stat_file = pid1_proc.stat()?;

    // Living with integer rounding
    Ok(Pid1Stats {
        cpu_time_kernel: (stat_file.stime) / (ticks_per_second),
        cpu_time_user: (stat_file.utime) / (ticks_per_second),
        memory_usage_bytes: (stat_file.rss) * (bytes_per_page),
        fd_count: pid1_proc.fd_count()?.try_into()?,
        // Using 0 as impossible number of tasks
        tasks: pid1_proc
            .tasks()?
            .flatten()
            .collect::<Vec<_>>()
            .len()
            .try_into()?,
    })
}

#[cfg(not(target_os = "linux"))]
pub fn get_pid_stats(_pid: i32) -> Result<Pid1Stats, MonitordPid1Error> {
    error!("pid1 stats not supported on this OS");
    Ok(Pid1Stats::default())
}

/// Async wrapper than can update PID1 stats when passed a locked struct
pub async fn update_pid1_stats(
    pid: i32,
    locked_machine_stats: Arc<RwLock<MachineStats>>,
) -> anyhow::Result<()> {
    let pid1_stats = match tokio::task::spawn_blocking(move || get_pid_stats(pid)).await {
        Ok(p1s) => p1s,
        Err(err) => return Err(err.into()),
    };

    let mut machine_stats = locked_machine_stats.write().await;
    machine_stats.pid1 = match pid1_stats {
        Ok(s) => Some(s),
        Err(err) => {
            error!("Unable to set pid1 stats: {:?}", err);
            None
        }
    };

    Ok(())
}

#[cfg(target_os = "linux")]
#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    pub fn test_get_stats() -> Result<(), MonitordPid1Error> {
        let pid1_stats = get_pid_stats(1)?;
        assert!(pid1_stats.tasks > 0);
        Ok(())
    }
}
