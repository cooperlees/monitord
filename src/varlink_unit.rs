//! # varlink_unit module
//!
//! Per-unit detail via the `io.systemd.Unit` varlink API on PID 1's socket.
//! `Unit.List` itself exists from systemd v258, but the per-type context
//! sections read here (`varlink-service.c`, `varlink-timer.c`) first appear in
//! **v261**, which is the real minimum. This is what the varlink path uses
//! instead of the per-unit D-Bus property fetches in `units.rs`.
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
use std::collections::HashSet;

use tracing::debug;

use crate::timer::TimerStats;
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

    /// Look a unit up, returning `Ok(None)` if systemd does not know it.
    ///
    /// A transport or protocol failure is an error rather than `None`, so the
    /// caller can fall back to D-Bus for the whole phase. Collapsing the two
    /// would leave service stats silently empty on a broken socket while
    /// reporting success.
    ///
    /// A unit that is genuinely absent is cached as absent: the answer will not
    /// change within a cycle, and re-asking would cost another round trip.
    pub async fn get(&mut self, name: &str) -> anyhow::Result<Option<&ListOutput>> {
        if !self.cache.contains_key(name) {
            let fetched = match self.connection.list(Some(name)).await? {
                Ok(output) => Some(output),
                Err(err) => {
                    debug!("No unit {} over varlink: {}", name, err);
                    None
                }
            };
            self.cache.insert(name.to_string(), fetched);
        }
        Ok(self.cache.get(name).and_then(|entry| entry.as_ref()))
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
        active_exit_timestamp: realtime(|runtime| runtime.active_exit_timestamp),
        // Every cgroup counter defaults to UNSET, not 0: systemd omits these
        // when the matching accounting option is off, and the D-Bus properties
        // then read `[not set]` (u64::MAX). Answering 0 would report a service
        // as using no memory rather than as unmeasured.
        cpuusage_nsec: cgroup
            .and_then(|cgroup| cgroup.cpu_usage_nsec)
            .unwrap_or(UNSET),
        inactive_exit_timestamp: realtime(|runtime| runtime.inactive_exit_timestamp),
        ioread_bytes: cgroup
            .and_then(|cgroup| cgroup.io_read_bytes)
            .unwrap_or(UNSET),
        ioread_operations: cgroup
            .and_then(|cgroup| cgroup.io_read_operations)
            .unwrap_or(UNSET),
        memory_available: cgroup
            .and_then(|cgroup| cgroup.memory_available)
            .unwrap_or(UNSET),
        memory_current: cgroup
            .and_then(|cgroup| cgroup.memory_current)
            .unwrap_or(UNSET),
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
        tasks_current: cgroup
            .and_then(|cgroup| cgroup.tasks_current)
            .unwrap_or(UNSET),
        // Under context.Exec, not context.Service as the D-Bus property name
        // suggests. Defaults to infinity, which D-Bus reports as u64::MAX.
        timeout_clean_usec: output
            .context
            .as_ref()
            .and_then(|context| context.exec.as_ref())
            .and_then(|exec| exec.timeout_clean_usec)
            .unwrap_or(UNSET),
        watchdog_usec: service_context
            .and_then(|service| service.watchdog_usec)
            .unwrap_or(0),
    }
}

/// The unit a timer triggers, e.g. "logrotate.service" for "logrotate.timer".
pub fn timer_triggered_unit(output: &ListOutput) -> Option<&str> {
    output
        .context
        .as_ref()
        .and_then(|context| context.timer.as_ref())
        .and_then(|timer| timer.unit.as_deref())
}

