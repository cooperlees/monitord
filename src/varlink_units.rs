//! # units module
//!
//! All main systemd unit statistics. Counts of types of units, unit states and
//! queued jobs. We also house service specific statistics and system unit states.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::RwLock;
use tracing::debug;

use tracing::warn;

use crate::unit_constants::{is_unit_unhealthy, SystemdUnitActiveState, SystemdUnitLoadState};
use crate::units::SystemdUnitStats;
use crate::varlink::metrics::{ListOutput, Metrics};
use crate::MachineStats;
use futures_util::stream::TryStreamExt;
use zlink::unix;

pub const METRICS_SOCKET_PATH: &str = "/run/systemd/report/io.systemd.Manager";

/// Parse a string value from a metric into an enum type, warning on failure
fn parse_metric_enum<T: FromStr>(metric: &ListOutput) -> Option<T> {
    if !metric.value().is_string() {
        warn!(
            "Metric {} has non-string value: {:?}",
            metric.name(),
            metric.value()
        );
        return None;
    }
    let value_str = metric.value_as_string();
    // Normalize hyphens to underscores to match enum variant names (e.g. "not-found" -> "not_found"),
    // mirroring the same replacement done in the D-Bus path (units.rs::parse_state).
    let normalized = value_str.replace('-', "_");
    match T::from_str(&normalized) {
        Ok(v) => Some(v),
        Err(_) => {
            warn!(
                "Metric {} has unrecognized value: {:?}",
                metric.name(),
                value_str
            );
            None
        }
    }
}

/// Check if a unit name should be skipped based on allowlist/blocklist
fn should_skip_unit(object_name: &str, config: &crate::config::UnitsConfig) -> bool {
    if config.state_stats_blocklist.contains(object_name) {
        debug!("Skipping state stats for {} due to blocklist", object_name);
        return true;
    }
    if !config.state_stats_allowlist.is_empty()
        && !config.state_stats_allowlist.contains(object_name)
    {
        return true;
    }
    false
}

