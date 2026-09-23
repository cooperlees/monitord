//! # monitord Crate
//!
//! `monitord` is a library to gather statistics about systemd.

use std::sync::Arc;

use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;

use thiserror::Error;
use tokio::sync::RwLock;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;
use tracing::Instrument;

#[derive(Error, Debug)]
pub enum MonitordError {
    #[error("D-Bus connection error: {0}")]
    ZbusError(#[from] zbus::Error),
}

impl MonitordError {
    /// Unwrap the inner zbus error. Exhaustive today: connection setup is
    /// the only thing that produces this error, so there is exactly one
    /// variant to unwrap.
    pub fn into_zbus(self) -> zbus::Error {
        match self {
            MonitordError::ZbusError(inner) => inner,
        }
    }
}

pub mod boot;
pub mod cgroup;
pub mod config;
pub(crate) mod dbus;
pub mod dbus_stats;
pub mod json;
pub mod logging;
pub mod machines;
pub mod networkd;
pub mod pid1;
pub mod system;
pub mod timer;
pub mod unit_constants;
pub mod units;
pub mod varlink;
pub mod varlink_boot;
pub mod varlink_fallback;
pub mod varlink_networkd;
pub mod varlink_system;
pub mod varlink_unit;
pub mod varlink_units;
pub mod varlink_verify;
pub mod verify;

pub const DEFAULT_DBUS_ADDRESS: &str = "unix:path=/run/dbus/system_bus_socket";

/// Per-collector timing for a single stat collection run.
///
/// `start_offset_ms` is the wall time between the top of the collection cycle and
/// the moment this collector's future was first polled. A non-trivial offset
/// indicates the spawn/scheduling loop or the runtime is delaying first poll,
/// which means collectors are not starting in parallel as intended.
///
/// `elapsed_ms` is the wall time between first poll and completion.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct CollectorTiming {
    /// Name of the collector (e.g. "units", "pid1", "dbus_stats")
    pub name: String,
    /// Milliseconds from top of the run until the spawned future's first poll.
    /// Should be small (< a few ms) when collectors are truly running in parallel.
    pub start_offset_ms: f64,
    /// Milliseconds from first poll to future completion.
    pub elapsed_ms: f64,
    /// Whether the collector returned Ok.
    pub success: bool,
}

/// Which API transport served a collector on the last run.
///
/// Recorded per enabled collector so operators can watch varlink adoption
/// climb as the fleet's systemd upgrades past each endpoint's minimum
/// version: a collector reports `Varlink` when its varlink attempt succeeded
/// and `Dbus` when it collected the legacy way — either because varlink is
/// disabled in config or because the varlink attempt failed and fell back.
/// `Dbus` also covers the file-based networkd fallback: the value answers
/// "did this collector use varlink", not "which fallback served it".
/// Collectors with no varlink path (`pid1`, `dbus_stats`) and disabled
/// collectors stay `None` and emit no gauge, so the present gauges are
/// exactly the enabled set — no separate enabled-collectors counter needed.
#[derive(
    serde_repr::Serialize_repr,
    serde_repr::Deserialize_repr,
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
)]
#[repr(u8)]
pub enum CollectorTransport {
    #[default]
    Dbus = 0,
    Varlink = 1,
}

impl CollectorTransport {
    /// Gauge value for flat JSON and metric exporters: 1 when varlink served
    /// the collector, 0 for the D-Bus/file fallback.
    ///
    /// The `repr(u8)` discriminants are the gauge values, so every output
    /// format (json, json-pretty, json-flat) reports the same integer —
    /// matching the other stat enums (`SystemdSystemState`, the unit state
    /// enums) rather than serializing as a string in some formats.
    pub fn as_u64(self) -> u64 {
        self as u64
    }
}

