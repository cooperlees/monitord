//! # report module
//!
//! Serve monitord's own health as an `io.systemd.Metrics` source so
//! `systemd-report` (systemd ≥ 260) includes monitord in its report.
//!
//! `systemd-report` calls `io.systemd.Metrics.List` / `Describe` on every
//! socket in `/run/systemd/report/`. A source's socket must be named as a
//! varlink interface name, and every metric it returns must be prefixed with
//! that name followed by a dot. Metrics without the prefix are dropped as
//! invalid. So the socket is always [`REPORT_SOCKET_NAME`] and every family
//! in [`FAMILIES`] starts with it.
//!
//! These metrics cover monitord itself: whether its last collection run
//! was clean, how long each collector took, and failure counters. They do
//! not re-export the stats monitord collects, which systemd's own report
//! sources already publish.

use std::collections::BTreeMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::stream;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};
use zlink::introspect;

use crate::{CollectorTiming, MonitordStats};

/// Socket filename, which is also the prefix of every metric name.
pub const REPORT_SOCKET_NAME: &str = "io.monitord";
/// Directory `systemd-report` scans for system-scope sources.
pub const REPORT_DIR: &str = "/run/systemd/report";

/// First fd systemd passes in socket activation (`SD_LISTEN_FDS_START`).
const SD_LISTEN_FDS_START: i32 = 3;

/// Metric family type, as in systemd's `io.systemd.Metrics` IDL.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, introspect::Type)]
#[serde(rename_all = "snake_case")]
#[zlink(rename_all = "snake_case")]
pub enum MetricFamilyType {
    Counter,
    Gauge,
    String,
    /// Arbitrary JSON object. monitord exports none; listed so our
    /// introspected IDL matches systemd's.
    Object,
}

/// A metric value: any JSON value, `any` in systemd's IDL (a systemd
/// extension zlink can describe but not derive for `serde_json::Value`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct MetricValue(pub serde_json::Value);

impl introspect::Type for MetricValue {
    const TYPE: &'static zlink::idl::Type<'static> = &zlink::idl::Type::Any;
}

/// One metric family: (name suffix after `io.monitord.`, description, type).
pub const FAMILIES: &[(&str, &str, MetricFamilyType)] = &[
    (
        "collectionErrorsTotal",
        "Collection runs where at least one collector failed",
        MetricFamilyType::Counter,
    ),
    (
        "collectionRunTimeMs",
        "Wall time of the last collection run in milliseconds",
        MetricFamilyType::Gauge,
    ),
    (
        "collectionRunsTotal",
        "Collection runs completed since monitord started",
        MetricFamilyType::Counter,
    ),
    (
        "collectorElapsedMs",
        "Wall time of each collector in the last run in milliseconds",
        MetricFamilyType::Gauge,
    ),
    (
        "collectorFailuresTotal",
        "Failed runs of each collector since monitord started",
        MetricFamilyType::Counter,
    ),
    (
        "collectorSuccess",
        "1 if the collector succeeded in the last run, else 0",
        MetricFamilyType::Gauge,
    ),
    (
        "healthy",
        "1 if the last collection run had no collector failures, else 0",
        MetricFamilyType::Gauge,
    ),
    (
        "lastCollectionTimestamp",
        "Unix time in seconds when the last collection run finished",
        MetricFamilyType::Gauge,
    ),
    (
        "varlinkUsage",
        "1 if varlink served the collector in the last run, 0 for D-Bus",
        MetricFamilyType::Gauge,
    ),
    ("version", "monitord version", MetricFamilyType::String),
];

/// Counters kept across collection runs. Not part of `MonitordStats` so the
/// JSON output is unchanged; only the report server reads them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReportState {
    pub collection_runs_total: u64,
    pub collection_errors_total: u64,
    /// Failure count per collector name. Every collector seen gets an
    /// entry, so a counter is present at 0 before its first failure.
    pub collector_failures_total: BTreeMap<String, u64>,
    /// `None` until the first run finishes.
    pub last_collection_timestamp: Option<u64>,
    pub last_run_healthy: bool,
}

