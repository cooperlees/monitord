//! # report_collector module
//!
//! Collects every `io.systemd.Metrics` source that systemd-report (systemd
//! v260+) would read from `/run/systemd/report/`, and passes their metrics
//! through as generic key/value stats.
//!
//! Each socket in that directory is named after a varlink interface (e.g.
//! `io.systemd.Basic`), and every metric it serves must be named
//! `<socket name>.<field>`. Sources monitord already parses into typed stats
//! (`io.systemd.Manager`, `io.systemd.Network`) and monitord's own source
//! (`io.monitord`) are always skipped.

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::TryStreamExt;
use tokio::sync::RwLock;
use tracing::debug;
use tracing::warn;

use crate::varlink::metrics::{ListOutput, Metrics, MetricsError};
use crate::MonitordStats;

/// Where systemd-report looks for system metrics sources.
pub const REPORT_DIR: &str = "/run/systemd/report";

/// Sources that are never collected here:
/// - `io.systemd.Manager` and `io.systemd.Network` already feed typed
///   collectors (`units`, `networkd`, ...).
/// - `io.monitord` is monitord itself.
/// - `io.systemd.CGroup` publishes per-cgroup metrics, hundreds of keys even
///   on a small host. It is left out until a dedicated change adds
///   per-cgroup filtering.
pub const SKIP_SOURCES: &[&str] = &[
    "io.systemd.Manager",
    "io.systemd.Network",
    "io.monitord",
    "io.systemd.CGroup",
];

/// Source name prefixes that are never collected: `io.systemd.Report.*`
/// are systemd-report's own signers (`SignPlain`, `SignTPM2`, `SignTSM`),
/// which only publish their public key, not health.
pub const SKIP_SOURCE_PREFIXES: &[&str] = &["io.systemd.Report."];

/// Same per-source cap as systemd-report (`METRICS_PER_SOURCE_MAX`).
pub const METRICS_PER_SOURCE_MAX: usize = 16384;

/// Same source cap as systemd-report (`METRICS_LINKS_MAX`).
pub const SOURCES_MAX: usize = 128;

/// Metrics from one report source.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct ReportSource {
    /// Whether the source answered `io.systemd.Metrics.List` this run
    pub up: bool,
    /// Metrics the source returned but that were dropped: names not prefixed
    /// with the source name, invalid names, or past the per-source cap
    pub dropped_metrics: u64,
    /// Metrics left out because one of their flat keys was already taken by
    /// an earlier metric, or repeated inside their own value. Keys are kept
    /// readable (unit names keep their dots), so two different metrics can
    /// map to the same key. The first one wins and later ones are counted
    /// here, never silently overwritten.
    pub colliding_metrics: u64,
    /// Metric values keyed by `<field>[.<object>][.<field>_<value>...]`,
    /// where the first `<field>` is the metric name without the source prefix
    pub metrics: BTreeMap<String, serde_json::Value>,
}

/// Report sources keyed by socket name (e.g. `io.systemd.Basic`)
pub type ReportStats = BTreeMap<String, ReportSource>;

/// Varlink interface name check, as `varlink_idl_interface_name_is_valid()`:
/// dot-separated segments, at least two, the first starting with a letter,
/// each of alphanumerics with inner (never leading or trailing) dashes.
pub fn interface_name_is_valid(name: &str) -> bool {
    let segments: Vec<&str> = name.split('.').collect();
    if segments.len() < 2 {
        return false;
    }
    segments.iter().enumerate().all(|(idx, segment)| {
        let bytes = segment.as_bytes();
        let (Some(first), Some(last)) = (bytes.first(), bytes.last()) else {
            return false;
        };
        let first_ok = if idx == 0 {
            first.is_ascii_alphabetic()
        } else {
            first.is_ascii_alphanumeric()
        };
        first_ok
            && last.is_ascii_alphanumeric()
            && bytes
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
    })
}