/// Map a `Unit.List` reply onto `TimerStats`.
///
/// `triggered` is the reply for the unit this timer starts, looked up
/// separately because its state change timestamps are properties of that unit
/// rather than of the timer. `None` when it could not be resolved, which
/// leaves those two fields at 0 — the same as the D-Bus path, which reports 0
/// when the timer names no unit.
pub fn map_timer_stats(output: &ListOutput, triggered: Option<&ListOutput>) -> TimerStats {
    let context = output
        .context
        .as_ref()
        .and_then(|context| context.timer.as_ref());
    let runtime = output
        .runtime
        .as_ref()
        .and_then(|runtime| runtime.timer.as_ref());
    let last_trigger = runtime.and_then(|runtime| runtime.last_trigger_usec);
    let service_state_change = triggered
        .and_then(|triggered| triggered.runtime.as_ref())
        .and_then(|runtime| runtime.state_change_timestamp);

    TimerStats {
        accuracy_usec: context.and_then(|timer| timer.accuracy_usec).unwrap_or(0),
        fixed_random_delay: context
            .and_then(|timer| timer.fixed_random_delay)
            .unwrap_or(false),
        last_trigger_usec: last_trigger
            .and_then(|timestamp| timestamp.realtime)
            .unwrap_or(0),
        last_trigger_usec_monotonic: last_trigger
            .and_then(|timestamp| timestamp.monotonic)
            .unwrap_or(0),
        next_elapse_usec_monotonic: runtime
            .and_then(|timer| timer.next_elapse_usec_monotonic)
            .unwrap_or(0),
        next_elapse_usec_realtime: runtime
            .and_then(|timer| timer.next_elapse_usec_realtime)
            .unwrap_or(0),
        persistent: context.and_then(|timer| timer.persistent).unwrap_or(false),
        randomized_delay_usec: context
            .and_then(|timer| timer.randomized_delay_usec)
            .unwrap_or(0),
        remain_after_elapse: context
            .and_then(|timer| timer.remain_after_elapse)
            .unwrap_or(false),
        service_unit_last_state_change_usec: service_state_change
            .and_then(|timestamp| timestamp.realtime)
            .unwrap_or(0),
        service_unit_last_state_change_usec_monotonic: service_state_change
            .and_then(|timestamp| timestamp.monotonic)
            .unwrap_or(0),
    }
}