impl ReportState {
    /// Record a finished collection run.
    pub fn record_run(&mut self, timings: &[CollectorTiming], had_error: bool) {
        self.collection_runs_total += 1;
        if had_error {
            self.collection_errors_total += 1;
        }
        for timing in timings {
            let failures = self
                .collector_failures_total
                .entry(timing.name.clone())
                .or_default();
            if !timing.success {
                *failures += 1;
            }
        }
        self.last_run_healthy = !had_error;
        self.last_collection_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs());
    }
}

/// `io.systemd.Metrics.List` reply.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, introspect::Type)]
pub struct MetricReply {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    pub value: MetricValue,
}

/// `io.systemd.Metrics.Describe` reply.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, introspect::Type)]
pub struct DescribeReply {
    pub name: String,
    pub description: String,
    pub r#type: MetricFamilyType,
}

/// Errors this source sends back, named as systemd names them.
#[derive(Debug, Clone, PartialEq, zlink::ReplyError)]
#[zlink(interface = "org.varlink.service")]
pub enum ReportError {
    /// `List`/`Describe` called without the `more` flag.
    ExpectedMore,
}

/// `ExpectedMore` is a standard `org.varlink.service` error, so it is kept
/// out of the `io.systemd.Metrics` IDL we introspect as.
impl introspect::ReplyError for ReportError {
    const VARIANTS: &'static [&'static zlink::idl::Error<'static>] = &[];
}

fn full_name(suffix: &str) -> String {
    format!("{REPORT_SOCKET_NAME}.{suffix}")
}

fn metric(suffix: &str, object: Option<&str>, value: impl Into<serde_json::Value>) -> MetricReply {
    MetricReply {
        name: full_name(suffix),
        object: object.map(str::to_string),
        value: MetricValue(value.into()),
    }
}

/// Build every `List` reply from the latest stats and counters.
///
/// Before the first run there are no timings and no timestamp, so only
/// the version, the run counters and `healthy` = 0 are returned.
pub fn build_metrics(stats: &MonitordStats, state: &ReportState) -> Vec<MetricReply> {
    let mut metrics = vec![
        metric("version", None, env!("CARGO_PKG_VERSION")),
        metric("healthy", None, u64::from(state.last_run_healthy)),
        metric("collectionRunsTotal", None, state.collection_runs_total),
        metric("collectionErrorsTotal", None, state.collection_errors_total),
    ];
    if let Some(ts) = state.last_collection_timestamp {
        metrics.push(metric("lastCollectionTimestamp", None, ts));
        metrics.push(metric(
            "collectionRunTimeMs",
            None,
            stats.stat_collection_run_time_ms,
        ));
    }
    for timing in &stats.collector_timings {
        let object = Some(timing.name.as_str());
        metrics.push(metric("collectorElapsedMs", object, timing.elapsed_ms));
        metrics.push(metric(
            "collectorSuccess",
            object,
            u64::from(timing.success),
        ));
    }
    for (name, failures) in &state.collector_failures_total {
        metrics.push(metric("collectorFailuresTotal", Some(name), *failures));
    }
    let usage = serde_json::to_value(&stats.varlink_usage).unwrap_or_default();
    if let serde_json::Value::Object(usage) = usage {
        for (collector, transport) in usage {
            if transport.is_null() {
                continue;
            }
            metrics.push(metric("varlinkUsage", Some(&collector), transport));
        }
    }
    metrics
}

/// Mark every reply but the last as continued, as a streamed reply needs.
fn into_stream<T>(
    items: Vec<T>,
) -> impl futures_util::Stream<Item = Result<zlink::Reply<T>, ReportError>> + Unpin {
    let last = items.len().saturating_sub(1);
    stream::iter(
        items
            .into_iter()
            .enumerate()
            .map(move |(i, item)| Ok(zlink::Reply::new(Some(item)).set_continues(Some(i < last)))),
    )
}

fn expected_more<T>(
) -> impl futures_util::Stream<Item = Result<zlink::Reply<T>, ReportError>> + Unpin {
    stream::iter(vec![Err(ReportError::ExpectedMore)])
}

/// The `io.systemd.Metrics` service.
pub struct MetricsService {
    stats: Arc<RwLock<MonitordStats>>,
    state: Arc<Mutex<ReportState>>,
}

impl MetricsService {
    pub fn new(stats: Arc<RwLock<MonitordStats>>, state: Arc<Mutex<ReportState>>) -> Self {
        Self { stats, state }
    }