/// Parse state of a unit into our unit_states hash
pub fn parse_one_metric(
    stats: &mut SystemdUnitStats,
    metric: &ListOutput,
    config: &crate::config::UnitsConfig,
) -> anyhow::Result<()> {
    let metric_name_suffix = metric.name_suffix();
    let object_name = metric.object_name();

    match metric_name_suffix {
        "UnitActiveState" => {
            if !config.state_stats || should_skip_unit(&object_name, config) {
                return Ok(());
            }
            let active_state: SystemdUnitActiveState = match parse_metric_enum(metric) {
                Some(v) => v,
                None => return Ok(()),
            };
            let unit_state = stats
                .unit_states
                .entry(object_name.to_string())
                .or_default();
            unit_state.active_state = active_state;
            unit_state.unhealthy =
                is_unit_unhealthy(unit_state.active_state, unit_state.load_state);
        }
        "UnitLoadState" => {
            let load_state: SystemdUnitLoadState = match parse_metric_enum(metric) {
                Some(v) => v,
                None => return Ok(()),
            };
            // Always count aggregate load state totals, matching D-Bus parse_unit() behaviour
            // which counts every unit regardless of the state_stats allowlist.
            match load_state {
                SystemdUnitLoadState::loaded => stats.loaded_units += 1,
                SystemdUnitLoadState::masked => stats.masked_units += 1,
                SystemdUnitLoadState::not_found => stats.not_found_units += 1,
                _ => {}
            }
            // Per-unit state tracking is gated by config.
            if !config.state_stats || should_skip_unit(&object_name, config) {
                return Ok(());
            }
            let unit_state = stats
                .unit_states
                .entry(object_name.to_string())
                .or_default();
            unit_state.load_state = load_state;
            unit_state.unhealthy =
                is_unit_unhealthy(unit_state.active_state, unit_state.load_state);
        }
        "NRestarts" => {
            if !config.state_stats || should_skip_unit(&object_name, config) {
                return Ok(());
            }
            if !metric.value().is_i64() {
                warn!(
                    "Metric {} has non-integer value: {:?}",
                    metric.name(),
                    metric.value()
                );
                return Ok(());
            }
            let value = metric.value_as_int();
            let nrestarts: u32 = match value.try_into() {
                Ok(v) => v,
                Err(_) => {
                    warn!(
                        "Metric {} has out-of-range value for u32: {}",
                        metric.name(),
                        value
                    );
                    return Ok(());
                }
            };
            stats
                .service_stats
                .entry(object_name.to_string())
                .or_default()
                .nrestarts = Some(nrestarts);
        }
        "UnitsByTypeTotal" => {
            if let Some(type_str) = metric.get_field_as_str("type") {
                if !metric.value().is_i64() {
                    warn!(
                        "Metric {} has non-integer value: {:?}",
                        metric.name(),
                        metric.value()
                    );
                    return Ok(());
                }
                let value = metric.value_as_int();
                let value: u64 = match value.try_into() {
                    Ok(v) => v,
                    Err(_) => {
                        warn!("Metric {} has negative value: {}", metric.name(), value);
                        return Ok(());
                    }
                };
                match type_str {
                    "automount" => stats.automount_units = value,
                    "device" => stats.device_units = value,
                    "mount" => stats.mount_units = value,
                    "path" => stats.path_units = value,
                    "scope" => stats.scope_units = value,
                    "service" => stats.service_units = value,
                    "slice" => stats.slice_units = value,
                    "socket" => stats.socket_units = value,
                    "target" => stats.target_units = value,
                    "timer" => stats.timer_units = value,
                    _ => debug!("Found unhandled unit type: {:?}", type_str),
                }
            }
        }
        "UnitsByStateTotal" => {
            if let Some(state_str) = metric.get_field_as_str("state") {
                if !metric.value().is_i64() {
                    warn!(
                        "Metric {} has non-integer value: {:?}",
                        metric.name(),
                        metric.value()
                    );
                    return Ok(());
                }
                let value = metric.value_as_int();
                let value: u64 = match value.try_into() {
                    Ok(v) => v,
                    Err(_) => {
                        warn!("Metric {} has negative value: {}", metric.name(), value);
                        return Ok(());
                    }
                };
                match state_str {
                    "active" => stats.active_units = value,
                    "failed" => stats.failed_units = value,
                    "inactive" => stats.inactive_units = value,
                    _ => debug!("Found unhandled unit state: {:?}", state_str),
                }
            }
        }
        _ => debug!("Found unhandled metric: {:?}", metric.name()),
    }

    Ok(())
}

/// Collect all metrics from the varlink socket.
/// Runs on a blocking thread with a dedicated runtime because the zlink
/// stream is !Send and cannot be held across await points in a Send future.
async fn collect_metrics(socket_path: String) -> anyhow::Result<Vec<ListOutput>> {
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(async move {
            let mut conn = unix::connect(&socket_path).await?;
            let stream = conn.list().await?;
            futures_util::pin_mut!(stream);

            let mut metrics = Vec::new();
            let mut count = 0;
            while let Some(result) = stream.try_next().await? {
                let result: std::result::Result<ListOutput, _> = result;
                match result {
                    Ok(metric) => {
                        debug!("Metrics {}: {:?}", count, metric);
                        count += 1;
                        metrics.push(metric);
                    }
                    Err(e) => {
                        debug!("Error deserializing metric {}: {:?}", count, e);
                        return Err(anyhow::anyhow!(e));
                    }
                }
            }
            Ok(metrics)
        })
    })
    .await?
}

pub async fn parse_metrics(
    stats: &mut SystemdUnitStats,
    socket_path: &str,
    config: &crate::config::UnitsConfig,
) -> anyhow::Result<()> {
    // Parity with the D-Bus path's UnitsCollectionTimings: list_units_ms is the
    // bulk fetch (varlink List on io.systemd.Manager), per_unit_loop_ms is the
    // local parse loop. The *_dbus_fetches counters stay 0 here -- itself a
    // useful signal that the varlink path doesn't pay per-unit D-Bus cost.
    let bulk_fetch_start = Instant::now();
    let metrics = collect_metrics(socket_path.to_string()).await?;
    let bulk_fetch_elapsed = bulk_fetch_start.elapsed();
    stats.collection_timings.list_units_ms = bulk_fetch_elapsed.as_secs_f64() * 1000.0;

    let parse_loop_start = Instant::now();
    for metric in &metrics {
        parse_one_metric(stats, metric, config)?;
    }
    let parse_loop_elapsed = parse_loop_start.elapsed();
    stats.collection_timings.per_unit_loop_ms = parse_loop_elapsed.as_secs_f64() * 1000.0;

    Ok(())
}

