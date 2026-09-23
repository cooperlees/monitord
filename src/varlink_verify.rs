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
/// references them).
///
/// One pass doubles as the trust check, since this enumeration must match the
/// D-Bus set exactly for the two verify paths to check the same units: a
/// missing family, any skipped family member (no object or a non-string
/// state), or an empty result each bail so the caller falls back to D-Bus
/// instead of verifying a silently smaller set and reporting success. A
/// running systemd always has units, so none of these mean "idle system".
fn collect_unit_names(metrics: &[ListOutput]) -> anyhow::Result<Vec<String>> {
    let mut names: Vec<String> = Vec::new();
    let mut saw_family = false;
    let mut skipped = 0u64;
    for metric in metrics {
        if metric.name_suffix() != "UnitLoadState" {
            continue;
        }
        saw_family = true;
        match (metric.object(), metric.value().is_string()) {
            (Some(unit_name), true) => names.push(unit_name.to_string()),
            (object, is_string) => {
                skipped += 1;
                debug!(
                    "Skipping {} for {object:?}: non-string load state or missing object (is_string={is_string}, value={:?})",
                    metric.name(),
                    metric.value()
                );
            }
        }
    }
    if !saw_family {
        anyhow::bail!("metrics carry no UnitLoadState family, cannot enumerate units");
    }
    if skipped > 0 {
        anyhow::bail!(
            "{skipped} UnitLoadState metrics skipped (missing object or non-string state), cannot trust enumeration"
        );
    }
    names.sort();
    names.dedup();
    if names.is_empty() {
        // Unreachable unless the loop above changes: a seen family with no
        // skips always yields names. Kept so a future refactor can't turn
        // this into a silent verify-nothing-and-report-success.
        anyhow::bail!("UnitLoadState family present but no unit names enumerated");
    }
    Ok(names)
}

/// Enumerate all units over varlink.
pub async fn list_unit_names(socket_path: &str) -> anyhow::Result<Vec<String>> {
    let metrics = crate::varlink_units::collect_metrics(socket_path.into()).await?;
    collect_unit_names(&metrics)
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
        let mut metrics = vec![
            loaded("b.service"),
            loaded("a.service"),
            metric(Some("masked.service"), serde_json::json!("masked")),
            metric(Some("ghost.service"), serde_json::json!("not-found")),
            // Duplicates collapse: ListUnits never repeats a name either.
            loaded("a.service"),
        ];

        // A different family sharing the stream must not leak names in.
        metrics.push(ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: serde_json::json!("active"),
            object: Some("active-only.service".to_string()),
            fields: None,
        });

        assert_eq!(
            collect_unit_names(&metrics).expect("valid family collects"),
            vec![
                "a.service".to_string(),
                "b.service".to_string(),
                "ghost.service".to_string(),
                "masked.service".to_string(),
            ]
        );
    }

    #[test]
    fn test_skipped_family_members_reject_enumeration() {
        // Any skipped family member makes the set untrustworthy: bail so the
        // caller falls back to D-Bus rather than verifying a silent subset.
        for metrics in [
            vec![
                loaded("good.service"),
                metric(None, serde_json::json!("loaded")),
            ],
            vec![
                loaded("good.service"),
                metric(Some("odd.service"), serde_json::json!(3)),
            ],
            vec![
                loaded("good.service"),
                metric(Some("other.service"), serde_json::json!(null)),
            ],
        ] {
            let err = collect_unit_names(&metrics).expect_err("skips must bail");
            assert!(
                err.to_string().contains("skipped"),
                "unexpected error: {err}"
            );
        }
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
        let err = collect_unit_names(&without_family).expect_err("missing family must bail");
        assert!(
            err.to_string().contains("no UnitLoadState family"),
            "unexpected error: {err}"
        );

        assert!(collect_unit_names(&[loaded("foo.service")]).is_ok());
    }
}
