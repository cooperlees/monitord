//! # cgroup module
//!
//! Per-service cgroup accounting read straight from cgroupfs instead of D-Bus
//! or varlink (issue #221, unblocking the zero-D-Bus goal in #37).
//!
//! Seven `ServiceStats` fields describe a service's cgroup, and every one of
//! them is a plain cgroup v2 file the kernel already exposes:
//!
//! | `ServiceStats` field | cgroupfs source |
//! |---|---|
//! | `cpuusage_nsec` | `cpu.stat` → `usage_usec` × 1000 |
//! | `memory_current` | `memory.current` |
//! | `memory_available` | `min(memory.max, memory.high) − memory.current`, minimized over the slice walk (see below) |
//! | `tasks_current` | `pids.current` |
//! | `processes` | recursive `cgroup.procs` walk + main PID fold-in |
//! | `ioread_bytes` | `io.stat` → `rbytes` summed over devices |
//! | `ioread_operations` | `io.stat` → `rios` summed over devices |
//!
//! systemd itself reads these files directly (`src/core/cgroup.c`:
//! `unit_get_cpu_usage_raw`, `unit_get_memory_accounting`,
//! `unit_get_memory_available`, `unit_get_tasks_current`,
//! `unit_get_io_accounting_raw`; D-Bus getters in `src/core/dbus-unit.c`
//! answer `u64::MAX` when the read fails). There is no accounting API
//! underneath to depend on — the accounting flags only control whether the
//! controller is enabled, which is itself visible as file presence. Dropping
//! the dependency on per-unit accounting flags being enabled is an explicit
//! goal of #221.
//!
//! ## `memory_available` semantics
//!
//! Replicates `unit_get_memory_available()`: walk the unit's cgroup directory
//! up through each ancestor slice to the filesystem root; at every level that
//! sets `memory.max` or `memory.high`, compute `min(max, high) − current` with
//! that level's own `memory.current`, and take the minimum. The root level
//! (the `-​-.slice` equivalent) is always constrained: its limit is host
//! physical memory (`MemTotal` from `/proc/meminfo`) and its current is host
//! memory pressure — which works out byte-exact to `MemAvailable` from
//! `/proc/meminfo` (verified live: D-Bus `MemoryAvailable` for a limitless
//! service equalled `MemAvailable` exactly). A unit with no limits anywhere
//! therefore reports host `MemAvailable`, matching D-Bus; a unit with
//! `MemoryMax=1G MemoryHigh=512M` reported `536870912 − 1368064 = 535502848`,
//! byte-exact with D-Bus.
//!
//! ## Why hand-rolled instead of a cgroupfs crate
//!
//! The ecosystem crates (`cgroups-rs` and friends) are synchronous —
//! `read_to_string` on cgroupfs is fast, but blocking the tokio runtime for
//! every service on every cycle is exactly what this collector avoids. An
//! async wrapper would just be `spawn_blocking` around the same file reads
//! below, plus another dependency and more binary size, against the embedded
//! goal. So: `tokio::fs` reads issued together with `tokio::join!`, zero new
//! dependencies, cgroup v2 only.
//!
//! ## `fs_root` and containers
//!
//! Like `collect_unit_files_stats`, every path is prefixed with `fs_root` —
//! empty for the host, `/proc/<leader>/root` for containers — so container
//! unit cgroups are readable without entering the PID namespace (#211). The
//! ancestor walk stops at the prefixed root (a container's systemd never sees
//! host slices above its own root), and the root level always uses the
//! *host* `/proc/meminfo`, matching what the container's PID 1 computes via
//! `physical_memory()` / `procfs_memory_get_used()`.
//!
//! ## Failure semantics
//!
//! Every field is `Option`: `None` means the file was absent or unparsable,
//! and callers map that to `u64::MAX` — the `[not set]` sentinel D-Bus uses.
//! `processes` is a plain count (`0` when the cgroup is gone), matching what
//! `GetProcesses` returns for a dead unit. Callers keep their existing
//! fallback (D-Bus props on the D-Bus path, the `Unit.List` reply on the
//! varlink path) for fields that come back `None`, which is also what covers
//! cgroup v1 hosts: no v2 files → all `None` → fallback, no new failure mode.

