//! # varlink_boot module
//!
//! Boot blame from the `io.systemd.Metrics` stream rather than per-unit D-Bus
//! property reads. Available from systemd v261+, which is when the metrics
//! endpoint gained the per-unit `ActiveTimestamp`/`InactiveExitTimestamp`
//! families this reads.
//!
//! The D-Bus path costs one `ListUnits` plus two property reads for *every*
//! unit on the system — several hundred round trips, which is why boot blame
//! ships disabled. Here it is one streamed `List`, and only once per boot:
//! `boot::update_boot_blame_stats` caches the result in memory and on disk
//! keyed by boot id, so a daemon pays this on its first cycle and never again.

use std::collections::HashMap;

use tracing::debug;

use crate::boot::BootBlameStats;
use crate::config::BootBlameConfig;
use crate::varlink::metrics::ListOutput;

pub use crate::varlink_units::METRICS_SOCKET_PATH;

/// The two timestamps a unit's activation time is derived from.
#[derive(Default)]
struct UnitActivation {
    /// When the unit entered the active state.
    active_enter_usec: u64,
    /// When it left the inactive state, i.e. when activation began.
    inactive_exit_usec: u64,
}

/// Fold the per-unit timestamp metrics into one entry per unit.
///
/// `ActiveTimestamp` is emitted twice per unit, distinguished by an `event`
/// field of `enter` or `exit`; only `enter` is wanted here, matching the D-Bus
/// path's `ActiveEnterTimestamp` property.
fn collect_activations(
    metrics: &[ListOutput],
    config: &BootBlameConfig,
) -> HashMap<String, UnitActivation> {
    let mut activations: HashMap<String, UnitActivation> = HashMap::new();
    for metric in metrics {
        let Some(unit_name) = metric.object() else {
            continue;
        };
        if config.blocklist.contains(unit_name) {
            debug!("Skipping boot blame for {} due to blocklist", unit_name);
            continue;
        }
        if !config.allowlist.is_empty() && !config.allowlist.contains(unit_name) {
            continue;
        }
        if !metric.value().is_i64() {
            debug!(
                "Skipping {} for {}: non-integer value {:?}",
                metric.name(),
                unit_name,
                metric.value()
            );
            continue;
        }
        let Ok(value) = u64::try_from(metric.value_as_int()) else {
            debug!(
                "Skipping {} for {}: negative value {}",
                metric.name(),
                unit_name,
                metric.value_as_int()
            );
            continue;
        };

        match metric.name_suffix() {
            "ActiveTimestamp" if metric.get_field_as_str("event") == Some("enter") => {
                activations
                    .entry(unit_name.to_string())
                    .or_default()
                    .active_enter_usec = value;
            }
            "InactiveExitTimestamp" => {
                activations
                    .entry(unit_name.to_string())
                    .or_default()
                    .inactive_exit_usec = value;
            }
            _ => {}
        }
    }
    activations
}

/// Rank units by activation time, slowest first, keeping the configured count.
///
/// Same arithmetic and same exclusions as `boot::get_unit_activation_time`: a
/// unit with either timestamp at zero never activated and is dropped, as is one
/// whose activation rounds to zero seconds.
fn rank_slowest(
    activations: HashMap<String, UnitActivation>,
    num_slowest: usize,
) -> BootBlameStats {
    let mut unit_times: Vec<(String, f64)> = activations
        .into_iter()
        .filter_map(|(name, activation)| {
            if activation.active_enter_usec == 0 || activation.inactive_exit_usec == 0 {
                return None;
            }
            let elapsed_usec = activation
                .active_enter_usec
                .saturating_sub(activation.inactive_exit_usec);
            let elapsed_sec = elapsed_usec as f64 / 1_000_000.0;
            (elapsed_sec > 0.0).then_some((name, elapsed_sec))
        })
        .collect();

    unit_times.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    unit_times.truncate(num_slowest);
    unit_times.into_iter().collect()
}

/// Whether the stream carries the per-unit timestamp families at all.
///
/// systemd v260 answers on this socket but predates `ActiveTimestamp` and
/// `InactiveExitTimestamp`, so those metrics are simply absent and the blame
/// would come back empty while the call looked like a success — the same
/// silent-wrong-answer shape the per-unit context guard exists for. A running
/// systemd always has units, so seeing none of these means the family is
/// missing rather than the system being idle.
fn has_activation_metrics(metrics: &[ListOutput]) -> bool {
    metrics
        .iter()
        .any(|metric| metric.name_suffix() == "ActiveTimestamp")
}