    /// Every `List` reply, built from one consistent run.
    ///
    /// Same lock order as the writer in `stat_collector`, which records a
    /// run in `ReportState` while it holds the stats write lock: take the
    /// stats read lock first, then copy the state under it. Copying the state
    /// first would let a run finish in between, and the reply would mix the
    /// old run's `healthy`/counters with the new run's timings.
    async fn snapshot(&self) -> Vec<MetricReply> {
        let stats = self.stats.read().await;
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        build_metrics(&stats, &state)
    }
}

#[zlink::service(
    interface = "io.systemd.Metrics",
    vendor = "monitord",
    product = "monitord",
    version = env!("CARGO_PKG_VERSION"),
    url = "https://github.com/cooperlees/monitord"
)]
impl MetricsService {
    #[zlink(more)]
    async fn list(
        &self,
        more: bool,
    ) -> impl futures_util::Stream<Item = Result<zlink::Reply<MetricReply>, ReportError>> + Unpin
    {
        if !more {
            return futures_util::future::Either::Left(expected_more());
        }
        let metrics = self.snapshot().await;
        futures_util::future::Either::Right(into_stream(metrics))
    }

    #[zlink(more)]
    async fn describe(
        &self,
        more: bool,
    ) -> impl futures_util::Stream<Item = Result<zlink::Reply<DescribeReply>, ReportError>> + Unpin
    {
        if !more {
            return futures_util::future::Either::Left(expected_more());
        }
        let families = FAMILIES
            .iter()
            .map(|(suffix, description, kind)| DescribeReply {
                name: full_name(suffix),
                description: description.to_string(),
                r#type: *kind,
            })
            .collect();
        futures_util::future::Either::Right(into_stream(families))
    }
}

/// Take the listening socket systemd passed, if it passed one to us.
///
/// Follows `sd_listen_fds(3)`: only when `LISTEN_PID` is our pid, using the
/// first fd. The env vars are then cleared, and the fd is set close-on-exec
/// so the machine helpers monitord re-execs don't inherit it.
///
/// Call it before starting anything that may spawn a process, as
/// [`spawn_server`] does: close-on-exec only protects later spawns, and a
/// helper started earlier keeps its copy of the listener.
pub fn take_activation_fd() -> Option<OwnedFd> {
    let pid = std::env::var("LISTEN_PID").ok()?.parse::<u32>().ok()?;
    if pid != std::process::id() {
        return None;
    }
    let fds = std::env::var("LISTEN_FDS").ok()?.parse::<i32>().ok()?;
    std::env::remove_var("LISTEN_PID");
    std::env::remove_var("LISTEN_FDS");
    std::env::remove_var("LISTEN_FDNAMES");
    if fds < 1 {
        return None;
    }
    if fds > 1 {
        warn!("Socket activation passed {fds} fds; using only the first for {REPORT_SOCKET_NAME}");
    }
    // SAFETY: systemd hands this fd to us and nothing else in monitord
    // uses it; taking ownership once is how sd_listen_fds consumers work.
    let fd = unsafe { OwnedFd::from_raw_fd(SD_LISTEN_FDS_START) };
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
        warn!(
            "Unable to mark the socket-activated fd close-on-exec: {}",
            std::io::Error::last_os_error()
        );
    }
    Some(fd)
}

/// Bind `path`, replacing a socket file only if nobody is serving on it.
///
/// Binding first means a live listener's file is never unlinked: only on
/// `AddrInUse` is the existing file probed. A refused connection means it
/// is stale (its owner died without removing it) and it is replaced; a
/// connection that succeeds means another monitord, or
/// `monitord-report.socket`, is serving there and this one backs off.
/// Anything that isn't a socket is left alone.
fn bind_unless_served(path: &Path) -> anyhow::Result<zlink::tokio::unix::Listener> {
    match zlink::tokio::unix::bind(path) {
        Ok(listener) => return Ok(listener),
        Err(zlink::Error::Io(e)) if e.kind() == std::io::ErrorKind::AddrInUse => {}
        Err(e) => return Err(e.into()),
    }
    let file_type = std::fs::symlink_metadata(path)?.file_type();
    if !file_type.is_socket() {
        anyhow::bail!(
            "{} exists and is not a socket; not replacing it",
            path.display()
        );
    }
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => anyhow::bail!(
            "{} is already being served (another monitord or monitord-report.socket)",
            path.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            debug!("Replacing stale {}", path.display());
            std::fs::remove_file(path)?;
            Ok(zlink::tokio::unix::bind(path)?)
        }
        Err(e) => Err(anyhow::anyhow!(
            "Unable to tell whether {} is in use: {e}",
            path.display()
        )),
    }
}