use std::collections::HashSet;
use std::path::Path;

use tracing::debug;

/// Accounting values for one service's cgroup, read from cgroupfs.
///
/// `None` per field means "no data" (file absent or unparsable); callers map
/// that to `u64::MAX`, the `[not set]` sentinel D-Bus reports for the same
/// situation. `processes` counts instead of measuring, so it is `0` — not
/// `None` — when the cgroup is gone, matching `GetProcesses` on a dead unit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CgroupStats {
    /// `cpu.stat` → `usage_usec` × 1000. No per-start baseline is subtracted:
    /// D-Bus subtracts the counter value captured at unit start, which is
    /// unknowable from the filesystem, so a reused cgroup reads marginally
    /// higher. Fresh cgroups (the common case) match exactly.
    pub cpu_usage_nsec: Option<u64>,
    /// `memory.current` verbatim.
    pub memory_current: Option<u64>,
    /// `min(memory.max, memory.high) − memory.current`, minimized over the
    /// unit's ancestor slices; the root level always binds at host
    /// `MemAvailable`. See the module docs for the full algorithm.
    pub memory_available: Option<u64>,
    /// `pids.current` verbatim.
    pub tasks_current: Option<u64>,
    /// Recursive `cgroup.procs` walk with the main PID folded in the way
    /// systemd does (it can sit outside the cgroup).
    pub processes: u32,
    /// `io.stat` → `rbytes` summed over all devices. `Some(0)` when the file
    /// exists but is empty (controller enabled, no IO yet — D-Bus reports `0`
    /// there too); `None` when the file is absent.
    pub io_read_bytes: Option<u64>,
    /// `io.stat` → `rios` summed over all devices, same presence semantics.
    pub io_read_operations: Option<u64>,
}

/// Sentinel both collectors use for "no data", matching the D-Bus `[not set]`
/// encoding (`u64::MAX`, rendered as `18446744073709551615` in JSON).
pub const UNSET: u64 = u64::MAX;

/// Read every cgroup accounting file for one service in a single parallel
/// batch.
///
/// `cgroup_path` is the unit's cgroup relative to the mount, e.g.
/// `/system.slice/chrony.service` (the `ControlGroup` D-Bus property or
/// `runtime.CGroup.Path` from `Unit.List`). `main_pid` folds the main process
/// in the way systemd does, since it can sit outside the cgroup.
///
/// The unit-level file reads run concurrently via `tokio::join!`, and callers
/// join this future with their in-flight D-Bus/varlink calls, so filesystem
/// IO never serializes behind IPC.
pub async fn read_service_cgroup(
    fs_root: &str,
    cgroup_path: &str,
    main_pid: Option<u32>,
) -> CgroupStats {
    let dir = format!("{fs_root}/sys/fs/cgroup{cgroup_path}");
    let dir = dir.as_str();

    let (cpu, memory_current, tasks, io_stat, processes) = tokio::join!(
        async { read_file(dir, "cpu.stat").await },
        async { read_file(dir, "memory.current").await },
        async { read_file(dir, "pids.current").await },
        async { read_file(dir, "io.stat").await },
        async { count_processes(dir, main_pid).await },
    );

    let cpu_usage_nsec = cpu.as_deref().and_then(parse_cpu_stat);
    let memory_current = memory_current.as_deref().and_then(parse_u64);
    let tasks_current = tasks.as_deref().and_then(parse_u64);
    let (io_read_bytes, io_read_operations) = match io_stat.as_deref() {
        Some(contents) => {
            let (bytes, ops) = parse_io_stat(contents);
            (Some(bytes), Some(ops))
        }
        None => (None, None),
    };

    // `memory_available` needs the ancestor chain as well (`memory.max` /
    // `memory.high` at every level up to the mount root). The walk is cheap
    // (a few small files up a shallow tree) and runs only for the units in
    // `[services]`.
    let memory_available = read_memory_available(fs_root, cgroup_path).await;

    CgroupStats {
        cpu_usage_nsec,
        memory_current,
        memory_available,
        tasks_current,
        processes,
        io_read_bytes,
        io_read_operations,
    }
}

