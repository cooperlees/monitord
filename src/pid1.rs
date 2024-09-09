//! # pid1 module
//!
//! `pid1` uses procfs to get some statistics on Linux's more important
//! process pid1. These metrics can help ensure newer systemds don't regress
//! or show stange behavior. E.g. more file descriptors without more units.

use procfs::process::Process;

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq)]
pub struct Pid1Stats {
    pub cpu_time_kernel: u64,
    pub cpu_time_user: u64,
    pub memory_usage_bytes: u64,
    pub fd_count: u64,
    pub tasks: u64,
}

/// Get procfs info on pid 1 - <https://manpages.debian.org/buster/manpages/procfs.5.en.html>
pub fn get_pid1_stats() -> anyhow::Result<Pid1Stats> {
    let bytes_per_page = procfs::page_size();
    let ticks_per_second = procfs::ticks_per_second();

    let pid1_proc = Process::new(1)?;
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

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    pub fn test_get_stats() -> anyhow::Result<()> {
        let pid1_stats = get_pid1_stats()?;
        assert!(pid1_stats.tasks > 0);
        Ok(())
    }
}