/// Collect boot blame over varlink.
pub async fn get_boot_blame_stats(
    socket_path: &str,
    config: &BootBlameConfig,
) -> anyhow::Result<BootBlameStats> {
    let metrics = crate::varlink_units::collect_metrics(socket_path.to_string()).await?;
    if !has_activation_metrics(&metrics) {
        anyhow::bail!(
            "metrics carry no ActiveTimestamp family: this systemd is older than v261, \
             where the per-unit boot timestamps were added"
        );
    }
    let activations = collect_activations(&metrics, config);
    Ok(rank_slowest(activations, config.num_slowest_units as usize))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn metric(name: &str, unit: &str, value: i64, event: Option<&str>) -> ListOutput {
        ListOutput {
            name: format!("io.systemd.Manager.{name}"),
            value: serde_json::json!(value),
            object: Some(unit.to_string()),
            fields: event.map(|event| {
                std::collections::HashMap::from([("event".to_string(), serde_json::json!(event))])
            }),
        }
    }

    fn unit_metrics(unit: &str, inactive_exit: i64, active_enter: i64) -> Vec<ListOutput> {
        vec![
            metric("InactiveExitTimestamp", unit, inactive_exit, None),
            metric("ActiveTimestamp", unit, active_enter, Some("enter")),
            // systemd emits the exit event too; it must not be mistaken for the
            // enter one, which would invert the calculation.
            metric("ActiveTimestamp", unit, 0, Some("exit")),
        ]
    }

    fn config() -> BootBlameConfig {
        BootBlameConfig {
            enabled: true,
            num_slowest_units: 5,
            ..Default::default()
        }
    }

    #[test]
    fn test_ranks_slowest_units_first() {
        let mut metrics = unit_metrics("slow.service", 1_000_000, 4_000_000);
        metrics.extend(unit_metrics("quick.service", 1_000_000, 1_500_000));
        metrics.extend(unit_metrics("middling.service", 1_000_000, 3_000_000));

        let stats = rank_slowest(collect_activations(&metrics, &config()), 5);

        assert_eq!(stats.len(), 3);
        assert_eq!(stats.get("slow.service"), Some(&3.0));
        assert_eq!(stats.get("middling.service"), Some(&2.0));
        assert_eq!(stats.get("quick.service"), Some(&0.5));
    }

    #[test]
    fn test_truncates_to_the_configured_count() {
        let mut metrics = unit_metrics("a.service", 1_000_000, 9_000_000);
        metrics.extend(unit_metrics("b.service", 1_000_000, 5_000_000));
        metrics.extend(unit_metrics("c.service", 1_000_000, 2_000_000));

        let stats = rank_slowest(collect_activations(&metrics, &config()), 2);

        assert_eq!(stats.len(), 2);
        assert!(stats.contains_key("a.service"));
        assert!(stats.contains_key("b.service"));
        assert!(!stats.contains_key("c.service"));
    }

    #[test]
    fn test_units_that_never_activated_are_dropped() {
        // Either timestamp at zero means the unit never activated, exactly as
        // the D-Bus path treats it.
        let mut metrics = unit_metrics("never-ran.service", 0, 0);
        metrics.extend(unit_metrics("no-enter.service", 1_000_000, 0));
        metrics.extend(unit_metrics("no-exit.service", 0, 4_000_000));
        // A unit whose activation is below a microsecond rounds to zero and is
        // dropped rather than reported as an instant unit.
        metrics.extend(unit_metrics("instant.service", 1_000_000, 1_000_000));

        let stats = rank_slowest(collect_activations(&metrics, &config()), 5);

        assert!(stats.is_empty(), "got {stats:?}");
    }

    #[test]
    fn test_pre_v261_metrics_are_rejected() {
        // v260 answers on this socket but has no per-unit timestamp families,
        // so the stream parses fine and yields nothing. Without this the blame
        // would report empty and never fall back to D-Bus. CI only runs
        // Rawhide, so this shape has to be pinned here.
        let v260_shaped = vec![ListOutput {
            name: "io.systemd.Manager.UnitsTotal".to_string(),
            value: serde_json::json!(295),
            object: None,
            fields: None,
        }];
        assert!(!has_activation_metrics(&v260_shaped));

        assert!(has_activation_metrics(&unit_metrics(
            "foo.service",
            1_000_000,
            2_000_000
        )));
    }

    #[test]
    fn test_allowlist_and_blocklist() {
        let mut metrics = unit_metrics("wanted.service", 1_000_000, 3_000_000);
        metrics.extend(unit_metrics("blocked.service", 1_000_000, 9_000_000));
        metrics.extend(unit_metrics("unlisted.service", 1_000_000, 8_000_000));

        let blocked = BootBlameConfig {
            blocklist: HashSet::from(["blocked.service".to_string()]),
            ..config()
        };
        let stats = rank_slowest(collect_activations(&metrics, &blocked), 5);
        assert!(!stats.contains_key("blocked.service"));
        assert!(stats.contains_key("unlisted.service"));

        let allowed = BootBlameConfig {
            allowlist: HashSet::from(["wanted.service".to_string()]),
            ..config()
        };
        let stats = rank_slowest(collect_activations(&metrics, &allowed), 5);
        assert_eq!(stats.len(), 1);
        assert!(stats.contains_key("wanted.service"));
    }
}