/// Read one cgroup attribute file, returning `None` when it is absent or
/// unreadable. A missing file is routine (controller not enabled on this
/// subtree), so it logs at debug, not warn.
async fn read_file(dir: &str, file: &str) -> Option<String> {
    match tokio::fs::read_to_string(format!("{dir}/{file}")).await {
        Ok(contents) => Some(contents),
        Err(err) => {
            debug!("Unable to read {dir}/{file}: {err:?}");
            None
        }
    }
}

/// Parse `cpu.stat`, extracting `usage_usec` and converting to nanoseconds.
///
/// `usage_usec` is the field systemd reads (`unit_get_cpu_usage_raw`); the
/// sibling `user_usec`/`system_usec` lines are informational only.
fn parse_cpu_stat(contents: &str) -> Option<u64> {
    keyed_u64(contents, "usage_usec").and_then(|us| us.checked_mul(1000))
}

/// Parse a plain `u64` attribute file (`memory.current`, `pids.current`).
/// The trailing newline the kernel appends is trimmed; anything else that
/// does not parse is "no data" rather than an error.
fn parse_u64(contents: &str) -> Option<u64> {
    contents.trim().parse().ok()
}

/// Parse a memory limit file (`memory.max`, `memory.high`).
///
/// Returns `None` for both "max" (unlimited — no constraint from this level)
/// and unparsable content; `Some(limit)` otherwise. Callers treat `None` as
/// infinity when taking the `min(max, high)` per level.
fn parse_memory_limit(contents: &str) -> Option<u64> {
    let trimmed = contents.trim();
    if trimmed == "max" {
        return None;
    }
    trimmed.parse().ok()
}

/// Extract a `key value` field from cgroup keyed files (`cpu.stat` uses
/// `usage_usec 123`, `io.stat` uses `rbytes=123` — hence the two prefixes).
fn keyed_u64(contents: &str, key: &str) -> Option<u64> {
    let prefix = format!("{key} ");
    contents
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .and_then(|value| value.trim().parse().ok())
}

/// Sum `rbytes=` / `rios=` across every device line of `io.stat`.
///
/// Lines look like `8:0 rbytes=2936832 wbytes=278528 rios=87 wios=68 ...`;
/// systemd (`unit_get_io_accounting_raw`) skips the `major:minor` head and
/// sums the named fields over all devices. A present-but-empty file sums to
/// `0` (controller enabled, nothing accounted yet); an absent file never
/// reaches here — that is the `None` case, decided by the caller.
fn parse_io_stat(contents: &str) -> (u64, u64) {
    let mut bytes = 0u64;
    let mut operations = 0u64;
    for line in contents.lines() {
        // Skip the leading `major:minor` token; the rest are `key=value`.
        for word in line.split_whitespace().skip(1) {
            let (key, value) = match word.split_once('=') {
                Some(pair) => pair,
                None => continue,
            };
            let parsed: u64 = match value.parse() {
                Ok(value) => value,
                Err(_) => continue,
            };
            match key {
                "rbytes" => bytes = bytes.saturating_add(parsed),
                "rios" => operations = operations.saturating_add(parsed),
                _ => {}
            }
        }
    }
    (bytes, operations)
}