pub async fn get_unit_stats(
    config: &crate::config::Config,
    socket_path: &str,
) -> anyhow::Result<SystemdUnitStats> {
    if !config.units.state_stats_allowlist.is_empty() {
        debug!(
            "Using unit state allowlist: {:?}",
            config.units.state_stats_allowlist
        );
    }

    if !config.units.state_stats_blocklist.is_empty() {
        debug!(
            "Using unit state blocklist: {:?}",
            config.units.state_stats_blocklist,
        );
    }

    let mut stats = SystemdUnitStats::default();

    // Always collect metrics to get aggregate counts (UnitsByTypeTotal, UnitsByStateTotal)
    // as well as per-unit state data when config.units.state_stats is enabled.
    parse_metrics(&mut stats, socket_path, &config.units).await?;

    // Derive total_units from the sum of all per-type counts, mirroring what the D-Bus
    // path computes as `units.len()` from list_units().
    stats.total_units = stats.automount_units
        + stats.device_units
        + stats.mount_units
        + stats.path_units
        + stats.scope_units
        + stats.service_units
        + stats.slice_units
        + stats.socket_units
        + stats.target_units
        + stats.timer_units;

    debug!("unit stats: {:?}", stats);
    Ok(stats)
}

/// Async wrapper that can update unit stats when passed a locked struct.
pub async fn update_unit_stats(
    config: Arc<crate::config::Config>,
    locked_machine_stats: Arc<RwLock<MachineStats>>,
    socket_path: String,
) -> anyhow::Result<()> {
    let units_stats = get_unit_stats(&config, &socket_path).await?;
    let mut machine_stats = locked_machine_stats.write().await;
    machine_stats.units = units_stats;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn string_value(s: &str) -> serde_json::Value {
        serde_json::json!(s)
    }

    fn int_value(i: i64) -> serde_json::Value {
        serde_json::json!(i)
    }

    fn empty_value() -> serde_json::Value {
        serde_json::Value::Null
    }

    fn default_units_config() -> crate::config::UnitsConfig {
        crate::config::UnitsConfig {
            enabled: true,
            state_stats: true,
            state_stats_allowlist: HashSet::new(),
            state_stats_blocklist: HashSet::new(),
            state_stats_time_in_state: false,
            unit_files: true,
        }
    }

    #[tokio::test]
    async fn test_parse_one_metric_unit_active_state() {
        let mut stats = SystemdUnitStats::default();
        let config = default_units_config();

        let metric = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("active"),
            object: Some("my-service.service".to_string()),
            fields: None,
        };

        parse_one_metric(&mut stats, &metric, &config).unwrap();

        assert_eq!(
            stats
                .unit_states
                .get("my-service.service")
                .unwrap()
                .active_state,
            SystemdUnitActiveState::active
        );
    }

    #[tokio::test]
    async fn test_parse_one_metric_unit_load_state() {
        let mut stats = SystemdUnitStats::default();
        let config = default_units_config();

        // systemd sends "not-found" with a hyphen over both D-Bus and varlink;
        // parse_metric_enum must normalize it to "not_found" before enum parsing.
        let metric = ListOutput {
            name: "io.systemd.Manager.UnitLoadState".to_string(),
            value: string_value("not-found"),
            object: Some("missing.service".to_string()),
            fields: None,
        };

        parse_one_metric(&mut stats, &metric, &config).unwrap();

        assert_eq!(
            stats.unit_states.get("missing.service").unwrap().load_state,
            SystemdUnitLoadState::not_found
        );
    }

    #[tokio::test]
    async fn test_parse_one_metric_nrestarts() {
        let mut stats = SystemdUnitStats::default();
        let config = default_units_config();

        let metric = ListOutput {
            name: "io.systemd.Manager.NRestarts".to_string(),
            value: int_value(5),
            object: Some("my-service.service".to_string()),
            fields: None,
        };

        parse_one_metric(&mut stats, &metric, &config).unwrap();

        assert_eq!(
            stats
                .service_stats
                .get("my-service.service")
                .unwrap()
                .nrestarts,
            Some(5)
        );
    }

    #[tokio::test]
    async fn test_parse_aggregated_metrics() {
        let mut stats = SystemdUnitStats::default();
        let config = default_units_config();

        // Test UnitsByTypeTotal
        let type_metric = ListOutput {
            name: "io.systemd.Manager.UnitsByTypeTotal".to_string(),
            value: int_value(42),
            object: None,
            fields: Some(std::collections::HashMap::from([(
                "type".to_string(),
                serde_json::json!("service"),
            )])),
        };
        parse_one_metric(&mut stats, &type_metric, &config).unwrap();
        assert_eq!(stats.service_units, 42);

        // Test UnitsByStateTotal
        let state_metric = ListOutput {
            name: "io.systemd.Manager.UnitsByStateTotal".to_string(),
            value: int_value(10),
            object: None,
            fields: Some(std::collections::HashMap::from([(
                "state".to_string(),
                serde_json::json!("active"),
            )])),
        };
        parse_one_metric(&mut stats, &state_metric, &config).unwrap();
        assert_eq!(stats.active_units, 10);
    }

    #[tokio::test]
    async fn test_parse_multiple_units() {
        let mut stats = SystemdUnitStats::default();
        let config = default_units_config();

        let metrics = vec![
            ListOutput {
                name: "io.systemd.Manager.UnitActiveState".to_string(),
                value: string_value("active"),
                object: Some("service1.service".to_string()),
                fields: None,
            },
            ListOutput {
                name: "io.systemd.Manager.UnitLoadState".to_string(),
                value: string_value("loaded"),
                object: Some("service1.service".to_string()),
                fields: None,
            },
            ListOutput {
                name: "io.systemd.Manager.UnitActiveState".to_string(),
                value: string_value("failed"),
                object: Some("service-2.service".to_string()),
                fields: None,
            },
        ];

        for metric in metrics {
            parse_one_metric(&mut stats, &metric, &config).unwrap();
        }

        assert_eq!(stats.unit_states.len(), 2);
        assert_eq!(
            stats
                .unit_states
                .get("service1.service")
                .unwrap()
                .active_state,
            SystemdUnitActiveState::active
        );
        assert_eq!(
            stats
                .unit_states
                .get("service1.service")
                .unwrap()
                .load_state,
            SystemdUnitLoadState::loaded
        );
        assert_eq!(
            stats
                .unit_states
                .get("service-2.service")
                .unwrap()
                .active_state,
            SystemdUnitActiveState::failed
        );
    }

    #[tokio::test]
    async fn test_parse_unknown_and_missing_values() {
        let mut stats = SystemdUnitStats::default();
        let config = default_units_config();

        // Unknown active state is skipped (not silently defaulted)
        let metric1 = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("invalid_state"),
            object: Some("test.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric1, &config).unwrap();
        assert!(
            !stats.unit_states.contains_key("test.service"),
            "invalid state should be skipped"
        );

        // Missing nrestarts value (null) is skipped
        let metric2 = ListOutput {
            name: "io.systemd.Manager.NRestarts".to_string(),
            value: empty_value(),
            object: Some("test2.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric2, &config).unwrap();
        assert!(
            !stats.service_stats.contains_key("test2.service"),
            "null value should be skipped"
        );
    }

    #[tokio::test]
    async fn test_parse_edge_cases() {
        let mut stats = SystemdUnitStats::default();
        let config = default_units_config();

        // Unknown unit type is ignored gracefully
        let metric1 = ListOutput {
            name: "io.systemd.Manager.UnitsByTypeTotal".to_string(),
            value: int_value(999),
            object: None,
            fields: Some(std::collections::HashMap::from([(
                "type".to_string(),
                serde_json::json!("unknown_type"),
            )])),
        };
        parse_one_metric(&mut stats, &metric1, &config).unwrap();
        assert_eq!(stats.service_units, 0);

        // Metric with no fields is handled gracefully
        let metric2 = ListOutput {
            name: "io.systemd.Manager.UnitsByTypeTotal".to_string(),
            value: int_value(42),
            object: None,
            fields: None,
        };
        parse_one_metric(&mut stats, &metric2, &config).unwrap();

        // Non-string field value is ignored
        let metric3 = ListOutput {
            name: "io.systemd.Manager.UnitsByTypeTotal".to_string(),
            value: int_value(42),
            object: None,
            fields: Some(std::collections::HashMap::from([(
                "type".to_string(),
                serde_json::json!(123),
            )])),
        };
        parse_one_metric(&mut stats, &metric3, &config).unwrap();

        // Unhandled metric name is ignored
        let metric4 = ListOutput {
            name: "io.systemd.Manager.UnknownMetric".to_string(),
            value: int_value(999),
            object: Some("test.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric4, &config).unwrap();
    }

    #[test]
    fn test_state_stats_disabled_skips_per_unit_data() {
        // When state_stats=false, UnitActiveState / UnitLoadState / NRestarts should be
        // skipped by parse_one_metric so that unit_states and service_stats remain empty.
        let config = crate::config::UnitsConfig {
            enabled: true,
            state_stats: false,
            state_stats_allowlist: HashSet::new(),
            state_stats_blocklist: HashSet::new(),
            state_stats_time_in_state: true,
            unit_files: true,
        };
        let mut stats = SystemdUnitStats::default();

        let active_state_metric = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("active"),
            object: Some("test.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &active_state_metric, &config).unwrap();

        let load_state_metric = ListOutput {
            name: "io.systemd.Manager.UnitLoadState".to_string(),
            value: string_value("loaded"),
            object: Some("test.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &load_state_metric, &config).unwrap();

        let nrestarts_metric = ListOutput {
            name: "io.systemd.Manager.NRestarts".to_string(),
            value: int_value(3),
            object: Some("test.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &nrestarts_metric, &config).unwrap();

        // Per-unit state data must be absent when state_stats=false
        assert_eq!(stats.unit_states.len(), 0);
        assert_eq!(stats.service_stats.len(), 0);

        // But aggregate type/state counts must still be processed (they are not gated on state_stats)
        let type_metric = ListOutput {
            name: "io.systemd.Manager.UnitsByTypeTotal".to_string(),
            value: int_value(10),
            object: None,
            fields: Some(std::collections::HashMap::from([(
                "type".to_string(),
                serde_json::json!("service"),
            )])),
        };
        parse_one_metric(&mut stats, &type_metric, &config).unwrap();
        assert_eq!(stats.service_units, 10);
    }

    #[test]
    fn test_parse_metric_enum() {
        let metric_active = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("active"),
            object: Some("test.service".to_string()),
            fields: None,
        };
        assert_eq!(
            parse_metric_enum::<SystemdUnitActiveState>(&metric_active),
            Some(SystemdUnitActiveState::active)
        );

        let metric_loaded = ListOutput {
            name: "io.systemd.Manager.UnitLoadState".to_string(),
            value: string_value("loaded"),
            object: Some("test.service".to_string()),
            fields: None,
        };
        assert_eq!(
            parse_metric_enum::<SystemdUnitLoadState>(&metric_loaded),
            Some(SystemdUnitLoadState::loaded)
        );

        // Invalid value returns None
        let metric_invalid = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("invalid"),
            object: Some("test.service".to_string()),
            fields: None,
        };
        assert_eq!(
            parse_metric_enum::<SystemdUnitActiveState>(&metric_invalid),
            None
        );

        // Null value returns None
        let metric_empty = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: empty_value(),
            object: Some("test.service".to_string()),
            fields: None,
        };
        assert_eq!(
            parse_metric_enum::<SystemdUnitActiveState>(&metric_empty),
            None
        );
    }

    #[test]
    fn test_parse_metric_enum_all_states() {
        // Test all active states
        let active_states = vec![
            ("active", SystemdUnitActiveState::active),
            ("reloading", SystemdUnitActiveState::reloading),
            ("inactive", SystemdUnitActiveState::inactive),
            ("failed", SystemdUnitActiveState::failed),
            ("activating", SystemdUnitActiveState::activating),
            ("deactivating", SystemdUnitActiveState::deactivating),
        ];

        for (state_str, expected) in active_states {
            let metric = ListOutput {
                name: "io.systemd.Manager.UnitActiveState".to_string(),
                value: string_value(state_str),
                object: Some("test.service".to_string()),
                fields: None,
            };
            assert_eq!(
                parse_metric_enum::<SystemdUnitActiveState>(&metric),
                Some(expected)
            );
        }

        // Test all load states
        let load_states = vec![
            ("loaded", SystemdUnitLoadState::loaded),
            ("error", SystemdUnitLoadState::error),
            ("masked", SystemdUnitLoadState::masked),
            ("not_found", SystemdUnitLoadState::not_found),
        ];

        for (state_str, expected) in load_states {
            let metric = ListOutput {
                name: "io.systemd.Manager.UnitLoadState".to_string(),
                value: string_value(state_str),
                object: Some("test.service".to_string()),
                fields: None,
            };
            assert_eq!(
                parse_metric_enum::<SystemdUnitLoadState>(&metric),
                Some(expected)
            );
        }
    }

    #[tokio::test]
    async fn test_parse_state_updates() {
        let mut stats = SystemdUnitStats::default();
        let config = default_units_config();

        // Parse initial state
        let metric1 = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("inactive"),
            object: Some("test.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric1, &config).unwrap();
        assert_eq!(
            stats.unit_states.get("test.service").unwrap().active_state,
            SystemdUnitActiveState::inactive
        );

        // Update to active state
        let metric2 = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("active"),
            object: Some("test.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric2, &config).unwrap();
        assert_eq!(
            stats.unit_states.get("test.service").unwrap().active_state,
            SystemdUnitActiveState::active
        );
    }

    #[tokio::test]
    async fn test_unhealthy_computed() {
        let mut stats = SystemdUnitStats::default();
        let config = default_units_config();

        // Set active state to failed
        let metric1 = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("failed"),
            object: Some("broken.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric1, &config).unwrap();

        // Set load state to loaded
        let metric2 = ListOutput {
            name: "io.systemd.Manager.UnitLoadState".to_string(),
            value: string_value("loaded"),
            object: Some("broken.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric2, &config).unwrap();

        // Should be unhealthy: loaded + failed
        assert!(stats.unit_states.get("broken.service").unwrap().unhealthy);

        // Set active state to active
        let metric3 = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("active"),
            object: Some("healthy.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric3, &config).unwrap();

        // Set load state to loaded
        let metric4 = ListOutput {
            name: "io.systemd.Manager.UnitLoadState".to_string(),
            value: string_value("loaded"),
            object: Some("healthy.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric4, &config).unwrap();

        // Should be healthy: loaded + active
        assert!(!stats.unit_states.get("healthy.service").unwrap().unhealthy);
    }

    #[tokio::test]
    async fn test_allowlist_filtering() {
        let mut stats = SystemdUnitStats::default();
        let config = crate::config::UnitsConfig {
            enabled: true,
            state_stats: true,
            state_stats_allowlist: HashSet::from(["allowed.service".to_string()]),
            state_stats_blocklist: HashSet::new(),
            state_stats_time_in_state: false,
            unit_files: true,
        };

        // Allowed unit should be tracked
        let metric1 = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("active"),
            object: Some("allowed.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric1, &config).unwrap();
        assert!(stats.unit_states.contains_key("allowed.service"));

        // Non-allowed unit should be skipped
        let metric2 = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("active"),
            object: Some("not-allowed.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric2, &config).unwrap();
        assert!(!stats.unit_states.contains_key("not-allowed.service"));
    }

    #[tokio::test]
    async fn test_blocklist_filtering() {
        let mut stats = SystemdUnitStats::default();
        let config = crate::config::UnitsConfig {
            enabled: true,
            state_stats: true,
            state_stats_allowlist: HashSet::new(),
            state_stats_blocklist: HashSet::from(["blocked.service".to_string()]),
            state_stats_time_in_state: false,
            unit_files: true,
        };

        // Blocked unit should be skipped
        let metric1 = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("active"),
            object: Some("blocked.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric1, &config).unwrap();
        assert!(!stats.unit_states.contains_key("blocked.service"));

        // Non-blocked unit should be tracked
        let metric2 = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("active"),
            object: Some("ok.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric2, &config).unwrap();
        assert!(stats.unit_states.contains_key("ok.service"));
    }

    #[tokio::test]
    async fn test_blocklist_overrides_allowlist() {
        let mut stats = SystemdUnitStats::default();
        let config = crate::config::UnitsConfig {
            enabled: true,
            state_stats: true,
            state_stats_allowlist: HashSet::from(["both.service".to_string()]),
            state_stats_blocklist: HashSet::from(["both.service".to_string()]),
            state_stats_time_in_state: false,
            unit_files: true,
        };

        // Unit in both lists should be blocked (blocklist takes priority)
        let metric = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("active"),
            object: Some("both.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric, &config).unwrap();
        assert!(!stats.unit_states.contains_key("both.service"));
    }

    #[test]
    fn test_load_state_counts_bypass_allowlist() {
        // loaded_units/masked_units/not_found_units must be counted for every unit,
        // regardless of the state_stats allowlist (matching D-Bus parse_unit() behaviour).
        let config = crate::config::UnitsConfig {
            enabled: true,
            state_stats: true,
            // Only "allowed.service" is in the allowlist
            state_stats_allowlist: HashSet::from(["allowed.service".to_string()]),
            state_stats_blocklist: HashSet::new(),
            state_stats_time_in_state: false,
            unit_files: true,
        };
        let mut stats = SystemdUnitStats::default();

        let metrics = vec![
            // allowed unit → counts AND stored in unit_states
            ListOutput {
                name: "io.systemd.Manager.UnitLoadState".to_string(),
                value: string_value("loaded"),
                object: Some("allowed.service".to_string()),
                fields: None,
            },
            // non-allowed unit → only counted, NOT stored in unit_states
            ListOutput {
                name: "io.systemd.Manager.UnitLoadState".to_string(),
                value: string_value("loaded"),
                object: Some("other.service".to_string()),
                fields: None,
            },
            ListOutput {
                name: "io.systemd.Manager.UnitLoadState".to_string(),
                value: string_value("not-found"), // systemd sends hyphenated form over the wire
                object: Some("missing.service".to_string()),
                fields: None,
            },
            ListOutput {
                name: "io.systemd.Manager.UnitLoadState".to_string(),
                value: string_value("masked"),
                object: Some("masked.service".to_string()),
                fields: None,
            },
        ];
        for m in metrics {
            parse_one_metric(&mut stats, &m, &config).unwrap();
        }

        // Aggregate counts include ALL units regardless of allowlist
        assert_eq!(stats.loaded_units, 2);
        assert_eq!(stats.not_found_units, 1);
        assert_eq!(stats.masked_units, 1);
        // per-unit state only tracked for the allowed unit
        assert_eq!(stats.unit_states.len(), 1);
        assert!(stats.unit_states.contains_key("allowed.service"));
    }

    #[test]
    fn test_load_state_counts_when_state_stats_disabled() {
        // Even when state_stats=false, aggregate load state counts must be populated.
        let config = crate::config::UnitsConfig {
            enabled: true,
            state_stats: false,
            state_stats_allowlist: HashSet::new(),
            state_stats_blocklist: HashSet::new(),
            state_stats_time_in_state: false,
            unit_files: true,
        };
        let mut stats = SystemdUnitStats::default();

        let metrics = vec![
            ListOutput {
                name: "io.systemd.Manager.UnitLoadState".to_string(),
                value: string_value("loaded"),
                object: Some("svc1.service".to_string()),
                fields: None,
            },
            ListOutput {
                name: "io.systemd.Manager.UnitLoadState".to_string(),
                value: string_value("not-found"), // systemd sends hyphenated form over the wire
                object: Some("svc2.service".to_string()),
                fields: None,
            },
        ];
        for m in metrics {
            parse_one_metric(&mut stats, &m, &config).unwrap();
        }

        assert_eq!(stats.loaded_units, 1);
        assert_eq!(stats.not_found_units, 1);
        // No per-unit state tracking when state_stats=false
        assert_eq!(stats.unit_states.len(), 0);
    }
}