/// Varlink field name check, as `varlink_idl_field_name_is_valid()`: starts
/// with a letter, alphanumerics with single inner underscores.
pub fn field_name_is_valid(name: &str) -> bool {
    let bytes = name.as_bytes();
    let (Some(first), Some(last)) = (bytes.first(), bytes.last()) else {
        return false;
    };
    first.is_ascii_alphabetic()
        && *last != b'_'
        && !name.contains("__")
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
}

/// Whether systemd-report would accept `metric_name` from `source`: an
/// interface name, a dot, then a field name, with the source as a true prefix.
pub fn metric_name_is_valid(metric_name: &str, source: &str) -> bool {
    let Some((interface, field)) = metric_name.rsplit_once('.') else {
        return false;
    };
    interface_name_is_valid(interface)
        && field_name_is_valid(field)
        && metric_name
            .strip_prefix(source)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// Whether a socket name in the report directory should be collected.
///
/// `allowlist`/`blocklist` entries are compared case-insensitively and must
/// already be lowercase, as `configparser` hands them over.
pub fn should_collect_source(
    name: &str,
    allowlist: &HashSet<String>,
    blocklist: &HashSet<String>,
) -> bool {
    if !interface_name_is_valid(name)
        || SKIP_SOURCES.contains(&name)
        || SKIP_SOURCE_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
    {
        return false;
    }
    // The INI parser lowercases keys, so lists hold lowercased names.
    let lowered = name.to_lowercase();
    if blocklist.contains(&lowered) {
        return false;
    }
    allowlist.is_empty() || allowlist.contains(&lowered)
}

/// Whether `metric_name` is in `blocklist`, exactly or below an entry at a
/// dot boundary, so a family entry covers all of its sub-names.
///
/// Entries must already be lowercase, as `configparser` hands them over.
pub fn metric_is_blocked(metric_name: &str, blocklist: &HashSet<String>) -> bool {
    if blocklist.is_empty() {
        return false;
    }
    let lowered = metric_name.to_lowercase();
    let mut prefix = lowered.as_str();
    loop {
        if blocklist.contains(prefix) {
            return true;
        }
        match prefix.rsplit_once('.') {
            Some((parent, _)) => prefix = parent,
            None => return false,
        }
    }
}

/// Flat key for one metric: the name without the source prefix, then the
/// object, then every field as `<field>_<value>` in sorted field order.
fn metric_key(metric: &ListOutput, source: &str) -> String {
    let mut key = metric
        .name()
        .strip_prefix(source)
        .and_then(|rest| rest.strip_prefix('.'))
        .unwrap_or(metric.name())
        .to_string();
    if let Some(object) = metric.object() {
        key.push('.');
        key.push_str(object);
    }
    if let Some(fields) = metric.fields() {
        let sorted: BTreeMap<&String, &serde_json::Value> = fields.iter().collect();
        for (field, value) in sorted {
            let value = match value {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            key.push('.');
            key.push_str(field);
            key.push('_');
            key.push_str(&value);
        }
    }
    key
}

/// Bools become 0/1 at any depth, so every scalar is a number or a string,
/// like the rest of monitord's output. Objects and arrays keep their shape
/// for `json`/`json-pretty`; [`flat_leaves`] flattens them for `json-flat`.
fn normalize_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Bool(b) => u64::from(*b).into(),
        serde_json::Value::Object(members) => members
            .iter()
            .map(|(member, value)| (member.clone(), normalize_value(value)))
            .collect::<serde_json::Map<_, _>>()
            .into(),
        serde_json::Value::Array(items) => items.iter().map(normalize_value).collect(),
        other => other.clone(),
    }
}