/// One level of the `memory_available` ancestor walk: the level's limit (the
/// `min(memory.max, memory.high)`, `None` when both are unlimited) and its
/// `memory.current` (`None` when unreadable, in which case systemd propagates
/// the previous — deeper — level's current as a lower bound).
struct MemoryLevel {
    limit: Option<u64>,
    current: Option<u64>,
}

/// Pure fold over the ancestor levels plus the root, mirroring
/// `unit_get_memory_available()`.
///
/// `levels` runs from the unit's own cgroup up to (excluding) the filesystem
/// root; `root` is the always-constrained top (`MemTotal`, host used bytes).
/// Levels with no limit are skipped; a level whose current is unreadable
/// reuses the deeper level's current. Returns `None` only when nothing was
/// readable at all — in practice the root level always binds, so this is
/// `Some` whenever `/proc/meminfo` parses.
fn fold_memory_available(levels: &[MemoryLevel], root: (u64, u64)) -> Option<u64> {
    let mut available = u64::MAX;
    let mut current_fallback: Option<u64> = None;
    for level in levels {
        if let Some(current) = level.current {
            current_fallback = Some(current);
        }
        let Some(limit) = level.limit else {
            continue;
        };
        // current_fallback is Some here whenever any level so far (including
        // this one) had a readable current; without one there is nothing to
        // subtract from, so the level contributes no constraint.
        if let Some(current) = current_fallback {
            available = available.min(limit.saturating_sub(current));
            if available == 0 {
                break;
            }
        }
    }
    let (root_limit, root_current) = root;
    available = available.min(root_limit.saturating_sub(root_current));
    (available != u64::MAX).then_some(available)
}

/// Read the host root-slice level: `(MemTotal, host used bytes)`.
///
/// Mirrors systemd's root-slice handling exactly: the limit is
/// `physical_memory()` (== `MemTotal`; verified `sysinfo.totalram` matches
/// it byte-exact on this kernel) and the current is `procfs_memory_get_used()`
/// (`src/basic/procfs-util.c`), which returns `MemTotal − MemAvailable`, so
/// the root level contributes `MemTotal − used == MemAvailable` — verified
/// live, D-Bus `MemoryAvailable` for a limitless service equalled
/// `MemAvailable` byte-exact.
///
/// Both numbers come from the host `/proc/meminfo` even for container reads:
/// a container's PID 1 computes the same two numbers from the same host
/// `/proc`.
fn read_host_memory() -> Option<(u64, u64)> {
    let contents = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut total = None;
    let mut available = None;
    for line in contents.lines() {
        let (key, rest) = line.split_once(':')?;
        // Values are in kB: `MemTotal:       11984296 kB`.
        let value: u64 = rest.split_whitespace().next()?.parse().ok()?;
        match key {
            "MemTotal" => total = value.checked_mul(1024),
            "MemAvailable" => available = value.checked_mul(1024),
            _ => {}
        }
        if total.is_some() && available.is_some() {
            break;
        }
    }
    match (total, available) {
        // `used = total − available`, exactly like `procfs_memory_get()`:
        // the fold below subtracts it from the limit again, so the root
        // level nets out to `MemAvailable`.
        (Some(total), Some(available)) => Some((total, total.saturating_sub(available))),
        _ => None,
    }
}