/// Listener for the report server: `activation_fd` (from
/// [`take_activation_fd`]) if systemd passed one, otherwise a socket
/// monitord binds in `dir` itself.
///
/// Must run inside the runtime that will serve it, as the listener
/// registers with that runtime's reactor. `dir` is never created: a
/// missing `/run/systemd/report` means systemd predates the report
/// framework, and the directory belongs to systemd.
pub fn listener(
    dir: &Path,
    activation_fd: Option<OwnedFd>,
) -> anyhow::Result<zlink::tokio::unix::Listener> {
    if let Some(fd) = activation_fd {
        info!("Serving {REPORT_SOCKET_NAME} metrics on the socket-activated fd");
        return Ok(zlink::tokio::unix::Listener::try_from(fd)?);
    }
    if !dir.is_dir() {
        anyhow::bail!(
            "{} does not exist; systemd-report needs systemd 260 or newer",
            dir.display()
        );
    }
    let path = dir.join(REPORT_SOCKET_NAME);
    let listener = bind_unless_served(&path)?;
    // Read-only health data, so let unprivileged systemd-report read it,
    // as systemd's own report sockets do (SocketMode=0666).
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))?;
    info!("Serving {REPORT_SOCKET_NAME} metrics on {}", path.display());
    Ok(listener)
}

/// Serve `io.systemd.Metrics` on `listener` until an error ends it.
///
/// The future is not `Send` (a zlink limitation, rust-lang/rust#100013), so
/// it can't go to `tokio::spawn` on a multi-threaded runtime. Run it with
/// `tokio::select!`, on a `LocalSet`, or use [`spawn_server`].
pub async fn serve(
    listener: zlink::tokio::unix::Listener,
    stats: Arc<RwLock<MonitordStats>>,
    state: Arc<Mutex<ReportState>>,
) -> anyhow::Result<()> {
    zlink::Server::new(listener, MetricsService::new(stats, state))
        .run()
        .await?;
    Ok(())
}

