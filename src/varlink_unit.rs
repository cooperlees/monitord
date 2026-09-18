//! # varlink_unit module
//!
//! Per-unit detail via the `io.systemd.Unit` varlink API on PID 1's socket
//! (systemd v258+). This is what the varlink path uses instead of the per-unit
//! D-Bus property fetches in `units.rs`.
//!
//! `Unit.List` can either stream every unit or answer for one named unit.
//! monitord asks per unit: measured on a test host with 305 units, streaming
//! all of them costs ~92ms and 1.3MB, against ~0.47ms for a single filtered
//! call on a warm connection. Break-even is around 198 units, and monitord
//! wants a handful — the `[services]` list, the tracked timers, and whichever
//! units need a oneshot type check. The collectors that do want every unit
//! (`boot_blame`, `verify`) are served from the metrics stream instead.
//!
//! systemd omits fields that sit at their default rather than sending a zero,
//! so every mapping below has to restate the default the D-Bus property would
//! have returned. Those defaults are asserted in the tests.

use std::collections::HashMap;

use tracing::debug;
use tracing::warn;

use crate::units::ServiceStats;
use crate::varlink::unit::{ListOutput, Unit};

pub use crate::varlink::manager::MANAGER_SOCKET_PATH;

/// systemd reports "unset" counters as `u64::MAX` over D-Bus (`[not set]` /
/// `infinity` in `systemctl show`) and by omitting the field over varlink.
const UNSET: u64 = u64::MAX;

/// A connection to PID 1's unit API that remembers what it has already asked.
///
/// Built once per collection cycle. The cache matters because the same unit can
/// be wanted by more than one caller in a cycle — a `.service` can be both in
/// `[services]` and the target of a tracked timer — and a miss costs a round
/// trip to PID 1, which serves varlink requests one at a time.
pub struct UnitLookup {
    connection: zlink::unix::Connection,
    cache: HashMap<String, Option<ListOutput>>,
}

impl UnitLookup {
    pub async fn connect(socket_path: &str) -> anyhow::Result<Self> {
        Ok(Self {
            connection: zlink::unix::connect(socket_path).await?,
            cache: HashMap::new(),
        })
    }

    /// Look a unit up, returning `None` if systemd does not know it.
    ///
    /// A unit that is absent is cached as absent: the answer will not change
    /// within a cycle, and re-asking would cost another round trip.
    pub async fn get(&mut self, name: &str) -> Option<&ListOutput> {
        if !self.cache.contains_key(name) {
            let fetched = match self.connection.list(Some(name)).await {
                Ok(Ok(output)) => Some(output),
                Ok(Err(err)) => {
                    debug!("No unit {} over varlink: {}", name, err);
                    None
                }
                Err(err) => {
                    warn!("Unable to look up {} over varlink: {:?}", name, err);
                    None
                }
            };
            self.cache.insert(name.to_string(), fetched);
        }
        self.cache.get(name).and_then(|entry| entry.as_ref())
    }

    /// Number of units fetched from PID 1 so far, for collection timings.
    pub fn fetches(&self) -> u64 {
        self.cache.len() as u64
    }
}

