//! # varlink_verify module
//!
//! Unit enumeration for `systemd-analyze verify` from the `io.systemd.Metrics`
//! stream rather than D-Bus `ListUnits`. This reads only the per-unit
//! `UnitLoadState` family, which the metrics endpoint has carried since the
//! report framework appeared in v260 — so unlike the timestamp families, no
//! v261 minimum applies here.
//!
//! Per the `varlink_unit` module docs, collectors that want every unit are
//! served from the metrics stream: one streamed `List` instead of hundreds of
//! per-unit round trips (over either transport).

use tracing::debug;

use crate::varlink::metrics::ListOutput;

pub use crate::varlink_units::METRICS_SOCKET_PATH;

/// Collect unit names from per-unit load-state metrics, sorted and deduplicated.
///
/// Every object is kept, whatever its load state: the metric objects are the
/// same set D-Bus `ListUnits` returns (verified live: 199 == 199, including
/// `not-found` units such as `syslog.service` that exist only because something
/// references them). Dropping any would make `systemd-analyze verify` check a
/// smaller set than the D-Bus path. Metrics without an object are skipped.
fn collect_unit_names(metrics: &[ListOutput]) -> Vec<String> {
    let mut names: Vec<String> = metrics
        .iter()
        .filter(|metric| metric.name_suffix() == "UnitLoadState")
        .filter_map(|metric| {
            let unit_name = metric.object()?;
            if !metric.value().is_string() {
                debug!(
                    "Skipping {} for {unit_name}: non-string load state {:?}",
                    metric.name(),
                    metric.value()
                );
                return None;
            }
            Some(unit_name.to_string())
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Whether the stream carries the per-unit load-state family at all.
///
/// A running systemd always has units, so seeing no `UnitLoadState` metrics
/// means the family is missing (or the socket answered with something
/// unexpected) rather than the system being idle. Without this guard the
/// verify run would check nothing and report success.
fn has_load_state_metrics(metrics: &[ListOutput]) -> bool {
    metrics
        .iter()
        .any(|metric| metric.name_suffix() == "UnitLoadState")
}

/// Enumerate all units over varlink.
pub async fn list_unit_names(socket_path: &str) -> anyhow::Result<Vec<String>> {
    let metrics = crate::varlink_units::collect_metrics(socket_path.to_string()).await?;
    if !has_load_state_metrics(&metrics) {
        anyhow::bail!("metrics carry no UnitLoadState family, cannot enumerate units");
    }
    Ok(collect_unit_names(&metrics))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metric(unit: Option<&str>, load_state: serde_json::Value) -> ListOutput {
        ListOutput {
            name: "io.systemd.Manager.UnitLoadState".to_string(),
            value: load_state,
            object: unit.map(|unit| unit.to_string()),
            fields: None,
        }
    }

    fn loaded(unit: &str) -> ListOutput {
        metric(Some(unit), serde_json::json!("loaded"))
    }

    #[test]
    fn test_collects_every_object_whatever_its_load_state() {
        // The metric objects are the same set ListUnits returns — including
        // not-found units, which analyze then reports on exactly as the D-Bus
        // path sees. Verified live: identical 199-unit sets on both sides.
        let metrics = vec![
            loaded("b.service"),
            loaded("a.service"),
            metric(Some("masked.service"), serde_json::json!("masked")),
            metric(Some("ghost.service"), serde_json::json!("not-found")),
            // Duplicates collapse: ListUnits never repeats a name either.
            loaded("a.service"),
        ];

        assert_eq!(
            collect_unit_names(&metrics),
            vec![
                "a.service".to_string(),
                "b.service".to_string(),
                "ghost.service".to_string(),
                "masked.service".to_string(),
            ]
        );
    }

    #[test]
    fn test_skips_metrics_without_object_or_string_state() {
        let metrics = vec![
            metric(None, serde_json::json!("loaded")),
            metric(Some("odd.service"), serde_json::json!(3)),
            metric(Some("other.service"), serde_json::json!(null)),
            // A different family sharing the stream must not leak names in.
            ListOutput {
                name: "io.systemd.Manager.UnitActiveState".to_string(),
                value: serde_json::json!("active"),
                object: Some("active-only.service".to_string()),
                fields: None,
            },
        ];

        assert!(collect_unit_names(&metrics).is_empty());
    }

    #[test]
    fn test_missing_family_is_rejected() {
        // No UnitLoadState metrics at all: the guard must trip so the caller
        // falls back to D-Bus instead of verifying nothing and reporting success.
        let without_family = vec![ListOutput {
            name: "io.systemd.Manager.UnitsTotal".to_string(),
            value: serde_json::json!(295),
            object: None,
            fields: None,
        }];
        assert!(!has_load_state_metrics(&without_family));
        assert!(collect_unit_names(&without_family).is_empty());

        assert!(has_load_state_metrics(&[loaded("foo.service")]));
    }
}
