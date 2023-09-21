use procfs::process::Stat;
use std::io::Cursor;
use std::path::Path;

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq)]
pub struct Pid1Stats {
    pub cpu_time_kernel: u64,
    pub cpu_time_user: u64,
    pub memory_usage_bytes: u64,
    pub num_threads: u64,
}

/// Get procfs info on pid 1 - https://manpages.debian.org/buster/manpages/procfs.5.en.html
pub fn get_pid1_stats() -> anyhow::Result<Pid1Stats> {
    let bytes_per_page = procfs::page_size();
    let ticks_per_second = procfs::ticks_per_second();

    let path = Path::new("/proc/1/stat");
    let file_contents = std::fs::read(path)?;
    let readable_string = Cursor::new(file_contents);
    let stat_file = Stat::from_reader(readable_string)?;

    // Living with integer rounding
    Ok(Pid1Stats {
        cpu_time_kernel: (stat_file.stime) / (ticks_per_second),
        cpu_time_user: (stat_file.utime) / (ticks_per_second),
        memory_usage_bytes: (stat_file.rss) * (bytes_per_page),
        // Using 0 as impossible number of threads
        num_threads: stat_file.num_threads.try_into().unwrap_or(0),
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    pub fn test_get_stats() -> anyhow::Result<()> {
        let pid1_stats = get_pid1_stats()?;
        assert!(pid1_stats.num_threads > 0);
        Ok(())
    }
}