/// Whether a unit is a oneshot service.
///
/// The varlink equivalent of `units::is_oneshot_service_by_name`, which the
/// varlink path previously had to reach back to D-Bus for.
pub fn is_oneshot(output: &ListOutput) -> bool {
    output
        .context
        .as_ref()
        .and_then(|context| context.service.as_ref())
        .and_then(|service| service.r#type.as_deref())
        == Some("oneshot")
}

/// Map a `Unit.List` reply onto `ServiceStats`.
///
/// `processes` comes from the unit's cgroup rather than the reply — systemd
/// exposes no per-cgroup process count over varlink (tracked in #37).
pub fn map_service_stats(output: &ListOutput, processes: u32) -> ServiceStats {
    let runtime = output.runtime.as_ref();
    let cgroup = runtime.and_then(|runtime| runtime.cgroup.as_ref());
    let service_runtime = runtime.and_then(|runtime| runtime.service.as_ref());
    let service_context = output
        .context
        .as_ref()
        .and_then(|context| context.service.as_ref());

    let realtime = |pick: fn(
        &crate::varlink::unit::UnitRuntime,
    ) -> Option<crate::varlink::unit::Timestamp>| {
        runtime
            .and_then(pick)
            .and_then(|timestamp| timestamp.realtime)
            .unwrap_or(0)
    };

    ServiceStats {
        active_enter_timestamp: realtime(|runtime| runtime.active_enter_timestamp),
        // systemd declares ActiveExitTimestamp in the IDL but does not emit it,
        // so this stays 0 where the D-Bus path reports a real value for a unit
        // that has left the active state. Tracked in #37.
        active_exit_timestamp: 0,
        cpuusage_nsec: cgroup.and_then(|cgroup| cgroup.cpu_usage_nsec).unwrap_or(0),
        inactive_exit_timestamp: realtime(|runtime| runtime.inactive_exit_timestamp),
        // Absent unless IOAccounting is on, where D-Bus reports u64::MAX.
        ioread_bytes: cgroup
            .and_then(|cgroup| cgroup.io_read_bytes)
            .unwrap_or(UNSET),
        ioread_operations: cgroup
            .and_then(|cgroup| cgroup.io_read_operations)
            .unwrap_or(UNSET),
        memory_available: cgroup
            .and_then(|cgroup| cgroup.memory_available)
            .unwrap_or(0),
        memory_current: cgroup.and_then(|cgroup| cgroup.memory_current).unwrap_or(0),
        nrestarts: service_runtime
            .and_then(|service| service.n_restarts)
            .unwrap_or(0),
        processes,
        restart_usec: service_context
            .and_then(|service| service.restart_usec)
            .unwrap_or(0),
        state_change_timestamp: realtime(|runtime| runtime.state_change_timestamp),
        status_errno: service_runtime
            .and_then(|service| service.status_errno)
            .unwrap_or(0),
        tasks_current: cgroup.and_then(|cgroup| cgroup.tasks_current).unwrap_or(0),
        // Defaults to infinity, which systemd reports as u64::MAX over D-Bus.
        timeout_clean_usec: service_context
            .and_then(|service| service.timeout_clean_usec)
            .unwrap_or(UNSET),
        watchdog_usec: service_context
            .and_then(|service| service.watchdog_usec)
            .unwrap_or(0),
    }
}

/// Count the processes in a unit's cgroup.
///
/// The D-Bus path counts what `GetProcesses` returns; varlink has no
/// equivalent, so read the same number out of the cgroup the reply points at.
/// `fs_root` prefixes the cgroup mount for container collection.
pub async fn count_cgroup_processes(fs_root: &str, output: &ListOutput) -> u32 {
    let Some(cgroup_path) = output
        .runtime
        .as_ref()
        .and_then(|runtime| runtime.cgroup.as_ref())
        .and_then(|cgroup| cgroup.path.as_deref())
    else {
        return 0;
    };
    let procs_path = format!("{}/sys/fs/cgroup{}/cgroup.procs", fs_root, cgroup_path);
    match tokio::fs::read_to_string(&procs_path).await {
        Ok(contents) => contents.lines().filter(|line| !line.is_empty()).count() as u32,
        Err(err) => {
            debug!("Unable to read {}: {:?}", procs_path, err);
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::varlink::unit::{
        CGroupRuntime, ServiceContext, ServiceRuntime, Timestamp, UnitContext, UnitRuntime,
    };

    fn output(context: Option<UnitContext>, runtime: Option<UnitRuntime>) -> ListOutput {
        ListOutput { context, runtime }
    }

    fn service_context(r#type: &str) -> UnitContext {
        UnitContext {
            service: Some(ServiceContext {
                r#type: Some(r#type.to_string()),
                restart_usec: None,
                timeout_clean_usec: None,
                watchdog_usec: None,
            }),
        }
    }

    #[test]
    fn test_is_oneshot() {
        assert!(is_oneshot(&output(Some(service_context("oneshot")), None)));
        assert!(!is_oneshot(&output(
            Some(service_context("notify-reload")),
            None
        )));
        // A unit with no service context at all is not a service, let alone a
        // oneshot one — the D-Bus path treats a failed lookup the same way.
        assert!(!is_oneshot(&output(None, None)));
    }

    #[test]
    fn test_map_service_stats_from_a_populated_reply() {
        let stats = map_service_stats(
            &output(
                Some(UnitContext {
                    service: Some(ServiceContext {
                        r#type: Some("notify-reload".to_string()),
                        restart_usec: Some(100_000),
                        timeout_clean_usec: None,
                        watchdog_usec: None,
                    }),
                }),
                Some(UnitRuntime {
                    state_change_timestamp: Some(Timestamp {
                        realtime: Some(1_789_701_442_296_989),
                        monotonic: Some(3_774_344_633),
                    }),
                    active_enter_timestamp: Some(Timestamp {
                        realtime: Some(1_789_701_442_296_989),
                        monotonic: Some(3_774_344_633),
                    }),
                    inactive_exit_timestamp: Some(Timestamp {
                        realtime: Some(1_789_701_442_287_084),
                        monotonic: Some(3_774_334_729),
                    }),
                    cgroup: Some(CGroupRuntime {
                        path: Some("/system.slice/dbus-broker.service".to_string()),
                        cpu_usage_nsec: Some(86_690_000),
                        memory_current: Some(3_457_024),
                        memory_available: Some(7_619_899_392),
                        tasks_current: Some(2),
                        io_read_bytes: None,
                        io_read_operations: None,
                    }),
                    service: Some(ServiceRuntime {
                        status_errno: Some(0),
                        n_restarts: Some(0),
                    }),
                }),
            ),
            2,
        );

        assert_eq!(stats.active_enter_timestamp, 1_789_701_442_296_989);
        assert_eq!(stats.cpuusage_nsec, 86_690_000);
        assert_eq!(stats.memory_current, 3_457_024);
        assert_eq!(stats.tasks_current, 2);
        assert_eq!(stats.processes, 2);
        assert_eq!(stats.restart_usec, 100_000);
        // Omitted by systemd, so the D-Bus defaults have to be restated here:
        // IO accounting off reads as u64::MAX, TimeoutCleanUSec defaults to
        // infinity, and WatchdogUSec to 0.
        assert_eq!(stats.ioread_bytes, u64::MAX);
        assert_eq!(stats.ioread_operations, u64::MAX);
        assert_eq!(stats.timeout_clean_usec, u64::MAX);
        assert_eq!(stats.watchdog_usec, 0);
    }

    #[test]
    fn test_map_service_stats_from_an_empty_reply() {
        // Everything absent must still land on the D-Bus defaults rather than
        // zeroing the unset sentinels.
        let stats = map_service_stats(&output(None, None), 0);
        assert_eq!(stats.ioread_bytes, u64::MAX);
        assert_eq!(stats.ioread_operations, u64::MAX);
        assert_eq!(stats.timeout_clean_usec, u64::MAX);
        assert_eq!(stats.watchdog_usec, 0);
        assert_eq!(stats.cpuusage_nsec, 0);
        assert_eq!(stats.active_enter_timestamp, 0);
    }
}