/// The `json-flat` leaves of `value` stored at `key`: objects descend into
/// `key.<member>`, arrays into `key.<index>`, and nulls produce nothing.
///
/// `json.rs` uses this too, so the collision check in [`normalize_metrics`]
/// sees exactly the keys the flat output will hold.
pub fn flat_leaves(
    key: String,
    value: &serde_json::Value,
    out: &mut Vec<(String, serde_json::Value)>,
) {
    match value {
        serde_json::Value::Object(members) => {
            for (member, member_value) in members {
                flat_leaves(format!("{key}.{member}"), member_value, out);
            }
        }
        serde_json::Value::Array(items) => {
            for (idx, item) in items.iter().enumerate() {
                flat_leaves(format!("{key}.{idx}"), item, out);
            }
        }
        serde_json::Value::Null => {}
        scalar => out.push((key, scalar.clone())),
    }
}

/// Turn one source's `List` replies into a [`ReportSource`], dropping what
/// systemd-report would drop and skipping what `metric_blocklist` names.
///
/// Blocked metrics are left out on purpose, so they are not counted in
/// `dropped_metrics`, which flags metrics a source got wrong.
pub fn normalize_metrics(
    source: &str,
    metrics: &[ListOutput],
    metric_blocklist: &HashSet<String>,
) -> ReportSource {
    let mut report_source = ReportSource {
        up: true,
        ..Default::default()
    };
    // Every flat key taken so far, including the members of object values,
    // so a later metric can't land on (and overwrite) an earlier one.
    let mut taken: HashSet<String> = HashSet::new();
    for (idx, metric) in metrics.iter().enumerate() {
        if idx >= METRICS_PER_SOURCE_MAX || !metric_name_is_valid(metric.name(), source) {
            debug!("Dropping metric {:?} from report source {}", metric, source);
            report_source.dropped_metrics += 1;
            continue;
        }
        if metric.value().is_null() || metric_is_blocked(metric.name(), metric_blocklist) {
            continue;
        }
        let key = metric_key(metric, source);
        let value = normalize_value(metric.value());
        let mut leaves = Vec::new();
        flat_leaves(key.clone(), &value, &mut leaves);
        let mut own: HashSet<&str> = HashSet::with_capacity(leaves.len());
        let collides = report_source.metrics.contains_key(&key)
            || leaves
                .iter()
                .any(|(leaf, _)| taken.contains(leaf) || !own.insert(leaf.as_str()));
        if collides {
            warn!(
                "Report source {} metric {} maps to flat key {:?}, already in use; skipping it",
                source,
                metric.name(),
                key
            );
            report_source.colliding_metrics += 1;
            continue;
        }
        taken.extend(leaves.into_iter().map(|(leaf, _)| leaf));
        report_source.metrics.insert(key, value);
    }
    report_source
}

/// List the sources to collect, sorted, in `dir`.
pub async fn list_sources(
    dir: &Path,
    allowlist: &HashSet<String>,
    blocklist: &HashSet<String>,
) -> anyhow::Result<Vec<String>> {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            debug!(
                "{} does not exist (systemd < v260?): no report sources",
                dir.display()
            );
            return Ok(Vec::new());
        }
        Err(err) => return Err(err.into()),
    };
    let mut sources = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let file_type = entry.file_type().await?;
        if !file_type.is_socket() && !file_type.is_symlink() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if should_collect_source(&name, allowlist, blocklist) {
            sources.push(name);
        }
    }
    sources.sort();
    if sources.len() > SOURCES_MAX {
        warn!(
            "Reached limit of {} report sources, skipping {}",
            SOURCES_MAX,
            sources.len() - SOURCES_MAX
        );
        sources.truncate(SOURCES_MAX);
    }
    Ok(sources)
}