/// Per-collector transport record for the last collection run.
///
/// Every field is `Some` when its collector ran and `None` when it did not
/// (disabled in config). Absent-when-disabled is what makes the adoption
/// ratio work: `count()` over the gauges is the enabled set.
///
/// NOTE for future collectors: the host `MachineStats` is constructed once
/// outside the daemon loop, not fresh each iteration — so a collector with
/// an early-return path that skips its gauge assignment would silently
/// inherit the previous run's value instead of dropping the gauge. Always
/// assign on every path, including cache hits and config-disabled fallbacks.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct VarlinkUsage {
    /// Always collected; follows `[system-state] varlink` via the shared
    /// `Manager.Describe` call. Normally agrees with `system_state` (one
    /// call serves both), but persists when `[system-state]` is disabled —
    /// making it the cheapest varlink canary on such hosts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<CollectorTransport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_state: Option<CollectorTransport>,
    /// `Varlink` means the bulk enumeration came from the metrics stream.
    /// Inside containers this still includes D-Bus calls underneath: the
    /// timer backfill (`collect_all_timers_dbus`) and the oneshot type
    /// override, which have no varlink equivalent there (see #211). The host
    /// varlink path needs neither, so a container `1` involves strictly more
    /// D-Bus traffic than a host `1` — compare host and container gauges
    /// separately rather than aggregating them into one adoption ratio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub units: Option<CollectorTransport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub networkd: Option<CollectorTransport>,
    /// Host-side machine enumeration, still D-Bus-only until machined grows
    /// a varlink List API (see #37). Per-container transports land on each
    /// machine's own `MachineStats` instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machines: Option<CollectorTransport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boot_blame: Option<CollectorTransport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify: Option<CollectorTransport>,
}

/// Stats collected for a single systemd-nspawn container or VM managed by systemd-machined
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct MachineStats {
    /// systemd-networkd interface states inside the container
    pub networkd: networkd::NetworkdState,
    /// PID 1 process stats from procfs (using the container's leader PID)
    pub pid1: Option<pid1::Pid1Stats>,
    /// Overall systemd system state (e.g. running, degraded) inside the container
    pub system_state: system::SystemdSystemState,
    /// Aggregated systemd unit counts and per-service/timer stats inside the container
    pub units: units::SystemdUnitStats,
    /// systemd version running inside the container
    pub version: system::SystemdVersion,
    /// D-Bus daemon/broker statistics inside the container
    pub dbus_stats: Option<dbus_stats::DBusStats>,
    /// Boot blame statistics: slowest units at boot with activation times in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boot_blame: Option<boot::BootBlameStats>,
    /// Unit verification error statistics
    pub verify_stats: Option<verify::VerifyStats>,
    /// Which transport served each collector on the last run.
    pub varlink_usage: VarlinkUsage,
}

/// Root struct containing all enabled monitord metrics for the host system and containers
#[derive(serde::Serialize, serde::Deserialize, Debug, Default, PartialEq)]
pub struct MonitordStats {
    /// systemd-networkd interface states and managed interface count
    pub networkd: networkd::NetworkdState,
    /// PID 1 (systemd) process stats from procfs: CPU, memory, FDs, tasks
    pub pid1: Option<pid1::Pid1Stats>,
    /// Overall systemd manager state (e.g. running, degraded, initializing)
    pub system_state: system::SystemdSystemState,
    /// Aggregated systemd unit counts by type/state and per-service/timer detailed metrics
    pub units: units::SystemdUnitStats,
    /// Installed systemd version (major.minor.revision.os)
    pub version: system::SystemdVersion,
    /// D-Bus daemon/broker statistics (connections, bus names, match rules, per-peer accounting)
    pub dbus_stats: Option<dbus_stats::DBusStats>,
    /// Per-container stats keyed by machine name, collected via systemd-machined
    pub machines: HashMap<String, MachineStats>,
    /// Boot blame statistics: slowest units at boot with activation times in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boot_blame: Option<boot::BootBlameStats>,
    /// Unit verification error statistics
    pub verify_stats: Option<verify::VerifyStats>,
    /// End-to-end duration of the last stat collection run in milliseconds.
    pub stat_collection_run_time_ms: f64,
    /// Per-collector timings from the last run, sorted slowest first. Empty
    /// before the first run completes. Callers compute parallelism ratio
    /// (sum of `elapsed_ms` / `stat_collection_run_time_ms`) and identify the
    /// gating collector (first entry) directly from this vector.
    pub collector_timings: Vec<CollectorTiming>,
    /// Which transport served each collector on the last run.
    pub varlink_usage: VarlinkUsage,
}

