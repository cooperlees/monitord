//! # units module
//!
//! All main systemd unit statistics. Counts of types of units, unit states and
//! queued jobs. We also house service specific statistics and system unit states.

use std::str::FromStr;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::debug;

use tracing::warn;

use crate::pid1::Pid1Stats;
use crate::system::SystemdSystemState;
use crate::unit_constants::{is_unit_unhealthy, SystemdUnitActiveState, SystemdUnitLoadState};
use crate::units::SystemdUnitStats;
use crate::varlink::metrics::{ListOutput, Metrics};
use crate::MachineStats;
use futures_util::stream::TryStreamExt;
use zlink::unix;

pub const METRICS_SOCKET_PATH: &str = "/run/systemd/report/io.systemd.Manager";

/// Result of a varlink collection indicating which data was provided
#[derive(Debug, Default)]
pub struct VarlinkCollectionResult {
    pub has_pid1: bool,
    pub has_system_state: bool,
}

/// Parse an unsigned integer value from a metric, warning on failure
fn parse_metric_u64(metric: &ListOutput) -> Option<u64> {
    if !metric.value().is_i64() && !metric.value().is_u64() {
        warn!(
            "Metric {} has non-integer value: {:?}",
            metric.name(),
            metric.value()
        );
        return None;
    }
    // Try u64 first (for values > i64::MAX), then fall back to i64
    if let Some(v) = metric.value().as_u64() {
        return Some(v);
    }
    let value = metric.value_as_int();
    match value.try_into() {
        Ok(v) => Some(v),
        Err(_) => {
            warn!("Metric {} has negative value: {}", metric.name(), value);
            None
        }
    }
}

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
    match T::from_str(value_str) {
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
            if should_skip_unit(&object_name, config) {
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
            if should_skip_unit(&object_name, config) {
                return Ok(());
            }
            let load_state: SystemdUnitLoadState = match parse_metric_enum(metric) {
                Some(v) => v,
                None => return Ok(()),
            };
            let unit_state = stats
                .unit_states
                .entry(object_name.to_string())
                .or_default();
            unit_state.load_state = load_state;
            unit_state.unhealthy =
                is_unit_unhealthy(unit_state.active_state, unit_state.load_state);
        }
        "NRestarts" => {
            if should_skip_unit(&object_name, config) {
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
        // Global unit count metrics
        "JobsQueued" => {
            if let Some(value) = parse_metric_u64(metric) {
                stats.jobs_queued = value;
            }
        }
        "UnitsTotal" => {
            if let Some(value) = parse_metric_u64(metric) {
                stats.total_units = value;
            }
        }
        "UnitsByTypeTotal" => {
            if let Some(type_str) = metric.get_field_as_str("type") {
                if let Some(value) = parse_metric_u64(metric) {
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
        }
        "UnitsByStateTotal" => {
            if let Some(state_str) = metric.get_field_as_str("state") {
                if let Some(value) = parse_metric_u64(metric) {
                    match state_str {
                        "active" => stats.active_units = value,
                        "failed" => stats.failed_units = value,
                        "inactive" => stats.inactive_units = value,
                        _ => debug!("Found unhandled unit state: {:?}", state_str),
                    }
                }
            }
        }
        "UnitsByLoadStateTotal" => {
            if let Some(load_state_str) = metric.get_field_as_str("load_state") {
                if let Some(value) = parse_metric_u64(metric) {
                    match load_state_str {
                        "loaded" => stats.loaded_units = value,
                        "masked" => stats.masked_units = value,
                        "not-found" => stats.not_found_units = value,
                        "error" => debug!("Received error load state count: {}", value),
                        _ => debug!("Found unhandled load state: {:?}", load_state_str),
                    }
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
    pid1: &mut Option<Pid1Stats>,
    system_state: &mut Option<SystemdSystemState>,
    socket_path: &str,
    config: &crate::config::UnitsConfig,
) -> anyhow::Result<()> {
    let metrics = collect_metrics(socket_path.to_string()).await?;

    let mut pid1_stats = Pid1Stats::default();
    let mut has_pid1 = false;

    for metric in &metrics {
        let suffix = metric.name_suffix();
        match suffix {
            // PID1 metrics (convert from microseconds to seconds to match procfs path)
            "Pid1CpuTimeKernelUSec" => {
                if let Some(value) = parse_metric_u64(metric) {
                    pid1_stats.cpu_time_kernel = value / 1_000_000;
                    has_pid1 = true;
                }
            }
            "Pid1CpuTimeUserUSec" => {
                if let Some(value) = parse_metric_u64(metric) {
                    pid1_stats.cpu_time_user = value / 1_000_000;
                    has_pid1 = true;
                }
            }
            "Pid1FdCount" => {
                if let Some(value) = parse_metric_u64(metric) {
                    pid1_stats.fd_count = value;
                    has_pid1 = true;
                }
            }
            "Pid1MemoryUsageBytes" => {
                if let Some(value) = parse_metric_u64(metric) {
                    pid1_stats.memory_usage_bytes = value;
                    has_pid1 = true;
                }
            }
            "Pid1Tasks" => {
                if let Some(value) = parse_metric_u64(metric) {
                    pid1_stats.tasks = value;
                    has_pid1 = true;
                }
            }
            // SystemState metric
            "SystemState" => {
                if let Some(state) = parse_metric_enum::<SystemdSystemState>(metric) {
                    *system_state = Some(state);
                }
            }
            // All other metrics handled by parse_one_metric
            _ => {
                parse_one_metric(stats, metric, config)?;
            }
        }
    }

    if has_pid1 {
        *pid1 = Some(pid1_stats);
    }

    Ok(())
}

pub async fn get_manager_stats(
    config: &crate::config::Config,
    socket_path: &str,
) -> anyhow::Result<(
    SystemdUnitStats,
    Option<Pid1Stats>,
    Option<SystemdSystemState>,
)> {
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
    let mut pid1: Option<Pid1Stats> = None;
    let mut system_state: Option<SystemdSystemState> = None;

    // Collect all metrics from the varlink socket
    parse_metrics(
        &mut stats,
        &mut pid1,
        &mut system_state,
        socket_path,
        &config.units,
    )
    .await?;

    debug!("unit stats: {:?}", stats);
    Ok((stats, pid1, system_state))
}

/// Async wrapper that can update stats when passed a locked struct.
/// Returns what was collected so the caller can skip redundant D-Bus collectors.
pub async fn update_unit_stats(
    config: Arc<crate::config::Config>,
    locked_machine_stats: Arc<RwLock<MachineStats>>,
    socket_path: String,
) -> anyhow::Result<VarlinkCollectionResult> {
    let (units_stats, pid1, system_state) = get_manager_stats(&config, &socket_path).await?;
    let mut machine_stats = locked_machine_stats.write().await;
    machine_stats.units = units_stats;

    let mut result = VarlinkCollectionResult::default();
    if let Some(pid1_stats) = pid1 {
        machine_stats.pid1 = Some(pid1_stats);
        result.has_pid1 = true;
    }
    if let Some(state) = system_state {
        machine_stats.system_state = state;
        result.has_system_state = true;
    }
    Ok(result)
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

        let metric = ListOutput {
            name: "io.systemd.Manager.UnitLoadState".to_string(),
            value: string_value("not_found"), // Enum variant name uses underscore
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
            5
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

    #[tokio::test]
    async fn test_get_manager_stats_unavailable_socket() {
        let config = crate::config::Config {
            units: crate::config::UnitsConfig {
                enabled: true,
                state_stats: false,
                state_stats_allowlist: HashSet::new(),
                state_stats_blocklist: HashSet::new(),
                state_stats_time_in_state: true,
            },
            ..Default::default()
        };

        // When the varlink socket is not available, get_manager_stats returns an error
        let result = get_manager_stats(&config, "/nonexistent/socket").await;
        assert!(result.is_err());
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
    async fn test_parse_global_unit_metrics() {
        let mut stats = SystemdUnitStats::default();
        let config = default_units_config();

        let metrics = vec![
            ListOutput {
                name: "io.systemd.Manager.JobsQueued".to_string(),
                value: int_value(5),
                object: None,
                fields: None,
            },
            ListOutput {
                name: "io.systemd.Manager.UnitsTotal".to_string(),
                value: int_value(250),
                object: None,
                fields: None,
            },
        ];

        for metric in metrics {
            parse_one_metric(&mut stats, &metric, &config).unwrap();
        }

        assert_eq!(stats.jobs_queued, 5);
        assert_eq!(stats.total_units, 250);
    }

    #[tokio::test]
    async fn test_parse_units_by_load_state_total() {
        let mut stats = SystemdUnitStats::default();
        let config = default_units_config();

        let metrics = vec![
            ListOutput {
                name: "io.systemd.Manager.UnitsByLoadStateTotal".to_string(),
                value: int_value(200),
                object: None,
                fields: Some(std::collections::HashMap::from([(
                    "load_state".to_string(),
                    serde_json::json!("loaded"),
                )])),
            },
            ListOutput {
                name: "io.systemd.Manager.UnitsByLoadStateTotal".to_string(),
                value: int_value(3),
                object: None,
                fields: Some(std::collections::HashMap::from([(
                    "load_state".to_string(),
                    serde_json::json!("masked"),
                )])),
            },
            ListOutput {
                name: "io.systemd.Manager.UnitsByLoadStateTotal".to_string(),
                value: int_value(7),
                object: None,
                fields: Some(std::collections::HashMap::from([(
                    "load_state".to_string(),
                    serde_json::json!("not-found"),
                )])),
            },
        ];

        for metric in metrics {
            parse_one_metric(&mut stats, &metric, &config).unwrap();
        }

        assert_eq!(stats.loaded_units, 200);
        assert_eq!(stats.masked_units, 3);
        assert_eq!(stats.not_found_units, 7);
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
}