/// Stream `io.systemd.Metrics.List` from one source socket.
///
/// Runs on a blocking thread with its own runtime for the same reason as
/// `varlink_units::collect_metrics`: the zlink stream is !Send. A source that
/// ends its stream with `NoSuchMetric` simply has nothing (more) to report.
async fn list_source(path: PathBuf, timeout: Duration) -> anyhow::Result<Vec<ListOutput>> {
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(async move {
            tokio::time::timeout(timeout, async move {
                let mut conn = zlink::tokio::unix::connect(&path).await?;
                let stream = conn.list().await?;
                futures_util::pin_mut!(stream);
                let mut metrics = Vec::new();
                while let Some(reply) = stream.try_next().await? {
                    match reply {
                        Ok(metric) => metrics.push(metric),
                        Err(MetricsError::NoSuchMetric) => break,
                    }
                    // One past the cap is enough for normalize_metrics to
                    // count drops without buffering an unbounded stream.
                    if metrics.len() > METRICS_PER_SOURCE_MAX {
                        break;
                    }
                }
                Ok::<_, anyhow::Error>(metrics)
            })
            .await
            .map_err(|_| anyhow::anyhow!("timed out after {:?}", timeout))?
        })
    })
    .await?
}

/// Collect every report source in `dir`, concurrently. A failing source is
/// reported with `up: false` and never fails the collector.
pub async fn collect_report_stats(
    dir: &Path,
    allowlist: &HashSet<String>,
    blocklist: &HashSet<String>,
    metric_blocklist: &HashSet<String>,
    timeout: Duration,
) -> anyhow::Result<ReportStats> {
    let sources = list_sources(dir, allowlist, blocklist).await?;
    let results = futures_util::future::join_all(
        sources
            .iter()
            .map(|source| list_source(dir.join(source), timeout)),
    )
    .await;

    let mut stats = ReportStats::new();
    for (source, result) in sources.into_iter().zip(results) {
        let report_source = match result {
            Ok(metrics) => normalize_metrics(&source, &metrics, metric_blocklist),
            Err(err) => {
                warn!("Unable to collect report source {}: {:#}", source, err);
                ReportSource::default()
            }
        };
        stats.insert(source, report_source);
    }
    Ok(stats)
}