/// Compute `memory_available` for one service: read the unit's `memory.max` /
/// `memory.high` / `memory.current`, walk the ancestor slices up to (but not
/// past) the `fs_root`-prefixed mount root collecting the same triple, and
/// fold with the host root level.
///
/// The walk stops at the prefixed root rather than continuing into host
/// parents: a container's systemd only sees its own slice chain. Ancestor
/// directories are derived by popping path components, so a `..` in a hostile
/// `cgroup_path` cannot escape the mount — it just yields nonexistent
/// directories whose files read as absent.
///
/// One level's `memory.max` / `memory.high` / `memory.current` triple.
///
/// The level limit is `min(max, high)` where "max" or an absent file counts
/// as infinity: `None` when both are unlimited, in which case the level
/// constrains nothing (though its current still feeds the fallback for
/// levels above — see `fold_memory_available`).
async fn read_memory_level(dir: &str) -> MemoryLevel {
    let (max, high, mem_current) = tokio::join!(
        async { read_file(dir, "memory.max").await },
        async { read_file(dir, "memory.high").await },
        async { read_file(dir, "memory.current").await },
    );
    let limit = match (
        max.as_deref().and_then(parse_memory_limit),
        high.as_deref().and_then(parse_memory_limit),
    ) {
        (Some(max), Some(high)) => Some(max.min(high)),
        (Some(max), None) => Some(max),
        (None, Some(high)) => Some(high),
        (None, None) => None,
    };
    MemoryLevel {
        limit,
        current: mem_current.as_deref().and_then(parse_u64),
    }
}

async fn read_memory_available(fs_root: &str, cgroup_path: &str) -> Option<u64> {
    let mount = format!("{fs_root}/sys/fs/cgroup");
    // The unit's directory plus each ancestor slice up to (excluding) the
    // mount root (`/system.slice/foo` -> `/system.slice`). The walk stops
    // there rather than continuing into host parents: a container's systemd
    // only sees its own slice chain. systemd never consults the mount root's
    // own files either — `-​-.slice` is special-cased to physical memory —
    // so the host root level folded in below substitutes for it, which also
    // avoids expected-miss log noise for files that never exist there.
    let mut levels = Vec::new();
    let mut relative: &Path = Path::new(cgroup_path);
    loop {
        if relative.as_os_str() == "/" || relative.as_os_str().is_empty() {
            break;
        }
        let dir = format!("{mount}{}", relative.display());
        levels.push(read_memory_level(&dir).await);
        match relative.parent() {
            Some(parent) => relative = parent,
            // `parent()` of `/` is `None`; the next loop test ends the walk.
            _ => break,
        }
    }
    let root = read_host_memory()?;
    fold_memory_available(&levels, root)
}