/// Run the report server on its own thread with a current-thread runtime.
///
/// Public so library users with their own collection loop (e.g.
/// monitord-exporter) can serve their stats and state too. A failure is
/// logged and ends only the server thread, never collection.
///
/// The socket-activated fd is claimed and marked close-on-exec here, on
/// the calling thread before this returns, so no process spawned after the
/// call can inherit it. Call it before starting collection.
pub fn spawn_server(
    dir: PathBuf,
    stats: Arc<RwLock<MonitordStats>>,
    state: Arc<Mutex<ReportState>>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    let activation_fd = take_activation_fd();
    std::thread::Builder::new()
        .name("monitord-report".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(e) => {
                    error!("Unable to start the systemd-report runtime: {e:?}");
                    return;
                }
            };
            runtime.block_on(async move {
                // The listener registers with the reactor, so it has to be
                // made inside this runtime rather than the caller's.
                let listener = match listener(&dir, activation_fd) {
                    Ok(listener) => listener,
                    Err(e) => {
                        warn!("Not serving systemd-report metrics: {e:?}");
                        return;
                    }
                };
                if let Err(e) = serve(listener, stats, state).await {
                    error!("systemd-report server stopped: {e:?}");
                }
            });
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::varlink::metrics::{Metrics, MetricsError};
    use futures_util::StreamExt;

    /// Port of systemd's `varlink_idl_interface_name_is_valid`: dot
    /// separated labels of ASCII letters, digits and `-`, each starting
    /// with a letter, at least two labels.
    fn interface_name_is_valid(name: &str) -> bool {
        let labels: Vec<&str> = name.split('.').collect();
        labels.len() >= 2
            && labels.iter().all(|label| {
                label
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic())
                    && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                    && !label.ends_with('-')
            })
    }

    /// Port of systemd's `varlink_idl_field_name_is_valid`.
    fn field_name_is_valid(name: &str) -> bool {
        name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !name.ends_with('_')
    }

    /// What `systemd-report` checks before it accepts a metric from the
    /// `io.monitord` source (`metrics_name_valid` + the source prefix).
    fn systemd_report_accepts(name: &str) -> bool {
        let Some((interface, field)) = name.rsplit_once('.') else {
            return false;
        };
        interface_name_is_valid(interface)
            && field_name_is_valid(field)
            && name
                .strip_prefix(REPORT_SOCKET_NAME)
                .is_some_and(|rest| rest.starts_with('.'))
    }

    fn timing(name: &str, success: bool) -> CollectorTiming {
        CollectorTiming {
            name: name.to_string(),
            start_offset_ms: 0.0,
            elapsed_ms: 1.5,
            success,
        }
    }

    #[test]
    fn test_socket_name_is_a_valid_source() {
        assert!(interface_name_is_valid(REPORT_SOCKET_NAME));
    }

    #[test]
    fn test_every_family_name_passes_systemd_report_checks() {
        for (suffix, _, _) in FAMILIES {
            assert!(systemd_report_accepts(&full_name(suffix)), "{suffix}");
        }
        assert!(!systemd_report_accepts("io.monitordfoo.bar"));
        assert!(!systemd_report_accepts("io.systemd.Manager.foo"));
    }

    #[test]
    fn test_families_are_sorted_and_unique() {
        let names: Vec<&str> = FAMILIES.iter().map(|(n, _, _)| *n).collect();
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(names, sorted);
    }

    #[test]
    fn test_record_run_counts() {
        let mut state = ReportState::default();
        state.record_run(&[timing("units", true), timing("pid1", true)], false);
        assert_eq!(state.collection_runs_total, 1);
        assert_eq!(state.collection_errors_total, 0);
        assert!(state.last_run_healthy);
        assert_eq!(state.collector_failures_total["units"], 0);
        assert!(state.last_collection_timestamp.is_some());

        state.record_run(&[timing("units", false), timing("pid1", true)], true);
        assert_eq!(state.collection_runs_total, 2);
        assert_eq!(state.collection_errors_total, 1);
        assert!(!state.last_run_healthy);
        assert_eq!(state.collector_failures_total["units"], 1);
        assert_eq!(state.collector_failures_total["pid1"], 0);

        // A clean run after a failure clears healthy but keeps the counter.
        state.record_run(&[timing("units", true)], false);
        assert!(state.last_run_healthy);
        assert_eq!(state.collector_failures_total["units"], 1);
    }

    #[test]
    fn test_build_metrics_before_first_run() {
        let metrics = build_metrics(&MonitordStats::default(), &ReportState::default());
        let names: Vec<&str> = metrics.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"io.monitord.version"));
        assert!(names.contains(&"io.monitord.healthy"));
        assert!(!names.contains(&"io.monitord.lastCollectionTimestamp"));
        let healthy = metrics
            .iter()
            .find(|m| m.name == "io.monitord.healthy")
            .unwrap();
        assert_eq!(healthy.value.0, serde_json::json!(0));
    }

    #[test]
    fn test_build_metrics_after_run() {
        let stats = MonitordStats {
            stat_collection_run_time_ms: 12.5,
            collector_timings: vec![timing("units", false)],
            ..Default::default()
        };
        let mut state = ReportState::default();
        state.record_run(&stats.collector_timings, true);

        let metrics = build_metrics(&stats, &state);
        for m in &metrics {
            assert!(systemd_report_accepts(&m.name), "{}", m.name);
        }
        let find = |name: &str, object: Option<&str>| {
            metrics
                .iter()
                .find(|m| m.name == name && m.object.as_deref() == object)
                .unwrap_or_else(|| panic!("missing {name} {object:?}"))
                .value
                .0
                .clone()
        };
        assert_eq!(find("io.monitord.healthy", None), serde_json::json!(0));
        assert_eq!(
            find("io.monitord.collectionRunTimeMs", None),
            serde_json::json!(12.5)
        );
        assert_eq!(
            find("io.monitord.collectorSuccess", Some("units")),
            serde_json::json!(0)
        );
        assert_eq!(
            find("io.monitord.collectorFailuresTotal", Some("units")),
            serde_json::json!(1)
        );
        // Disabled collectors have no transport and so no gauge.
        assert!(!metrics.iter().any(|m| m.name == "io.monitord.varlinkUsage"));
    }

    #[tokio::test]
    async fn test_snapshot_never_mixes_two_runs() {
        // A clean run is on record when a List arrives mid-collection...
        let stats = Arc::new(RwLock::new(MonitordStats {
            collector_timings: vec![timing("units", true)],
            ..Default::default()
        }));
        let state = Arc::new(Mutex::new(ReportState::default()));
        state
            .lock()
            .unwrap()
            .record_run(&[timing("units", true)], false);
        let service = MetricsService::new(stats.clone(), state.clone());

        // ...while the writer holds the stats lock to publish a failed run.
        let mut writer = stats.write().await;
        let mut reply = std::pin::pin!(service.snapshot());
        assert!(futures_util::poll!(reply.as_mut()).is_pending());
        writer.collector_timings = vec![timing("units", false)];
        state
            .lock()
            .unwrap()
            .record_run(&writer.collector_timings, true);
        drop(writer);

        // The reply must describe the failed run throughout, not the old
        // run's healthy=1 next to the new run's failed collector.
        let metrics = reply.await;
        let value = |name: &str| {
            metrics
                .iter()
                .find(|m| m.name == name)
                .unwrap_or_else(|| panic!("missing {name}"))
                .value
                .0
                .clone()
        };
        assert_eq!(value("io.monitord.healthy"), serde_json::json!(0));
        assert_eq!(value("io.monitord.collectorSuccess"), serde_json::json!(0));
        assert_eq!(
            value("io.monitord.collectionErrorsTotal"),
            serde_json::json!(1)
        );
    }

    #[tokio::test]
    async fn test_self_bind_refuses_a_served_socket() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _serving = listener(dir.path(), None).expect("first listener");
        let err = listener(dir.path(), None).expect_err("second listener must back off");
        assert!(err.to_string().contains("already being served"), "{err}");
        // The first listener's socket file is still there and still answers.
        std::os::unix::net::UnixStream::connect(dir.path().join(REPORT_SOCKET_NAME))
            .expect("original listener still reachable");
    }

    #[tokio::test]
    async fn test_self_bind_reclaims_a_stale_socket() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(REPORT_SOCKET_NAME);
        // A dead owner: the socket file stays behind after its listener closes.
        drop(std::os::unix::net::UnixListener::bind(&path).expect("bind"));
        assert!(path.exists());
        let _listener = listener(dir.path(), None).expect("stale socket replaced");
        std::os::unix::net::UnixStream::connect(&path).expect("new listener reachable");
    }

    #[tokio::test]
    async fn test_self_bind_leaves_non_sockets_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(REPORT_SOCKET_NAME);
        std::fs::write(&path, "not a socket").expect("write");
        let err = listener(dir.path(), None).expect_err("must not replace a file");
        assert!(err.to_string().contains("not a socket"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not a socket");
    }

    #[tokio::test]
    async fn test_server_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let listener = listener(dir.path(), None).expect("listener");
        let socket = dir.path().join(REPORT_SOCKET_NAME);
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o666);

        let timings = vec![timing("pid1", true)];
        let mut report_state = ReportState::default();
        report_state.record_run(&timings, false);
        let stats = Arc::new(RwLock::new(MonitordStats {
            collector_timings: timings,
            ..Default::default()
        }));
        let state = Arc::new(Mutex::new(report_state));
        tokio::select! {
            res = serve(listener, stats, state) => panic!("server exited: {res:?}"),
            () = round_trip_client(&socket) => {}
        }
    }

    async fn round_trip_client(socket: &Path) {
        let mut conn = zlink::tokio::unix::connect(socket).await.expect("connect");
        let listed: Vec<_> = conn
            .list()
            .await
            .expect("list")
            .map(|r| r.expect("reply").expect("no error"))
            .collect()
            .await;
        assert!(listed.iter().any(
            |m| m.name == "io.monitord.collectorSuccess" && m.object.as_deref() == Some("pid1")
        ));
        assert!(listed
            .iter()
            .any(|m| m.name == "io.monitord.healthy" && m.value == serde_json::json!(1)));

        let described: Vec<Result<_, MetricsError>> = conn
            .describe()
            .await
            .expect("describe")
            .map(|r| r.expect("reply"))
            .collect()
            .await;
        assert_eq!(described.len(), FAMILIES.len());
        assert!(described.iter().all(Result::is_ok));
    }
}