/// Count the processes in a unit's cgroup, including nested ones.
///
/// The D-Bus path counts what `GetProcesses` returns, and systemd walks the
/// whole subtree there — a service that delegates its cgroup and puts workers
/// in children would be undercounted by reading only its own `cgroup.procs`.
/// The main PID is folded in the same way systemd does, since it can sit
/// outside the cgroup. `fs_root` prefixes the cgroup mount for containers.
pub async fn count_cgroup_processes(fs_root: &str, output: &ListOutput) -> u32 {
    let runtime = output.runtime.as_ref();
    let Some(cgroup_path) = runtime
        .and_then(|runtime| runtime.cgroup.as_ref())
        .and_then(|cgroup| cgroup.path.as_deref())
    else {
        return 0;
    };

    let mut pids: HashSet<u32> = HashSet::new();
    let mut directories = vec![format!("{}/sys/fs/cgroup{}", fs_root, cgroup_path)];
    while let Some(directory) = directories.pop() {
        match tokio::fs::read_to_string(format!("{directory}/cgroup.procs")).await {
            Ok(contents) => {
                pids.extend(contents.lines().filter_map(|line| line.parse::<u32>().ok()))
            }
            Err(err) => debug!("Unable to read {}/cgroup.procs: {:?}", directory, err),
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

    if let Some(main_pid) = runtime
        .and_then(|runtime| runtime.service.as_ref())
        .and_then(|service| service.main_pid.as_ref())
        .and_then(|process| process.pid)
    {
        pids.insert(main_pid);
    }

    pids.len() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::varlink::unit::{
        CGroupRuntime, ExecContext, ServiceContext, ServiceRuntime, TimerContext, TimerRuntime,
        Timestamp, UnitContext, UnitRuntime,
    };

    fn output(context: Option<UnitContext>, runtime: Option<UnitRuntime>) -> ListOutput {
        ListOutput { context, runtime }
    }

    fn service_context(r#type: &str) -> UnitContext {
        UnitContext {
            service: Some(ServiceContext {
                r#type: Some(r#type.to_string()),
                restart_usec: None,
                watchdog_usec: None,
            }),
            exec: None,
            timer: None,
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
                        watchdog_usec: None,
                    }),
                    // TimeoutCleanUSec lives here, not under Service.
                    exec: Some(ExecContext {
                        timeout_clean_usec: Some(30_000_000),
                    }),
                    timer: None,
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
                    active_exit_timestamp: Some(Timestamp {
                        realtime: Some(1_789_701_442_280_000),
                        monotonic: Some(3_774_327_645),
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
                        main_pid: None,
                        status_errno: Some(0),
                        n_restarts: Some(0),
                    }),
                    timer: None,
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
        // Real, and emitted once a unit has actually left the active state.
        assert_eq!(stats.active_exit_timestamp, 1_789_701_442_280_000);
        // Read from context.Exec rather than context.Service.
        assert_eq!(stats.timeout_clean_usec, 30_000_000);
        // Omitted by systemd, so the D-Bus defaults have to be restated here:
        // IO accounting off reads as u64::MAX, WatchdogUSec as 0.
        assert_eq!(stats.ioread_bytes, u64::MAX);
        assert_eq!(stats.ioread_operations, u64::MAX);
        assert_eq!(stats.watchdog_usec, 0);
    }

    fn timer_output() -> ListOutput {
        // Shaped from a real systemd-tmpfiles-clean.timer reply.
        ListOutput {
            context: Some(UnitContext {
                service: None,
                exec: None,
                timer: Some(TimerContext {
                    unit: Some("systemd-tmpfiles-clean.service".to_string()),
                    accuracy_usec: Some(60_000_000),
                    randomized_delay_usec: None,
                    fixed_random_delay: Some(false),
                    persistent: Some(false),
                    remain_after_elapse: Some(true),
                }),
            }),
            runtime: Some(UnitRuntime {
                state_change_timestamp: None,
                active_enter_timestamp: None,
                inactive_exit_timestamp: None,
                active_exit_timestamp: None,
                cgroup: None,
                service: None,
                timer: Some(TimerRuntime {
                    next_elapse_usec_realtime: Some(0),
                    next_elapse_usec_monotonic: Some(91_091_400_515),
                    last_trigger_usec: Some(Timestamp {
                        realtime: Some(1_789_702_359_355_506),
                        monotonic: Some(4_691_399_604),
                    }),
                }),
            }),
        }
    }

    #[test]
    fn test_map_timer_stats() {
        let triggered = output(
            None,
            Some(UnitRuntime {
                state_change_timestamp: Some(Timestamp {
                    realtime: Some(1_789_702_359_400_000),
                    monotonic: Some(4_691_444_098),
                }),
                active_enter_timestamp: None,
                inactive_exit_timestamp: None,
                active_exit_timestamp: None,
                cgroup: None,
                service: None,
                timer: None,
            }),
        );
        let stats = map_timer_stats(&timer_output(), Some(&triggered));

        assert_eq!(stats.accuracy_usec, 60_000_000);
        assert_eq!(stats.last_trigger_usec, 1_789_702_359_355_506);
        assert_eq!(stats.last_trigger_usec_monotonic, 4_691_399_604);
        assert_eq!(stats.next_elapse_usec_monotonic, 91_091_400_515);
        // A monotonic-only timer reports 0 here, as the D-Bus property does.
        assert_eq!(stats.next_elapse_usec_realtime, 0);
        assert!(stats.remain_after_elapse);
        assert!(!stats.persistent);
        // Omitted by systemd when unset, and 0 over D-Bus.
        assert_eq!(stats.randomized_delay_usec, 0);
        // Comes from the triggered unit, not the timer.
        assert_eq!(
            stats.service_unit_last_state_change_usec,
            1_789_702_359_400_000
        );
        assert_eq!(
            stats.service_unit_last_state_change_usec_monotonic,
            4_691_444_098
        );
    }

    #[test]
    fn test_map_timer_stats_without_the_triggered_unit() {
        // The D-Bus path reports 0 for both when the timer names no unit, so an
        // unresolvable trigger target must not invent a timestamp.
        let stats = map_timer_stats(&timer_output(), None);
        assert_eq!(stats.service_unit_last_state_change_usec, 0);
        assert_eq!(stats.service_unit_last_state_change_usec_monotonic, 0);
        assert_eq!(stats.accuracy_usec, 60_000_000);
    }

    #[test]
    fn test_timer_triggered_unit() {
        assert_eq!(
            timer_triggered_unit(&timer_output()),
            Some("systemd-tmpfiles-clean.service")
        );
        assert_eq!(timer_triggered_unit(&output(None, None)), None);
    }

    #[test]
    fn test_map_service_stats_from_an_empty_reply() {
        // Everything absent must land on the D-Bus defaults rather than zeroing
        // the unset sentinels. A service with accounting off reports [not set]
        // over D-Bus, so reporting 0 bytes of memory would be a fabrication.
        let stats = map_service_stats(&output(None, None), 0);
        assert_eq!(stats.ioread_bytes, u64::MAX);
        assert_eq!(stats.ioread_operations, u64::MAX);
        assert_eq!(stats.timeout_clean_usec, u64::MAX);
        assert_eq!(stats.cpuusage_nsec, u64::MAX);
        assert_eq!(stats.memory_current, u64::MAX);
        assert_eq!(stats.memory_available, u64::MAX);
        assert_eq!(stats.tasks_current, u64::MAX);
        // These two genuinely default to zero over D-Bus.
        assert_eq!(stats.watchdog_usec, 0);
        assert_eq!(stats.active_enter_timestamp, 0);
    }
}