/// Print statistics in the format set in configuration
pub fn print_stats(
    key_prefix: &str,
    output_format: &config::MonitordOutputFormat,
    stats: &MonitordStats,
) {
    match output_format {
        config::MonitordOutputFormat::Json => println!(
            "{}",
            serde_json::to_string(&stats).expect("Invalid JSON serialization")
        ),
        config::MonitordOutputFormat::JsonFlat => println!(
            "{}",
            json::flatten(stats, key_prefix).expect("Invalid JSON serialization")
        ),
        config::MonitordOutputFormat::JsonPretty => println!(
            "{}",
            serde_json::to_string_pretty(&stats).expect("Invalid JSON serialization")
        ),
    }
}

fn set_stat_collection_run_time(stats: &mut MonitordStats, elapsed_runtime: Duration) {
    stats.stat_collection_run_time_ms = elapsed_runtime.as_secs_f64() * 1000.0;
}

/// Output produced by every spawned collector future after wrapping with timing.
type TimedCollectorOutput = (String, anyhow::Result<()>, Duration, Duration);

/// Spawn a collector future onto the join set with timing instrumentation.
///
/// The wrapping closure records the moment the future is first polled (relative
/// to `collect_start`) and the elapsed wall time until it completes. Both
/// durations and the collector name are returned alongside the original result.
///
/// `tokio::task::JoinSet::spawn` runs the future on a new task, and tracing
/// spans do not cross task boundaries automatically — without explicitly
/// capturing and re-attaching the caller's span here, every collector (and
/// anything it spawns in turn, e.g. units.rs's per-unit tasks) would show up
/// as an unrelated root trace instead of a child of the current collection run.
fn spawn_timed<F>(
    join_set: &mut tokio::task::JoinSet<TimedCollectorOutput>,
    name: &'static str,
    collect_start: Instant,
    fut: F,
) where
    F: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let parent_span = tracing::Span::current();
    let span = tracing::debug_span!(
        parent: &parent_span,
        "collector",
        name,
        elapsed_ms = tracing::field::Empty,
        success = tracing::field::Empty,
    );
    let recording_span = span.clone();
    join_set.spawn(
        async move {
            let task_first_poll = Instant::now();
            let start_offset = task_first_poll.duration_since(collect_start);
            let result = fut.await;
            let elapsed = task_first_poll.elapsed();
            recording_span.record("elapsed_ms", elapsed.as_secs_f64() * 1000.0);
            recording_span.record("success", result.is_ok());
            (name.to_string(), result, start_offset, elapsed)
        }
        .instrument(span),
    );
}

/// Lazily-created system bus connection shared by every collector that
/// needs D-Bus — also passed into collectors that decide their transport
/// internally (`verify`, `boot_blame`), so those resolve it only on their
/// D-Bus code paths rather than up front (see finding 1 on #224).
///
/// The cell starts empty (or seeded from `maybe_connection` for library
/// callers) and is only connected on first use, so a run whose enabled
/// collectors all succeed over varlink/fs/procfs never touches the bus at
/// all — the prerequisite for the zero-D-Bus claim in #37.
///
/// `tokio::sync::OnceCell` serializes initializers behind a semaphore:
/// concurrent first-users wait for the in-flight attempt and share its
/// result — there is no race and no spare connection. A failed attempt is
/// *not* cached (a waiter starts a fresh attempt instead), so a dead bus
/// costs one connect attempt per D-Bus collector, sequentially. `ENOENT`
/// fails fast and the run finishes in milliseconds, but `method_timeout`
/// only bounds method calls, not `Builder::build()` — a socket that
/// exists and hangs (wedged broker) would serialize one blocking connect
/// per D-Bus collector per cycle, where the old code blocked exactly once
/// at startup. Worth knowing when pointing at an unfamiliar bus address.
pub(crate) type DbusCell = Arc<tokio::sync::OnceCell<zbus::Connection>>;

