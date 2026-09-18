//! Varlink proxy for the io.systemd.Unit interface on PID 1's socket.
//! Adapted from the interface definition in systemd's
//! `src/core/varlink-unit.c`. The `List` method exists from v258, but the
//! per-type context sections monitord reads land later: `src/core/varlink-service.c`
//! and `src/core/varlink-timer.c` first appear in **v261**, so that is the real
//! minimum for anything here.
//!
//! Only the fields monitord maps onto its own stats types are declared; serde
//! drops the rest, which is most of a ~7KB per-unit reply.
//!
//! Note that systemd omits fields sitting at their default rather than sending
//! a zero, so almost everything here is optional and the caller supplies the
//! default (see `varlink_units`, where those defaults have to match what the
//! D-Bus properties return for the same unit).

use serde::{Deserialize, Serialize};
use zlink::{proxy, ReplyError};

/// Proxy trait for calling methods on the io.systemd.Unit interface.
#[proxy("io.systemd.Unit")]
pub trait Unit {
    /// Look up a single unit by name.
    ///
    /// Called without the `more` flag and with a name, this returns one unit
    /// rather than streaming every unit — which is what monitord wants: a
    /// filtered call costs ~0.5ms against ~92ms to stream all 305 units on a
    /// test host, and monitord only needs a handful per collection.
    async fn list(&mut self, name: Option<&str>) -> zlink::Result<Result<ListOutput, UnitError>>;
}

/// Output parameters for the List method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListOutput {
    /// Unit configuration.
    pub context: Option<UnitContext>,
    /// Unit runtime state.
    pub runtime: Option<UnitRuntime>,
}

/// Configuration of a unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitContext {
    #[serde(rename = "Service")]
    pub service: Option<ServiceContext>,
    #[serde(rename = "Exec")]
    pub exec: Option<ExecContext>,
    #[serde(rename = "Timer")]
    pub timer: Option<TimerContext>,
}

/// Execution configuration shared by every unit type that runs processes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecContext {
    /// Timeout for cleaning up resources after the unit exits. Lives here
    /// rather than under `Service`, unlike the D-Bus property of the same name.
    #[serde(rename = "TimeoutCleanUSec")]
    pub timeout_clean_usec: Option<u64>,
}

/// Timer-specific configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerContext {
    /// Unit this timer triggers, e.g. "logrotate.service".
    #[serde(rename = "Unit")]
    pub unit: Option<String>,
    #[serde(rename = "AccuracyUSec")]
    pub accuracy_usec: Option<u64>,
    #[serde(rename = "RandomizedDelayUSec")]
    pub randomized_delay_usec: Option<u64>,
    #[serde(rename = "FixedRandomDelay")]
    pub fixed_random_delay: Option<bool>,
    #[serde(rename = "Persistent")]
    pub persistent: Option<bool>,
    #[serde(rename = "RemainAfterElapse")]
    pub remain_after_elapse: Option<bool>,
}

/// Service-specific configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceContext {
    /// Service type: simple, oneshot, notify, notify-reload, …
    #[serde(rename = "Type")]
    pub r#type: Option<String>,
    /// Configured restart delay in microseconds.
    #[serde(rename = "RestartUSec")]
    pub restart_usec: Option<u64>,
    /// Configured watchdog timeout in microseconds.
    ///
    /// This is the *configured* value; the D-Bus property of the same name is
    /// the currently armed one, which reads `infinity` while a service is not
    /// running. They agree for running services, which is the case monitord
    /// collects, and systemd exposes no runtime equivalent over varlink.
    #[serde(rename = "WatchdogUSec")]
    pub watchdog_usec: Option<u64>,
}

/// Runtime state of a unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitRuntime {
    #[serde(rename = "StateChangeTimestamp")]
    pub state_change_timestamp: Option<Timestamp>,
    #[serde(rename = "ActiveEnterTimestamp")]
    pub active_enter_timestamp: Option<Timestamp>,
    #[serde(rename = "InactiveExitTimestamp")]
    pub inactive_exit_timestamp: Option<Timestamp>,
    /// Emitted once the unit has actually left the active state; absent (not
    /// zero) before that.
    #[serde(rename = "ActiveExitTimestamp")]
    pub active_exit_timestamp: Option<Timestamp>,
    #[serde(rename = "CGroup")]
    pub cgroup: Option<CGroupRuntime>,
    #[serde(rename = "Service")]
    pub service: Option<ServiceRuntime>,
    #[serde(rename = "Timer")]
    pub timer: Option<TimerRuntime>,
}

/// Timer-specific runtime state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerRuntime {
    /// Next elapse on CLOCK_REALTIME; 0 for a purely monotonic timer.
    #[serde(rename = "NextElapseUSecRealtime")]
    pub next_elapse_usec_realtime: Option<u64>,
    #[serde(rename = "NextElapseUSecMonotonic")]
    pub next_elapse_usec_monotonic: Option<u64>,
    /// When the timer last fired; absent if it never has.
    #[serde(rename = "LastTriggerUSec")]
    pub last_trigger_usec: Option<Timestamp>,
}

/// A systemd timestamp, carrying both clocks.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Timestamp {
    pub realtime: Option<u64>,
    pub monotonic: Option<u64>,
}

/// cgroup accounting for a unit. Fields appear only when the matching
/// accounting option is enabled for the unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CGroupRuntime {
    /// cgroup path, relative to the cgroup mount point.
    #[serde(rename = "Path")]
    pub path: Option<String>,
    #[serde(rename = "CPUUsageNSec")]
    pub cpu_usage_nsec: Option<u64>,
    #[serde(rename = "MemoryCurrent")]
    pub memory_current: Option<u64>,
    #[serde(rename = "MemoryAvailable")]
    pub memory_available: Option<u64>,
    #[serde(rename = "TasksCurrent")]
    pub tasks_current: Option<u64>,
    #[serde(rename = "IOReadBytes")]
    pub io_read_bytes: Option<u64>,
    #[serde(rename = "IOReadOperations")]
    pub io_read_operations: Option<u64>,
}

/// Service-specific runtime state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceRuntime {
    /// Main process of the service, if it has one.
    #[serde(rename = "MainPID")]
    pub main_pid: Option<ProcessId>,
    /// errno-style status reported by the service via sd_notify.
    #[serde(rename = "StatusErrno")]
    pub status_errno: Option<i32>,
    /// Number of times systemd has restarted this service.
    #[serde(rename = "NRestarts")]
    pub n_restarts: Option<u32>,
}

/// A process reference, carrying the pid plus fields that disambiguate reuse.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessId {
    pub pid: Option<u32>,
}

/// Errors that can occur in the io.systemd.Unit interface.
#[derive(Debug, Clone, PartialEq, ReplyError)]
#[zlink(interface = "io.systemd.Unit")]
pub enum UnitError {
    /// No unit by that name is loaded.
    NoSuchUnit,
}

impl std::fmt::Display for UnitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnitError::NoSuchUnit => write!(f, "No such unit"),
        }
    }
}

impl std::error::Error for UnitError {}