/// Count processes in a unit's cgroup, including nested ones.
///
/// systemd's `GetProcesses` walks the whole subtree — a service that delegates
/// its cgroup and puts workers in children would be undercounted by reading
/// only its own `cgroup.procs`. The main PID is folded in the same way systemd
/// does, since it can sit outside the cgroup. `dir` is the already-prefixed
/// absolute cgroup directory.
async fn count_processes(dir: &str, main_pid: Option<u32>) -> u32 {
    let mut pids: HashSet<u32> = HashSet::new();
    let mut directories = vec![dir.to_string()];
    while let Some(directory) = directories.pop() {
        match tokio::fs::read_to_string(format!("{directory}/cgroup.procs")).await {
            Ok(contents) => {
                pids.extend(contents.lines().filter_map(|line| line.parse::<u32>().ok()));
            }
            Err(err) => debug!("Unable to read {directory}/cgroup.procs: {err:?}"),
        }
        let Ok(mut entries) = tokio::fs::read_dir(&directory).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry.file_type().await.is_ok_and(|kind| kind.is_dir()) {
                directories.push(entry.path().to_string_lossy().into_owned());
            }
        }
    }

    if let Some(main_pid) = main_pid {
        pids.insert(main_pid);
    }

    pids.len() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_parse_cpu_stat() {
        let contents = "usage_usec 8864\nuser_usec 1772\nsystem_usec 7091\n";
        assert_eq!(parse_cpu_stat(contents), Some(8_864_000));
        // Missing key is "no data", not zero.
        assert_eq!(parse_cpu_stat("user_usec 5\n"), None);
        assert_eq!(parse_cpu_stat(""), None);
        // Overflow saturates via checked_mul → None rather than wrapping.
        assert_eq!(parse_cpu_stat("usage_usec 18446744073709551615\n"), None);
    }

    #[test]
    fn test_parse_u64() {
        // The kernel appends a trailing newline.
        assert_eq!(parse_u64("1368064\n"), Some(1_368_064));
        assert_eq!(parse_u64("garbage\n"), None);
        assert_eq!(parse_u64(""), None);
    }

    #[test]
    fn test_parse_memory_limit() {
        assert_eq!(parse_memory_limit("1073741824\n"), Some(1_073_741_824));
        // "max" means unlimited: no constraint, not zero.
        assert_eq!(parse_memory_limit("max\n"), None);
        assert_eq!(parse_memory_limit("garbage\n"), None);
    }

    #[test]
    fn test_parse_io_stat() {
        // Two devices, values summed — the shape systemd parses.
        let contents = "8:0 rbytes=2936832 wbytes=278528 rios=87 wios=68 dbytes=0 dios=0\n\
                        252:0 rbytes=337215488 wbytes=898760704 rios=6622 wios=105892 dbytes=0 dios=0\n";
        assert_eq!(parse_io_stat(contents), (340_152_320, 6_709));
        // Present-but-empty (controller enabled, no IO yet) sums to zero.
        assert_eq!(parse_io_stat(""), (0, 0));
        // Malformed tokens are skipped, not fatal.
        assert_eq!(parse_io_stat("8:0 rbytes=abc rios=7\n"), (0, 7));
    }

    #[test]
    fn test_fold_memory_available_with_unit_limits() {
        // Mirrors the live test service: MemoryMax=1G MemoryHigh=512M,
        // current 1368064, ancestors unlimited.
        let levels = vec![
            MemoryLevel {
                limit: Some(536_870_912),
                current: Some(1_368_064),
            },
            MemoryLevel {
                limit: None,
                current: Some(2_183_753_728),
            },
        ];
        // Host root binds far above the unit limit, so the unit wins.
        // (MemTotal, used = MemTotal − MemAvailable.)
        let root = (12_271_919_104, 8_474_013_504);
        assert_eq!(
            fold_memory_available(&levels, root),
            Some(536_870_912 - 1_368_064)
        );
    }

    #[test]
    fn test_fold_memory_available_no_limits_uses_host_available() {
        // No limits anywhere: the root level nets out to MemAvailable
        // (MemTotal − used), exactly what D-Bus reports for a limitless
        // service.
        let levels = vec![
            MemoryLevel {
                limit: None,
                current: Some(4_227_072),
            },
            MemoryLevel {
                limit: None,
                current: Some(2_183_753_728),
            },
        ];
        let root = (12_271_919_104, 3_761_983_488);
        assert_eq!(fold_memory_available(&levels, root), Some(8_509_935_616));
    }

    #[test]
    fn test_fold_memory_available_takes_minimum_over_slices() {
        // A tighter ancestor slice wins over a looser unit limit.
        let levels = vec![
            MemoryLevel {
                limit: Some(1_073_741_824),
                current: Some(1_000_000),
            },
            MemoryLevel {
                limit: Some(100_000_000),
                current: Some(90_000_000),
            },
        ];
        let root = (12_271_919_104, 3_761_983_488);
        assert_eq!(fold_memory_available(&levels, root), Some(10_000_000));
    }

    #[test]
    fn test_fold_memory_available_saturates_rather_than_underflows() {
        // Over-limit cgroup: limit − current saturates at 0 (LESS_BY).
        let levels = vec![MemoryLevel {
            limit: Some(1_000),
            current: Some(2_000),
        }];
        let root = (12_271_919_104, 3_761_983_488);
        assert_eq!(fold_memory_available(&levels, root), Some(0));
    }

    /// Build a fake cgroup tree under a temp dir shaped like
    /// `$root/sys/fs/cgroup/system.slice/<unit>/…` and read it back through
    /// `read_service_cgroup`, proving the whole reader end to end without
    /// systemd.
    async fn fake_tree() -> (tempfile::TempDir, String) {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = tmp.path().to_str().unwrap().to_string();
        let unit = format!("{root}/sys/fs/cgroup/system.slice/fake.service");
        let child = format!("{unit}/worker");
        std::fs::create_dir_all(&child).unwrap();
        let write = |path: &str, contents: &str| {
            let mut file = std::fs::File::create(path).unwrap();
            file.write_all(contents.as_bytes()).unwrap();
        };
        write(
            &format!("{unit}/cpu.stat"),
            "usage_usec 8864\nuser_usec 1772\n",
        );
        write(&format!("{unit}/memory.current"), "1368064\n");
        write(&format!("{unit}/memory.max"), "1073741824\n");
        write(&format!("{unit}/memory.high"), "536870912\n");
        write(&format!("{unit}/pids.current"), "2\n");
        write(
            &format!("{unit}/io.stat"),
            "8:0 rbytes=100 wbytes=0 rios=3 wios=0\n",
        );
        write(&format!("{unit}/cgroup.procs"), "100\n101\n");
        // Nested child, as with delegated workers.
        write(&format!("{child}/cgroup.procs"), "102\n");
        // Ancestor slice with no limits of its own.
        let slice = format!("{root}/sys/fs/cgroup/system.slice");
        write(&format!("{slice}/memory.max"), "max\n");
        write(&format!("{slice}/memory.high"), "max\n");
        write(&format!("{slice}/memory.current"), "999999\n");
        (tmp, root)
    }

    #[tokio::test]
    async fn test_read_service_cgroup_from_fake_tree() {
        let (_tmp, root) = fake_tree().await;
        let stats = read_service_cgroup(&root, "/system.slice/fake.service", Some(103)).await;
        assert_eq!(stats.cpu_usage_nsec, Some(8_864_000));
        assert_eq!(stats.memory_current, Some(1_368_064));
        assert_eq!(stats.tasks_current, Some(2));
        assert_eq!(stats.io_read_bytes, Some(100));
        assert_eq!(stats.io_read_operations, Some(3));
        // cgroup.procs across the subtree (100, 101, 102) + folded-in main
        // PID 103, which sits outside the cgroup.
        assert_eq!(stats.processes, 4);
        // Unit limit binds: 512M − current; the host root level is above it.
        assert_eq!(stats.memory_available, Some(536_870_912 - 1_368_064));
    }

    #[tokio::test]
    async fn test_read_service_cgroup_missing_dir() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = tmp.path().to_str().unwrap().to_string();
        std::fs::create_dir_all(format!("{root}/sys/fs/cgroup")).unwrap();
        let stats = read_service_cgroup(&root, "/system.slice/gone.service", None).await;
        // No cgroup at all: every gauge is "no data", zero processes.
        assert_eq!(
            stats,
            CgroupStats {
                cpu_usage_nsec: None,
                memory_current: None,
                // …except memory_available, whose root level always binds via
                // the host /proc/meminfo.
                memory_available: stats.memory_available,
                tasks_current: None,
                processes: 0,
                io_read_bytes: None,
                io_read_operations: None,
            }
        );
        assert!(stats.memory_available.is_some());
    }

    #[tokio::test]
    async fn test_read_service_cgroup_absent_io_file_is_none() {
        let (_tmp, root) = fake_tree().await;
        std::fs::remove_file(format!(
            "{root}/sys/fs/cgroup/system.slice/fake.service/io.stat"
        ))
        .unwrap();
        let stats = read_service_cgroup(&root, "/system.slice/fake.service", None).await;
        // Absent file (controller not enabled) is "no data", distinct from
        // the present-but-empty zero.
        assert_eq!(stats.io_read_bytes, None);
        assert_eq!(stats.io_read_operations, None);
        // …while the other fields still read fine.
        assert_eq!(stats.cpu_usage_nsec, Some(8_864_000));
    }
}