/// Resolve the shared D-Bus connection, connecting on first use.
///
/// Only called on D-Bus code paths (varlink/fs-first collectors reach here
/// solely through their fallbacks), so a missing bus surfaces as that
/// collector's ordinary error — logged per-collector, never fatal to the
/// run. Daemon-mode reuse is preserved: the same connection serves every
/// cycle until a failed cycle drops it (see below).
pub(crate) async fn dbus_connection(
    cell: &DbusCell,
    dbus_timeout: u64,
) -> Result<zbus::Connection, MonitordError> {
    cell.get_or_try_init(|| async {
        zbus::connection::Builder::system()?
            .method_timeout(std::time::Duration::from_secs(dbus_timeout))
            .build()
            .await
    })
    .await
    .cloned()
    .map_err(MonitordError::ZbusError)
}

/// Main statistic collection function running what's required by configuration in parallel
/// Takes an optional locked stats struct to update and to output stats to STDOUT or not.
/// Takes an optional D-Bus connection. Returns `Some(connection)` if the
/// collection cycle completed without errors (meaning the connection is reusable),
/// `None` if errors occurred.
pub async fn stat_collector(
    config: config::Config,
    maybe_locked_stats: Option<Arc<RwLock<MonitordStats>>>,
    output_stats: bool,
    maybe_connection: Option<zbus::Connection>,
) -> Result<Option<zbus::Connection>, MonitordError> {
    let mut collect_interval_ms: u128 = 0;
    if config.monitord.daemon {
        collect_interval_ms = (config.monitord.daemon_stats_refresh_secs * 1000).into();
    }

    let config = Arc::new(config);
    let locked_monitord_stats: Arc<RwLock<MonitordStats>> =
        maybe_locked_stats.unwrap_or(Arc::new(RwLock::new(MonitordStats::default())));
    let locked_machine_stats: Arc<RwLock<MachineStats>> =
        Arc::new(RwLock::new(MachineStats::default()));
    let cached_machine_connections: Arc<tokio::sync::Mutex<machines::MachineConnections>> =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));

    std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", &config.monitord.dbus_address);
    // Seeded when the library caller passes a connection, otherwise
    // connected lazily by the first collector that needs the bus. Mutable
    // so a failed daemon cycle can swap in a fresh cell (see below) rather
    // than letting the next cycle reuse a bus that may have gone away.
    let mut dbus_cell: DbusCell = Arc::new(tokio::sync::OnceCell::new());
    if let Some(conn) = maybe_connection {
        // Seeding cannot fail: a fresh cell is always empty.
        let _ = dbus_cell.set(conn);
    }
    let dbus_timeout = config.monitord.dbus_timeout;
    let mut join_set: tokio::task::JoinSet<TimedCollectorOutput> = tokio::task::JoinSet::new();
    let mut had_error;

    loop {
        let collect_start_time = Instant::now();
        // Kept alive for the whole iteration so its reported duration covers
        // the full run (spawn + drain), not just the synchronous spawn phase
        // below where `run_guard` is held.
        let run_span = tracing::info_span!("stat_collector_run");
        let run_guard = run_span.enter();
        info!("Starting stat collection run");

        // One Manager.Describe serves both the version and system state
        // collectors: PID 1 handles varlink requests one at a time, so a second
        // call would sit behind the units collector's whole metrics stream.
        //
        // Which request reaches PID 1 first is still up to the scheduler, so
        // under CPU pressure this call can land behind that stream anyway. That
        // is the old cost for one call rather than two, not a new failure mode.
        // Making it deterministic would mean holding the units collector until
        // this resolves, which would put the collector that gates the cycle
        // behind an unrelated socket.
        let manager_describe = config.use_varlink(&[config.system_state.varlink]).then(|| {
            crate::varlink_system::shared_describe(
                crate::varlink_system::MANAGER_SOCKET_PATH.into(),
            )
        });

        // Always collect systemd version
        {
            let dbus_cell = Arc::clone(&dbus_cell);
            let stats_clone = locked_machine_stats.clone();
            let describe = manager_describe.clone();
            let no_fallback = config.varlink.no_fallback;
            spawn_timed(&mut join_set, "version", collect_start_time, async move {
                if let Some(describe) = describe {
                    match crate::varlink_system::update_version(describe, stats_clone.clone()).await
                    {
                        Ok(()) => {
                            stats_clone.write().await.varlink_usage.version =
                                Some(CollectorTransport::Varlink);
                            return Ok(());
                        }
                        Err(err) => {
                            crate::varlink_fallback::report_varlink_failure(
                                no_fallback,
                                "version",
                                "D-Bus",
                                err,
                            )?;
                        }
                    }
                }
                stats_clone.write().await.varlink_usage.version = Some(CollectorTransport::Dbus);
                let conn = dbus_connection(&dbus_cell, dbus_timeout).await?;
                crate::system::update_version(conn, stats_clone.clone()).await
            });
        }

        // Collect pid1 procfs stats
        if config.pid1.enabled {
            spawn_timed(
                &mut join_set,
                "pid1",
                collect_start_time,
                crate::pid1::update_pid1_stats(1, locked_machine_stats.clone()),
            );
        }

        // Run networkd collector if enabled
        if config.networkd.enabled {
            let config_clone = Arc::clone(&config);
            let dbus_cell = Arc::clone(&dbus_cell);
            let stats_clone = locked_machine_stats.clone();
            let no_fallback = config.varlink.no_fallback;
            spawn_timed(&mut join_set, "networkd", collect_start_time, async move {
                if config_clone.use_varlink(&[config_clone.networkd.varlink]) {
                    let endpoint = crate::varlink_networkd::NETWORK_SOCKET_PATH.into();
                    match crate::varlink_networkd::get_networkd_state(&endpoint).await {
                        Ok(networkd_stats) => {
                            let mut machine_stats = stats_clone.write().await;
                            machine_stats.networkd = networkd_stats;
                            machine_stats.varlink_usage.networkd =
                                Some(CollectorTransport::Varlink);
                            return Ok(());
                        }
                        Err(err) => {
                            crate::varlink_fallback::report_varlink_failure(
                                no_fallback,
                                "networkd",
                                "file-based",
                                err,
                            )?;
                        }
                    }
                }
                stats_clone.write().await.varlink_usage.networkd = Some(CollectorTransport::Dbus);
                // No eager connect: the cell flows into the collector and
                // is resolved only if sysfs yields no usable map — the
                // same lazy pattern as boot_blame.
                crate::networkd::update_networkd_stats(
                    config_clone.networkd.link_state_dir.clone(),
                    None,
                    std::path::PathBuf::from("/sys"),
                    Some((dbus_cell, dbus_timeout)),
                    stats_clone,
                )
                .await
            });
        }

        // Run system running (SystemState) state collector
        if config.system_state.enabled {
            let dbus_cell = Arc::clone(&dbus_cell);
            let stats_clone = locked_machine_stats.clone();
            let describe = manager_describe.clone();
            let no_fallback = config.varlink.no_fallback;
            spawn_timed(
                &mut join_set,
                "system_state",
                collect_start_time,
                async move {
                    if let Some(describe) = describe {
                        match crate::varlink_system::update_system_stats(
                            describe,
                            stats_clone.clone(),
                        )
                        .await
                        {
                            Ok(()) => {
                                stats_clone.write().await.varlink_usage.system_state =
                                    Some(CollectorTransport::Varlink);
                                return Ok(());
                            }
                            Err(err) => {
                                crate::varlink_fallback::report_varlink_failure(
                                    no_fallback,
                                    "system state",
                                    "D-Bus",
                                    err,
                                )?;
                            }
                        }
                    }
                    stats_clone.write().await.varlink_usage.system_state =
                        Some(CollectorTransport::Dbus);
                    let conn = dbus_connection(&dbus_cell, dbus_timeout).await?;
                    crate::system::update_system_stats(conn, stats_clone.clone()).await
                },
            );
        }

        // Run service collectors if there are services listed in config
        if config.units.enabled {
            let config_clone = Arc::clone(&config);
            let dbus_cell = Arc::clone(&dbus_cell);
            let stats_clone = locked_machine_stats.clone();
            let no_fallback = config.varlink.no_fallback;
            spawn_timed(&mut join_set, "units", collect_start_time, async move {
                if config_clone.use_varlink(&[config_clone.units.varlink]) {
                    match crate::varlink_units::update_unit_stats(
                        Arc::clone(&config_clone),
                        stats_clone.clone(),
                        crate::varlink_units::METRICS_SOCKET_PATH.into(),
                    )
                    .await
                    {
                        Ok(timer_names) => {
                            // Per-service stats, timer properties and service
                            // types all come from io.systemd.Unit.List. If that
                            // socket is unusable the whole units collection is
                            // redone over D-Bus: restoring pieces of it would
                            // leave [services] and timers partially filled from
                            // the metrics, which is worse than either path alone.
                            if let Err(err) = crate::varlink_units::apply_unit_details(
                                &crate::varlink_unit::MANAGER_SOCKET_PATH.into(),
                                &stats_clone,
                                &config_clone,
                                "",
                                &timer_names,
                            )
                            .await
                            {
                                crate::varlink_fallback::report_varlink_failure(
                                    no_fallback,
                                    "unit details",
                                    "D-Bus",
                                    err,
                                )?;
                                stats_clone.write().await.varlink_usage.units =
                                    Some(CollectorTransport::Dbus);
                                let conn = dbus_connection(&dbus_cell, dbus_timeout).await?;
                                return crate::units::update_unit_stats(
                                    config_clone,
                                    conn,
                                    stats_clone,
                                    String::new(),
                                )
                                .await;
                            }
                            if config_clone.units.unit_files {
                                let unit_files = crate::units::collect_unit_files_stats("").await;
                                let mut ms = stats_clone.write().await;
                                ms.units.unit_files = unit_files;
                            }
                            stats_clone.write().await.varlink_usage.units =
                                Some(CollectorTransport::Varlink);
                            return Ok(());
                        }
                        Err(err) => {
                            crate::varlink_fallback::report_varlink_failure(
                                no_fallback,
                                "units",
                                "D-Bus",
                                err,
                            )?;
                        }
                    }
                }
                stats_clone.write().await.varlink_usage.units = Some(CollectorTransport::Dbus);
                let conn = dbus_connection(&dbus_cell, dbus_timeout).await?;
                crate::units::update_unit_stats(config_clone, conn, stats_clone, String::new())
                    .await
            });
        }

        if config.machines.enabled {
            let stats_clone = locked_machine_stats.clone();
            let config_clone = Arc::clone(&config);
            let dbus_cell = Arc::clone(&dbus_cell);
            let monitord_stats_clone = locked_monitord_stats.clone();
            let connections_clone = cached_machine_connections.clone();
            spawn_timed(&mut join_set, "machines", collect_start_time, async move {
                // Enumeration is D-Bus-only: machined has no varlink
                // List API yet (see #37). Per-container collection
                // records its own transports on the machine stats.
                stats_clone.write().await.varlink_usage.machines = Some(CollectorTransport::Dbus);
                let conn = dbus_connection(&dbus_cell, dbus_timeout).await?;
                crate::machines::update_machines_stats(
                    config_clone,
                    conn,
                    monitord_stats_clone,
                    connections_clone,
                )
                .await
            });
        }

        if config.dbus_stats.enabled {
            let dbus_cell = Arc::clone(&dbus_cell);
            let config_clone = Arc::clone(&config);
            let stats_clone = locked_machine_stats.clone();
            spawn_timed(
                &mut join_set,
                "dbus_stats",
                collect_start_time,
                async move {
                    let conn = dbus_connection(&dbus_cell, dbus_timeout).await?;
                    crate::dbus_stats::update_dbus_stats(config_clone, conn, stats_clone).await
                },
            );
        }

        // `verify` and `boot_blame` decide their transport internally:
        // they take the shared cell and resolve it only on a D-Bus code
        // path, so a varlink success or cache hit never connects.
        if config.boot_blame.enabled {
            let dbus_cell = Arc::clone(&dbus_cell);
            let config_clone = Arc::clone(&config);
            let stats_clone = locked_machine_stats.clone();
            spawn_timed(
                &mut join_set,
                "boot_blame",
                collect_start_time,
                async move {
                    let no_fallback = config_clone.varlink.no_fallback;
                    crate::boot::update_boot_blame_stats(
                        config_clone,
                        dbus_cell,
                        dbus_timeout,
                        stats_clone,
                        no_fallback,
                    )
                    .await
                },
            );
        }

        if config.verify.enabled {
            let dbus_cell = Arc::clone(&dbus_cell);
            let config_clone = Arc::clone(&config);
            let stats_clone = locked_machine_stats.clone();
            spawn_timed(&mut join_set, "verify", collect_start_time, async move {
                crate::verify::update_verify_stats(
                    dbus_cell,
                    dbus_timeout,
                    stats_clone,
                    config_clone.verify.allowlist.clone(),
                    config_clone.verify.blocklist.clone(),
                    config_clone.use_varlink(&[config_clone.verify.varlink]),
                    config_clone.varlink.no_fallback,
                )
                .await
            });
        }

        if join_set.len() == 1 {
            warn!("No collectors except systemd version scheduled to run. Exiting");
        }

        // All collectors above were spawned with `run_span` captured as their
        // parent; the guard must be dropped before the first `.await` below
        // since span guards are not valid to hold across an await point.
        drop(run_guard);

        // Drain join_set, collect per-collector timings + log per-collector failures
        had_error = false;
        let mut timings: Vec<CollectorTiming> = Vec::new();
        while let Some(res) = join_set.join_next().await {
            match res {
                Ok((name, collector_result, start_offset, elapsed)) => {
                    let success = collector_result.is_ok();
                    if let Err(e) = collector_result {
                        had_error = true;
                        error!("Collector '{}' failure: {:?}", name, e);
                    }
                    timings.push(CollectorTiming {
                        name,
                        start_offset_ms: start_offset.as_secs_f64() * 1000.0,
                        elapsed_ms: elapsed.as_secs_f64() * 1000.0,
                        success,
                    });
                }
                Err(e) => {
                    had_error = true;
                    error!("Join error: {:?}", e);
                }
            }
        }

        let elapsed_runtime = collect_start_time.elapsed();
        let elapsed_runtime_ms = elapsed_runtime.as_millis();

        // Sort timings by elapsed desc so the slowest collector is first in the JSON output
        timings.sort_by(|a, b| {
            b.elapsed_ms
                .partial_cmp(&a.elapsed_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Per-collector lines log at debug! to keep daemon-mode noise low.
        // The same data is on MonitordStats::collector_timings for callers that need it.
        for t in &timings {
            debug!(
                "collector '{}' start_offset={:.1}ms elapsed={:.1}ms{}",
                t.name,
                t.start_offset_ms,
                t.elapsed_ms,
                if t.success { "" } else { " (FAILED)" },
            );
        }

        {
            // Update monitord stats with machine stats
            let mut monitord_stats = locked_monitord_stats.write().await;
            let machine_stats = locked_machine_stats.read().await;
            monitord_stats.pid1 = machine_stats.pid1.clone();
            monitord_stats.networkd = machine_stats.networkd.clone();
            monitord_stats.system_state = machine_stats.system_state;
            monitord_stats.version = machine_stats.version.clone();
            monitord_stats.units = machine_stats.units.clone();
            monitord_stats.dbus_stats = machine_stats.dbus_stats.clone();
            monitord_stats.boot_blame = machine_stats.boot_blame.clone();
            monitord_stats.verify_stats = machine_stats.verify_stats.clone();
            monitord_stats.varlink_usage = machine_stats.varlink_usage.clone();
            set_stat_collection_run_time(&mut monitord_stats, elapsed_runtime);
            monitord_stats.collector_timings = timings;
        }

        info!("stat collection run took {}ms", elapsed_runtime_ms);
        if output_stats {
            let monitord_stats = locked_monitord_stats.read().await;
            print_stats(
                &config.monitord.key_prefix,
                &config.monitord.output_format,
                &monitord_stats,
            );
        }
        if !config.monitord.daemon {
            break;
        }
        if had_error {
            // Drop the shared connection so the next cycle reconnects
            // rather than reusing a bus that may have gone away mid-run.
            // Each iteration clones the cell into its tasks at spawn time,
            // so reassigning the local is enough for the next cycle's
            // spawns to pick up the fresh cell.
            dbus_cell = Arc::new(tokio::sync::OnceCell::new());
        }
        let sleep_time_ms = collect_interval_ms - elapsed_runtime_ms;
        info!("stat collection sleeping for {}s 😴", sleep_time_ms / 1000);
        tokio::time::sleep(Duration::from_millis(
            sleep_time_ms
                .try_into()
                .expect("Sleep time does not fit into a u64 :O"),
        ))
        .await;
    }
    // A failed cycle returns no connection so a repeated one-shot caller
    // reconnects instead of reusing a broken bus; a clean cycle hands the
    // shared connection back for reuse. (In daemon mode the reset above
    // already swapped in a fresh cell, so `get()` here is empty by design
    // and `None` is the only honest answer.)
    let conn = if had_error {
        None
    } else {
        dbus_cell.get().cloned()
    };
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lazy-connect contract finding 6 on #224 pins down: a failed
    /// connect is not cached, so consecutive failures each attempt (and
    /// report) a fresh error rather than inheriting a poisoned cell.
    #[tokio::test]
    async fn test_dbus_connection_failure_is_not_cached() {
        std::env::set_var(
            "DBUS_SYSTEM_BUS_ADDRESS",
            "unix:path=/nonexistent/monitord-test-bus-socket",
        );
        let cell: DbusCell = Arc::new(tokio::sync::OnceCell::new());
        assert!(dbus_connection(&cell, 1).await.is_err());
        assert!(
            cell.get().is_none(),
            "failed connect must not populate the cell"
        );
        assert!(dbus_connection(&cell, 1).await.is_err());
        assert!(cell.get().is_none());
    }

    #[test]
    fn test_stat_collection_run_time_ms_conversion() {
        let mut stats = MonitordStats::default();
        set_stat_collection_run_time(&mut stats, Duration::from_millis(5));
        assert_eq!(stats.stat_collection_run_time_ms, 5.0);

        set_stat_collection_run_time(&mut stats, Duration::from_micros(500));
        assert!((stats.stat_collection_run_time_ms - 0.5).abs() < f64::EPSILON);
    }
}
