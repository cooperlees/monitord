//! # units module
//!
//! All main systemd unit statistics. Counts of types of units, unit states and
//! queued jobs. We also house service specific statistics and system unit states.

use std::collections::HashMap;
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::RwLock;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::debug;

use tracing::warn;

use crate::unit_constants::{
    is_unit_unhealthy, is_unit_unhealthy_for_service, SystemdUnitActiveState, SystemdUnitLoadState,
    SYSTEMD_SERVICE_SUFFIX,
};
use crate::units::SystemdUnitStats;
use crate::units::UnitsCollectionTimings;
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
///
/// `services` is the `[services]` config list: per-service stats are tracked
/// only for those units, mirroring the D-Bus path.
pub fn parse_one_metric(
    stats: &mut SystemdUnitStats,
    metric: &ListOutput,
    config: &crate::config::UnitsConfig,
    services: &HashSet<String>,
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
            // Service stats follow the [services] list (like the D-Bus path),
            // independent of the state_stats gating used for unit_states.
            if !services.contains(&object_name) {
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
                .nrestarts = nrestarts;
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
                    "activating" => stats.activating_units = value,
                    "active" => stats.active_units = value,
                    "failed" => stats.failed_units = value,
                    "inactive" => stats.inactive_units = value,
                    // Other states (reloading, deactivating, maintenance,
                    // refreshing) have no counter, matching the D-Bus path
                    // which also only counts the four states above.
                    _ => debug!("Found unhandled unit state: {:?}", state_str),
                }
            }
        }
        "JobsQueued" => {
            if !metric.value().is_i64() {
                warn!(
                    "Metric {} has non-integer value: {:?}",
                    metric.name(),
                    metric.value()
                );
                return Ok(());
            }
            let value = metric.value_as_int();
            match value.try_into() {
                Ok(value) => stats.jobs_queued = value,
                Err(_) => {
                    warn!("Metric {} has negative value: {}", metric.name(), value);
                }
            }
        }
        "UnitsTotal" => {
            if !metric.value().is_i64() {
                warn!(
                    "Metric {} has non-integer value: {:?}",
                    metric.name(),
                    metric.value()
                );
                return Ok(());
            }
            let value = metric.value_as_int();
            match value.try_into() {
                Ok(value) => stats.total_units = value,
                Err(_) => {
                    warn!("Metric {} has negative value: {}", metric.name(), value);
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
    services: &HashSet<String>,
) -> anyhow::Result<()> {
    // Parity with the D-Bus path's UnitsCollectionTimings: list_units_ms is the
    // bulk fetch (varlink List on io.systemd.Manager), per_unit_loop_ms is the
    // local parse loop. The oneshot D-Bus fallback later adds its own phase
    // duration to per_unit_loop_ms and its successful lookups to
    // service_dbus_fetches (see record_oneshot_lookup_timings); the remaining
    // *_dbus_fetches counters stay 0 on the varlink path.
    let bulk_fetch_start = Instant::now();
    let metrics = collect_metrics(socket_path.to_string()).await?;
    let bulk_fetch_elapsed = bulk_fetch_start.elapsed();
    stats.collection_timings.list_units_ms = bulk_fetch_elapsed.as_secs_f64() * 1000.0;

    let parse_loop_start = Instant::now();
    for metric in &metrics {
        parse_one_metric(stats, metric, config, services)?;
    }
    let parse_loop_elapsed = parse_loop_start.elapsed();
    stats.collection_timings.per_unit_loop_ms = parse_loop_elapsed.as_secs_f64() * 1000.0;

    Ok(())
}

/// Select unit names whose health needs a service-type check.
///
/// Mirrors the condition in `units::parse_state`: only inactive, loaded
/// `.service` units can be rescued by the oneshot override, so only those
/// need the `Service.Type` lookup. Pure function over already collected
/// stats, so it runs under a read lock without any I/O.
pub fn select_oneshot_candidates(
    stats: &SystemdUnitStats,
    config: &crate::config::UnitsConfig,
) -> Vec<String> {
    if !config.ignore_inactive_oneshot_services {
        return Vec::new();
    }
    stats
        .unit_states
        .iter()
        .filter(|(name, state)| {
            name.ends_with(SYSTEMD_SERVICE_SUFFIX)
                && matches!(state.active_state, SystemdUnitActiveState::inactive)
                && matches!(state.load_state, SystemdUnitLoadState::loaded)
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// Look up `Service.Type` for each candidate over D-Bus.
///
/// Service type is not exposed via the varlink metrics API, so the varlink
/// path resolves it here to keep `unhealthy` in parity with the D-Bus path.
/// (`io.systemd.Unit.List` does expose it as `context.Service.Type`, so this
/// lookup can go away once monitord adopts that API — see #37.)
/// Concurrency is bounded by `per_unit_concurrency`, like the D-Bus per-unit
/// loop. A lookup failure for one unit is logged and omitted from the map
/// (which `apply_oneshot_types` treats as "not oneshot") rather than failing
/// the whole collection, mirroring `units::parse_state`.
///
/// Returns the resolved types plus the number of successful lookups, so the
/// caller can account this phase in `UnitsCollectionTimings` like any other
/// per-service D-Bus work.
pub async fn fetch_oneshot_types(
    connection: &zbus::Connection,
    candidates: Vec<String>,
    per_unit_concurrency: u64,
) -> (HashMap<String, bool>, u64) {
    let semaphore = Arc::new(Semaphore::new(per_unit_concurrency.max(1) as usize));
    let mut join_set: JoinSet<(String, Option<bool>)> = JoinSet::new();
    for name in candidates {
        let semaphore = Arc::clone(&semaphore);
        let connection = connection.clone();
        join_set.spawn(async move {
            let _permit = semaphore
                .acquire()
                .await
                .expect("semaphore closed unexpectedly");
            let is_oneshot =
                match crate::units::is_oneshot_service_by_name(&connection, &name).await {
                    Ok(is_oneshot) => Some(is_oneshot),
                    Err(err) => {
                        warn!(
                            "Unable to get Service.Type for {} (assuming not oneshot): {:?}",
                            name, err
                        );
                        None
                    }
                };
            (name, is_oneshot)
        });
    }
    let mut types = HashMap::new();
    let mut successful_fetches: u64 = 0;
    while let Some(res) = join_set.join_next().await {
        match res {
            Ok((name, Some(is_oneshot))) => {
                types.insert(name, is_oneshot);
                successful_fetches += 1;
            }
            Ok((_, None)) => {}
            Err(err) => {
                warn!("Oneshot type lookup task failed to join: {:?}", err);
            }
        }
    }
    (types, successful_fetches)
}

/// Account a completed oneshot lookup phase in the collection timings.
///
/// Successful `Service.Type` resolutions are per-service D-Bus property
/// fetches, so they count toward `service_dbus_fetches`; the phase duration
/// folds into `per_unit_loop_ms`, the same bucket the D-Bus path uses for
/// its own per-unit work (including its oneshot checks).
fn record_oneshot_lookup_timings(
    timings: &mut UnitsCollectionTimings,
    elapsed_ms: f64,
    successful_fetches: u64,
) {
    timings.per_unit_loop_ms += elapsed_ms;
    timings.service_dbus_fetches += successful_fetches;
}

/// Recompute `unhealthy` for tracked units given resolved service types.
///
/// Units missing from `oneshot_types` (lookup failed or never a candidate)
/// are treated as "not oneshot", matching the D-Bus path's assumption.
pub fn apply_oneshot_types(
    stats: &mut SystemdUnitStats,
    oneshot_types: &HashMap<String, bool>,
    config: &crate::config::UnitsConfig,
) {
    if !config.ignore_inactive_oneshot_services {
        return;
    }
    for (unit_name, unit_state) in stats.unit_states.iter_mut() {
        let is_oneshot = oneshot_types.get(unit_name).copied().unwrap_or(false);
        unit_state.unhealthy = is_unit_unhealthy_for_service(
            unit_state.active_state,
            unit_state.load_state,
            is_oneshot,
            config.ignore_inactive_oneshot_services,
        );
    }
}

/// Apply the oneshot health override to varlink-collected unit stats.
///
/// Runs candidate selection under a read lock, resolves service types over
/// D-Bus without holding any lock, then applies the results under a write
/// lock — so D-Bus round trips never block other collectors on the shared
/// `MachineStats` lock.
pub async fn apply_oneshot_dbus_override(
    connection: &zbus::Connection,
    locked_machine_stats: &Arc<RwLock<MachineStats>>,
    config: &crate::config::UnitsConfig,
) {
    let candidates = {
        let machine_stats = locked_machine_stats.read().await;
        select_oneshot_candidates(&machine_stats.units, config)
    };
    if candidates.is_empty() {
        return;
    }
    let fetch_start = Instant::now();
    let (oneshot_types, successful_fetches) =
        fetch_oneshot_types(connection, candidates, config.per_unit_concurrency).await;
    let fetch_elapsed_ms = fetch_start.elapsed().as_secs_f64() * 1000.0;
    let mut machine_stats = locked_machine_stats.write().await;
    apply_oneshot_types(&mut machine_stats.units, &oneshot_types, config);
    record_oneshot_lookup_timings(
        &mut machine_stats.units.collection_timings,
        fetch_elapsed_ms,
        successful_fetches,
    );
}

/// Sum per-type counts as a fallback total for systemd versions whose metrics
/// lack `UnitsTotal`. Mirrors what the D-Bus path computes as `units.len()`,
/// except unmapped types (e.g. swap) are missed.
fn sum_units_by_type(stats: &SystemdUnitStats) -> u64 {
    stats.automount_units
        + stats.device_units
        + stats.mount_units
        + stats.path_units
        + stats.scope_units
        + stats.service_units
        + stats.slice_units
        + stats.socket_units
        + stats.target_units
        + stats.timer_units
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
    parse_metrics(&mut stats, socket_path, &config.units, &config.services).await?;

    // Prefer the UnitsTotal metric when present: it is exact, including unit
    // types we do not map (e.g. swap). Fall back to summing per-type counts
    // on systemd versions whose metrics lack it.
    if stats.total_units == 0 {
        stats.total_units = sum_units_by_type(&stats);
    }

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
            ignore_inactive_oneshot_services: true,
            unit_files: true,
            ..Default::default()
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

        parse_one_metric(&mut stats, &metric, &config, &HashSet::new())
            .expect("metric should parse successfully");

        assert_eq!(
            stats
                .unit_states
                .get("my-service.service")
                .expect("my-service.service should have a unit_states entry")
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

        parse_one_metric(&mut stats, &metric, &config, &HashSet::new())
            .expect("metric should parse successfully");

        assert_eq!(
            stats
                .unit_states
                .get("missing.service")
                .expect("missing.service should have a unit_states entry")
                .load_state,
            SystemdUnitLoadState::not_found
        );
    }

    #[test]
    fn test_parse_one_metric_nrestarts() {
        let mut stats = SystemdUnitStats::default();
        let config = default_units_config();
        // NRestarts is tracked per the [services] list, mirroring the D-Bus path.
        let services = HashSet::from(["my-service.service".to_string()]);

        let metric = ListOutput {
            name: "io.systemd.Manager.NRestarts".to_string(),
            value: int_value(5),
            object: Some("my-service.service".to_string()),
            fields: None,
        };

        parse_one_metric(&mut stats, &metric, &config, &services)
            .expect("metric should parse successfully");

        assert_eq!(
            stats
                .service_stats
                .get("my-service.service")
                .expect("my-service.service should have a service_stats entry")
                .nrestarts,
            5
        );

        // Units outside [services] get no service_stats entry even with data present.
        let other_metric = ListOutput {
            name: "io.systemd.Manager.NRestarts".to_string(),
            value: int_value(7),
            object: Some("other.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &other_metric, &config, &services)
            .expect("other_metric should parse successfully");
        assert!(!stats.service_stats.contains_key("other.service"));
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
        parse_one_metric(&mut stats, &type_metric, &config, &HashSet::new())
            .expect("type_metric should parse successfully");
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
        parse_one_metric(&mut stats, &state_metric, &config, &HashSet::new())
            .expect("state_metric should parse successfully");
        assert_eq!(stats.active_units, 10);

        // Test UnitsByStateTotal with activating state
        let activating_metric = ListOutput {
            name: "io.systemd.Manager.UnitsByStateTotal".to_string(),
            value: int_value(1),
            object: None,
            fields: Some(std::collections::HashMap::from([(
                "state".to_string(),
                serde_json::json!("activating"),
            )])),
        };
        parse_one_metric(&mut stats, &activating_metric, &config, &HashSet::new())
            .expect("activating_metric should parse successfully");
        assert_eq!(stats.activating_units, 1);

        // Test JobsQueued
        let jobs_metric = ListOutput {
            name: "io.systemd.Manager.JobsQueued".to_string(),
            value: int_value(3),
            object: None,
            fields: None,
        };
        parse_one_metric(&mut stats, &jobs_metric, &config, &HashSet::new())
            .expect("jobs_metric should parse successfully");
        assert_eq!(stats.jobs_queued, 3);

        // Test UnitsTotal
        let total_metric = ListOutput {
            name: "io.systemd.Manager.UnitsTotal".to_string(),
            value: int_value(196),
            object: None,
            fields: None,
        };
        parse_one_metric(&mut stats, &total_metric, &config, &HashSet::new())
            .expect("total_metric should parse successfully");
        assert_eq!(stats.total_units, 196);
    }

    #[test]
    fn test_sum_units_by_type_fallback() {
        // The fallback sums mapped per-type counts; unmapped types like swap
        // are missed, which is why the UnitsTotal metric is preferred.
        let stats = SystemdUnitStats {
            service_units: 86,
            timer_units: 2,
            ..Default::default()
        };
        assert_eq!(sum_units_by_type(&stats), 88);
        assert_eq!(sum_units_by_type(&SystemdUnitStats::default()), 0);
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
            parse_one_metric(&mut stats, &metric, &config, &HashSet::new())
                .expect("metric should parse successfully");
        }

        assert_eq!(stats.unit_states.len(), 2);
        assert_eq!(
            stats
                .unit_states
                .get("service1.service")
                .expect("service1.service should have a unit_states entry")
                .active_state,
            SystemdUnitActiveState::active
        );
        assert_eq!(
            stats
                .unit_states
                .get("service1.service")
                .expect("service1.service should have a unit_states entry")
                .load_state,
            SystemdUnitLoadState::loaded
        );
        assert_eq!(
            stats
                .unit_states
                .get("service-2.service")
                .expect("service-2.service should have a unit_states entry")
                .active_state,
            SystemdUnitActiveState::failed
        );
    }

    #[test]
    fn test_parse_unknown_and_missing_values() {
        let mut stats = SystemdUnitStats::default();
        let config = default_units_config();

        // Unknown active state is skipped (not silently defaulted)
        let metric1 = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("invalid_state"),
            object: Some("test.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric1, &config, &HashSet::new())
            .expect("metric1 should parse successfully");
        assert!(
            !stats.unit_states.contains_key("test.service"),
            "invalid state should be skipped"
        );

        // Missing nrestarts value (null) is skipped. The service must be in
        // [services] so the null-value path (not the gating) is what skips it.
        let metric2 = ListOutput {
            name: "io.systemd.Manager.NRestarts".to_string(),
            value: empty_value(),
            object: Some("test2.service".to_string()),
            fields: None,
        };
        let services = HashSet::from(["test2.service".to_string()]);
        parse_one_metric(&mut stats, &metric2, &config, &services)
            .expect("metric2 should parse successfully");
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
        parse_one_metric(&mut stats, &metric1, &config, &HashSet::new())
            .expect("metric1 should parse successfully");
        assert_eq!(stats.service_units, 0);

        // Metric with no fields is handled gracefully
        let metric2 = ListOutput {
            name: "io.systemd.Manager.UnitsByTypeTotal".to_string(),
            value: int_value(42),
            object: None,
            fields: None,
        };
        parse_one_metric(&mut stats, &metric2, &config, &HashSet::new())
            .expect("metric2 should parse successfully");

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
        parse_one_metric(&mut stats, &metric3, &config, &HashSet::new())
            .expect("metric3 should parse successfully");

        // Unhandled metric name is ignored
        let metric4 = ListOutput {
            name: "io.systemd.Manager.UnknownMetric".to_string(),
            value: int_value(999),
            object: Some("test.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric4, &config, &HashSet::new())
            .expect("metric4 should parse successfully");
    }

    #[test]
    fn test_state_stats_disabled_skips_unit_states_only() {
        // When state_stats=false, UnitActiveState / UnitLoadState are skipped so
        // unit_states remains empty. NRestarts follows the [services] list
        // instead (like the D-Bus path) and is unaffected by state_stats.
        let config = crate::config::UnitsConfig {
            enabled: true,
            state_stats: false,
            state_stats_allowlist: HashSet::new(),
            state_stats_blocklist: HashSet::new(),
            state_stats_time_in_state: true,
            ignore_inactive_oneshot_services: true,
            unit_files: true,
            ..Default::default()
        };
        let services = HashSet::from(["test.service".to_string()]);
        let mut stats = SystemdUnitStats::default();

        let active_state_metric = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("active"),
            object: Some("test.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &active_state_metric, &config, &services)
            .expect("active_state_metric should parse successfully");

        let load_state_metric = ListOutput {
            name: "io.systemd.Manager.UnitLoadState".to_string(),
            value: string_value("loaded"),
            object: Some("test.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &load_state_metric, &config, &services)
            .expect("load_state_metric should parse successfully");

        let nrestarts_metric = ListOutput {
            name: "io.systemd.Manager.NRestarts".to_string(),
            value: int_value(3),
            object: Some("test.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &nrestarts_metric, &config, &services)
            .expect("nrestarts_metric should parse successfully");

        // Per-unit state data must be absent when state_stats=false, but the
        // [services]-listed unit still gets its restart count.
        assert_eq!(stats.unit_states.len(), 0);
        assert_eq!(
            stats
                .service_stats
                .get("test.service")
                .expect("test.service should have a service_stats entry")
                .nrestarts,
            3
        );

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
        parse_one_metric(&mut stats, &type_metric, &config, &HashSet::new())
            .expect("type_metric should parse successfully");
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
        parse_one_metric(&mut stats, &metric1, &config, &HashSet::new())
            .expect("metric1 should parse successfully");
        assert_eq!(
            stats
                .unit_states
                .get("test.service")
                .expect("test.service should have a unit_states entry")
                .active_state,
            SystemdUnitActiveState::inactive
        );

        // Update to active state
        let metric2 = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("active"),
            object: Some("test.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric2, &config, &HashSet::new())
            .expect("metric2 should parse successfully");
        assert_eq!(
            stats
                .unit_states
                .get("test.service")
                .expect("test.service should have a unit_states entry")
                .active_state,
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
        parse_one_metric(&mut stats, &metric1, &config, &HashSet::new())
            .expect("metric1 should parse successfully");

        // Set load state to loaded
        let metric2 = ListOutput {
            name: "io.systemd.Manager.UnitLoadState".to_string(),
            value: string_value("loaded"),
            object: Some("broken.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric2, &config, &HashSet::new())
            .expect("metric2 should parse successfully");

        // Should be unhealthy: loaded + failed
        assert!(
            stats
                .unit_states
                .get("broken.service")
                .expect("broken.service should have a unit_states entry")
                .unhealthy
        );

        // Set active state to active
        let metric3 = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("active"),
            object: Some("healthy.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric3, &config, &HashSet::new())
            .expect("metric3 should parse successfully");

        // Set load state to loaded
        let metric4 = ListOutput {
            name: "io.systemd.Manager.UnitLoadState".to_string(),
            value: string_value("loaded"),
            object: Some("healthy.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric4, &config, &HashSet::new())
            .expect("metric4 should parse successfully");

        // Should be healthy: loaded + active
        assert!(
            !stats
                .unit_states
                .get("healthy.service")
                .expect("healthy.service should have a unit_states entry")
                .unhealthy
        );
    }

    #[test]
    fn test_oneshot_inactive_service_is_candidate() {
        let mut stats = SystemdUnitStats::default();
        let config = default_units_config();
        let metrics = vec![
            ListOutput {
                name: "io.systemd.Manager.UnitActiveState".to_string(),
                value: string_value("inactive"),
                object: Some("done.service".to_string()),
                fields: None,
            },
            ListOutput {
                name: "io.systemd.Manager.UnitLoadState".to_string(),
                value: string_value("loaded"),
                object: Some("done.service".to_string()),
                fields: None,
            },
        ];

        for metric in &metrics {
            parse_one_metric(&mut stats, metric, &config, &HashSet::new())
                .expect("metric should parse successfully");
        }

        assert_eq!(
            select_oneshot_candidates(&stats, &config),
            vec!["done.service".to_string()]
        );
    }

    #[test]
    fn test_oneshot_candidate_selection_skips_non_candidates() {
        let mut stats = SystemdUnitStats::default();
        let config = default_units_config();
        // Active service: healthy already, no type lookup needed.
        stats.unit_states.insert(
            "running.service".to_string(),
            crate::units::UnitStates {
                active_state: SystemdUnitActiveState::active,
                load_state: SystemdUnitLoadState::loaded,
                unhealthy: false,
                time_in_state_usecs: None,
            },
        );
        // Non-service unit: service type does not apply.
        stats.unit_states.insert(
            "waiting.timer".to_string(),
            crate::units::UnitStates {
                active_state: SystemdUnitActiveState::inactive,
                load_state: SystemdUnitLoadState::loaded,
                unhealthy: true,
                time_in_state_usecs: None,
            },
        );
        // Masked service: never unhealthy, no type lookup needed.
        stats.unit_states.insert(
            "masked.service".to_string(),
            crate::units::UnitStates {
                active_state: SystemdUnitActiveState::inactive,
                load_state: SystemdUnitLoadState::masked,
                unhealthy: false,
                time_in_state_usecs: None,
            },
        );
        // The only real candidate.
        stats.unit_states.insert(
            "done.service".to_string(),
            crate::units::UnitStates {
                active_state: SystemdUnitActiveState::inactive,
                load_state: SystemdUnitLoadState::loaded,
                unhealthy: true,
                time_in_state_usecs: None,
            },
        );

        assert_eq!(
            select_oneshot_candidates(&stats, &config),
            vec!["done.service".to_string()]
        );
    }

    #[test]
    fn test_oneshot_override_marks_oneshot_healthy() {
        let mut stats = SystemdUnitStats::default();
        let config = default_units_config();
        stats.unit_states.insert(
            "done.service".to_string(),
            crate::units::UnitStates {
                active_state: SystemdUnitActiveState::inactive,
                load_state: SystemdUnitLoadState::loaded,
                unhealthy: true,
                time_in_state_usecs: None,
            },
        );
        stats.unit_states.insert(
            "simple.service".to_string(),
            crate::units::UnitStates {
                active_state: SystemdUnitActiveState::inactive,
                load_state: SystemdUnitLoadState::loaded,
                unhealthy: true,
                time_in_state_usecs: None,
            },
        );
        // failed.service has no type entry (lookup failed): stays unhealthy.
        stats.unit_states.insert(
            "failed.service".to_string(),
            crate::units::UnitStates {
                active_state: SystemdUnitActiveState::inactive,
                load_state: SystemdUnitLoadState::loaded,
                unhealthy: true,
                time_in_state_usecs: None,
            },
        );
        let types = std::collections::HashMap::from([
            ("done.service".to_string(), true),
            ("simple.service".to_string(), false),
        ]);

        apply_oneshot_types(&mut stats, &types, &config);

        assert!(
            !stats
                .unit_states
                .get("done.service")
                .expect("done.service should have a unit_states entry")
                .unhealthy
        );
        assert!(
            stats
                .unit_states
                .get("simple.service")
                .expect("simple.service should have a unit_states entry")
                .unhealthy
        );
        assert!(
            stats
                .unit_states
                .get("failed.service")
                .expect("failed.service should have a unit_states entry")
                .unhealthy
        );
    }

    #[test]
    fn test_oneshot_lookup_timings_accumulate() {
        // Lookup accounting adds to (never overwrites) the parse-phase values
        // already recorded by parse_metrics.
        let mut timings = UnitsCollectionTimings {
            per_unit_loop_ms: 10.0,
            service_dbus_fetches: 2,
            ..Default::default()
        };

        record_oneshot_lookup_timings(&mut timings, 5.0, 3);

        assert_eq!(timings.per_unit_loop_ms, 15.0);
        assert_eq!(timings.service_dbus_fetches, 5);
        // Untouched counters stay zero.
        assert_eq!(timings.state_dbus_fetches, 0);
        assert_eq!(timings.timer_dbus_fetches, 0);
    }

    #[test]
    fn test_oneshot_override_can_be_disabled() {
        let mut stats = SystemdUnitStats::default();
        let mut config = default_units_config();
        config.ignore_inactive_oneshot_services = false;
        stats.unit_states.insert(
            "done.service".to_string(),
            crate::units::UnitStates {
                active_state: SystemdUnitActiveState::inactive,
                load_state: SystemdUnitLoadState::loaded,
                unhealthy: true,
                time_in_state_usecs: None,
            },
        );

        assert!(select_oneshot_candidates(&stats, &config).is_empty());

        let types = std::collections::HashMap::from([("done.service".to_string(), true)]);
        apply_oneshot_types(&mut stats, &types, &config);

        assert!(
            stats
                .unit_states
                .get("done.service")
                .expect("done.service should have a unit_states entry")
                .unhealthy
        );
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
            ignore_inactive_oneshot_services: true,
            unit_files: true,
            ..Default::default()
        };

        // Allowed unit should be tracked
        let metric1 = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("active"),
            object: Some("allowed.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric1, &config, &HashSet::new())
            .expect("metric1 should parse successfully");
        assert!(stats.unit_states.contains_key("allowed.service"));

        // Non-allowed unit should be skipped
        let metric2 = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("active"),
            object: Some("not-allowed.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric2, &config, &HashSet::new())
            .expect("metric2 should parse successfully");
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
            ignore_inactive_oneshot_services: true,
            unit_files: true,
            ..Default::default()
        };

        // Blocked unit should be skipped
        let metric1 = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("active"),
            object: Some("blocked.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric1, &config, &HashSet::new())
            .expect("metric1 should parse successfully");
        assert!(!stats.unit_states.contains_key("blocked.service"));

        // Non-blocked unit should be tracked
        let metric2 = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("active"),
            object: Some("ok.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric2, &config, &HashSet::new())
            .expect("metric2 should parse successfully");
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
            ignore_inactive_oneshot_services: true,
            unit_files: true,
            ..Default::default()
        };

        // Unit in both lists should be blocked (blocklist takes priority)
        let metric = ListOutput {
            name: "io.systemd.Manager.UnitActiveState".to_string(),
            value: string_value("active"),
            object: Some("both.service".to_string()),
            fields: None,
        };
        parse_one_metric(&mut stats, &metric, &config, &HashSet::new())
            .expect("metric should parse successfully");
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
            ignore_inactive_oneshot_services: true,
            unit_files: true,
            ..Default::default()
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
            parse_one_metric(&mut stats, &m, &config, &HashSet::new())
                .expect("m should parse successfully");
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
            ignore_inactive_oneshot_services: true,
            unit_files: true,
            ..Default::default()
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
            parse_one_metric(&mut stats, &m, &config, &HashSet::new())
                .expect("m should parse successfully");
        }

        assert_eq!(stats.loaded_units, 1);
        assert_eq!(stats.not_found_units, 1);
        // No per-unit state tracking when state_stats=false
        assert_eq!(stats.unit_states.len(), 0);
    }
}