/// Collector entry point: replace `MonitordStats::report` with this run's sources.
pub async fn update_report_stats(
    config: Arc<crate::config::Config>,
    locked_monitord_stats: Arc<RwLock<MonitordStats>>,
) -> anyhow::Result<()> {
    let stats = collect_report_stats(
        Path::new(REPORT_DIR),
        &config.report.allowlist,
        &config.report.blocklist,
        &config.report.metric_blocklist,
        Duration::from_secs(config.monitord.dbus_timeout),
    )
    .await?;
    locked_monitord_stats.write().await.report = Some(stats);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn metric(
        name: &str,
        object: Option<&str>,
        fields: Option<&[(&str, serde_json::Value)]>,
        value: serde_json::Value,
    ) -> ListOutput {
        ListOutput {
            name: name.to_string(),
            value,
            object: object.map(str::to_string),
            fields: fields.map(|f| {
                f.iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect::<HashMap<_, _>>()
            }),
        }
    }

    #[test]
    fn test_interface_name_is_valid() {
        for valid in ["io.systemd.Basic", "io.monitord", "a.b", "org.foo-bar.X1"] {
            assert!(interface_name_is_valid(valid), "{valid}");
        }
        for invalid in [
            "",
            "io",
            "io.",
            ".io",
            "1io.systemd",
            "io.-systemd",
            "io.systemd-",
            "io.sys_temd",
            "io..systemd",
        ] {
            assert!(!interface_name_is_valid(invalid), "{invalid}");
        }
    }

    #[test]
    fn test_field_name_is_valid() {
        for valid in ["UnitsTotal", "unitActiveState", "a_b", "x1"] {
            assert!(field_name_is_valid(valid), "{valid}");
        }
        for invalid in ["", "_a", "a_", "a__b", "1a", "a-b", "a.b"] {
            assert!(!field_name_is_valid(invalid), "{invalid}");
        }
    }

    #[test]
    fn test_metric_name_is_valid() {
        assert!(metric_name_is_valid(
            "io.systemd.Basic.BootId",
            "io.systemd.Basic"
        ));
        assert!(metric_name_is_valid(
            "io.systemd.Basic.Sub.Value",
            "io.systemd.Basic"
        ));
        // Not prefixed by the source it came from
        assert!(!metric_name_is_valid(
            "io.systemd.Other.BootId",
            "io.systemd.Basic"
        ));
        // A prefix only counts at a dot boundary
        assert!(!metric_name_is_valid(
            "io.systemd.BasicX.BootId",
            "io.systemd.Basic"
        ));
        // The source name alone is not a metric
        assert!(!metric_name_is_valid(
            "io.systemd.Basic",
            "io.systemd.Basic"
        ));
        assert!(!metric_name_is_valid(
            "io.systemd.Basic.bad-field",
            "io.systemd.Basic"
        ));
    }

    #[test]
    fn test_should_collect_source() {
        let empty = HashSet::new();
        assert!(should_collect_source("io.systemd.Basic", &empty, &empty));
        for skipped in SKIP_SOURCES {
            assert!(!should_collect_source(skipped, &empty, &empty), "{skipped}");
        }
        assert!(!should_collect_source("not-an-interface", &empty, &empty));
        // systemd-report's signers only publish their public key
        assert!(!should_collect_source(
            "io.systemd.Report.SignPlain",
            &empty,
            &empty
        ));
        assert!(!should_collect_source(
            "io.systemd.Report.SignTPM2",
            &empty,
            &empty
        ));
        // The prefix is a true prefix: a source merely named alike is kept
        assert!(should_collect_source("io.systemd.Reporter", &empty, &empty));

        let allow = HashSet::from([String::from("io.systemd.basic")]);
        assert!(should_collect_source("io.systemd.Basic", &allow, &empty));
        assert!(!should_collect_source(
            "io.systemd.DiskSpace",
            &allow,
            &empty
        ));
        // Allowlisting a skipped source does not bring it back
        let allow_manager = HashSet::from([String::from("io.systemd.manager")]);
        assert!(!should_collect_source(
            "io.systemd.Manager",
            &allow_manager,
            &empty
        ));

        // Blocklist wins over allowlist
        assert!(!should_collect_source("io.systemd.Basic", &allow, &allow));
    }

    #[test]
    fn test_normalize_metrics() {
        let source = "io.systemd.Basic";
        let metrics = vec![
            metric(
                "io.systemd.Basic.Hostname",
                None,
                None,
                serde_json::json!("box"),
            ),
            metric(
                "io.systemd.Basic.UnitActive",
                Some("foo.service"),
                None,
                serde_json::json!(true),
            ),
            metric(
                "io.systemd.Basic.Bytes",
                Some("sda"),
                Some(&[
                    ("op", serde_json::json!("write")),
                    ("cpu", serde_json::json!(0)),
                ]),
                serde_json::json!(4096),
            ),
            metric(
                "io.systemd.Basic.Info",
                None,
                None,
                serde_json::json!({"a": 1}),
            ),
            metric(
                "io.systemd.Basic.Nothing",
                None,
                None,
                serde_json::Value::Null,
            ),
            // Wrong prefix and invalid field: both dropped
            metric("io.systemd.Other.Value", None, None, serde_json::json!(1)),
            metric(
                "io.systemd.Basic.bad-name",
                None,
                None,
                serde_json::json!(1),
            ),
        ];
        let normalized = normalize_metrics(source, &metrics, &HashSet::new());
        assert!(normalized.up);
        assert_eq!(normalized.dropped_metrics, 2);
        assert_eq!(
            normalized.metrics,
            BTreeMap::from([
                ("Hostname".to_string(), serde_json::json!("box")),
                ("UnitActive.foo.service".to_string(), serde_json::json!(1)),
                // Fields sort by name: cpu before op
                (
                    "Bytes.sda.cpu_0.op_write".to_string(),
                    serde_json::json!(4096)
                ),
                ("Info".to_string(), serde_json::json!({"a": 1})),
            ])
        );
    }

    #[test]
    fn test_metric_is_blocked() {
        let blocklist = HashSet::from([
            "io.systemd.basic.machineid".to_string(),
            "io.systemd.basic.smbios".to_string(),
        ]);
        assert!(metric_is_blocked("io.systemd.Basic.MachineID", &blocklist));
        // A family entry covers every name below it at a dot boundary
        assert!(metric_is_blocked(
            "io.systemd.Basic.SMBIOS.SystemProductName",
            &blocklist
        ));
        // ...but not names that merely share its text
        assert!(!metric_is_blocked("io.systemd.Basic.SMBIOSish", &blocklist));
        assert!(!metric_is_blocked(
            "io.systemd.Basic.MachineIDs",
            &blocklist
        ));
        assert!(!metric_is_blocked(
            "io.systemd.Basic.CPUsOnline",
            &blocklist
        ));
        assert!(!metric_is_blocked(
            "io.systemd.Basic.MachineID",
            &HashSet::new()
        ));
    }

    #[test]
    fn test_normalize_metrics_skips_blocked_without_counting_drops() {
        let metrics = vec![
            metric(
                "io.systemd.Basic.MachineID",
                None,
                None,
                serde_json::json!("7000"),
            ),
            metric(
                "io.systemd.Basic.SMBIOS.SystemVendor",
                None,
                None,
                serde_json::json!("acme"),
            ),
            metric(
                "io.systemd.Basic.CPUsOnline",
                None,
                None,
                serde_json::json!(4),
            ),
        ];
        let blocklist = HashSet::from([
            "io.systemd.basic.machineid".to_string(),
            "io.systemd.basic.smbios".to_string(),
        ]);
        let normalized = normalize_metrics("io.systemd.Basic", &metrics, &blocklist);
        assert_eq!(normalized.dropped_metrics, 0);
        assert_eq!(
            normalized.metrics,
            BTreeMap::from([("CPUsOnline".to_string(), serde_json::json!(4))])
        );
    }

    #[test]
    fn test_normalize_metrics_counts_dimension_collisions() {
        // Different field sets that render to the same key: Value.x_a.y_b
        let metrics = vec![
            metric(
                "io.systemd.Basic.Value",
                None,
                Some(&[("x", serde_json::json!("a")), ("y", serde_json::json!("b"))]),
                serde_json::json!(1),
            ),
            metric(
                "io.systemd.Basic.Value",
                None,
                Some(&[("x", serde_json::json!("a.y_b"))]),
                serde_json::json!(2),
            ),
            // An object name that renders like a field suffix
            metric(
                "io.systemd.Basic.Value",
                Some("x_a.y_b"),
                None,
                serde_json::json!(3),
            ),
        ];
        let normalized = normalize_metrics("io.systemd.Basic", &metrics, &HashSet::new());
        assert_eq!(normalized.colliding_metrics, 2);
        assert_eq!(normalized.dropped_metrics, 0);
        // The first metric keeps its value instead of being overwritten
        assert_eq!(
            normalized.metrics,
            BTreeMap::from([("Value.x_a.y_b".to_string(), serde_json::json!(1))])
        );
    }

    #[test]
    fn test_normalize_metrics_counts_flattened_path_collisions() {
        let metrics = vec![
            // Collides with itself: {"a": {"b": 1}} and "a.b" both flatten
            // to Info.a.b
            metric(
                "io.systemd.Basic.Info",
                None,
                None,
                serde_json::json!({"a": {"b": 1}, "a.b": 2}),
            ),
            // A plain metric whose key is one of a later object's leaves
            metric(
                "io.systemd.Basic.Other",
                Some("a"),
                None,
                serde_json::json!(3),
            ),
            metric(
                "io.systemd.Basic.Other",
                None,
                None,
                serde_json::json!({"a": 4, "b": 5}),
            ),
        ];
        let normalized = normalize_metrics("io.systemd.Basic", &metrics, &HashSet::new());
        assert_eq!(normalized.colliding_metrics, 2);
        assert_eq!(
            normalized.metrics,
            BTreeMap::from([("Other.a".to_string(), serde_json::json!(3))])
        );
    }

    #[test]
    fn test_normalize_metrics_converts_nested_booleans() {
        let metrics = vec![metric(
            "io.systemd.Basic.Info",
            None,
            None,
            serde_json::json!({"enabled": true, "flags": [false, true], "inner": {"on": false}}),
        )];
        let normalized = normalize_metrics("io.systemd.Basic", &metrics, &HashSet::new());
        assert_eq!(
            normalized.metrics["Info"],
            serde_json::json!({"enabled": 1, "flags": [0, 1], "inner": {"on": 0}})
        );
    }

    #[test]
    fn test_normalize_metrics_caps_per_source() {
        let metrics: Vec<ListOutput> = (0..METRICS_PER_SOURCE_MAX + 2)
            .map(|i| {
                metric(
                    "io.systemd.Basic.Value",
                    Some(&format!("o{i}")),
                    None,
                    serde_json::json!(i),
                )
            })
            .collect();
        let normalized = normalize_metrics("io.systemd.Basic", &metrics, &HashSet::new());
        assert_eq!(normalized.metrics.len(), METRICS_PER_SOURCE_MAX);
        assert_eq!(normalized.dropped_metrics, 2);
    }

    #[tokio::test]
    async fn test_list_sources_filters_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _basic =
            std::os::unix::net::UnixListener::bind(dir.path().join("io.systemd.Basic")).unwrap();
        let _manager =
            std::os::unix::net::UnixListener::bind(dir.path().join("io.systemd.Manager")).unwrap();
        let _bad = std::os::unix::net::UnixListener::bind(dir.path().join("bad")).unwrap();
        std::fs::write(dir.path().join("io.systemd.File"), b"").unwrap();
        std::os::unix::fs::symlink(
            dir.path().join("io.systemd.Basic"),
            dir.path().join("io.systemd.Link"),
        )
        .unwrap();

        let empty = HashSet::new();
        let sources = list_sources(dir.path(), &empty, &empty).await.unwrap();
        assert_eq!(sources, vec!["io.systemd.Basic", "io.systemd.Link"]);

        let missing = list_sources(&dir.path().join("nope"), &empty, &empty)
            .await
            .unwrap();
        assert!(missing.is_empty());
    }

    #[tokio::test]
    async fn test_collect_report_stats_marks_dead_source_down() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Serves two metrics then ends the stream
        let good = tokio::net::UnixListener::bind(dir.path().join("io.example.Good")).unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = good.accept().await.expect("accept");
            let mut request = vec![0u8; 4096];
            let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut request).await;
            tokio::io::AsyncWriteExt::write_all(
                &mut socket,
                b"{\"parameters\":{\"name\":\"io.example.Good.Value\",\"value\":1},\"continues\":true}\0\
                  {\"parameters\":{\"name\":\"io.example.Good.Other\",\"object\":\"x\",\"value\":\"y\"}}\0",
            )
            .await
            .expect("write reply");
        });
        // A socket file nobody listens on: connect is refused
        drop(std::os::unix::net::UnixListener::bind(dir.path().join("io.example.Dead")).unwrap());

        let empty = HashSet::new();
        let stats =
            collect_report_stats(dir.path(), &empty, &empty, &empty, Duration::from_secs(5))
                .await
                .unwrap();
        server.await.expect("server");

        assert_eq!(stats.len(), 2);
        assert_eq!(stats["io.example.Dead"], ReportSource::default());
        let good = &stats["io.example.Good"];
        assert!(good.up);
        assert_eq!(good.metrics["Value"], serde_json::json!(1));
        assert_eq!(good.metrics["Other.x"], serde_json::json!("y"));
    }
}
