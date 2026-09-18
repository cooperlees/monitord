//! # timers module
//!
//! All timer related logic goes here. This will be hitting timer specific
//! dbus / varlink etc.

use struct_field_names_as_array::FieldNamesAsArray;
use thiserror::Error;
use tracing::error;

#[derive(Error, Debug)]
pub enum MonitordTimerError {
    #[error("Timer D-Bus error: {0}")]
    ZbusError(#[from] zbus::Error),
}

#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, FieldNamesAsArray, PartialEq,
)]

/// Per-timer unit metrics from the org.freedesktop.systemd1.Timer D-Bus interface.
/// Ref: <https://www.freedesktop.org/software/systemd/man/org.freedesktop.systemd1.html>
pub struct TimerStats {
    /// AccuracySec timer property in microseconds; systemd may coalesce timer firings within this window to save wakeups
    pub accuracy_usec: u64,
    /// Whether FixedRandomDelay= is set; when true, the random delay is stable across reboots for this timer
    pub fixed_random_delay: bool,
    /// Realtime timestamp (usec since epoch) when this timer last triggered its service unit
    pub last_trigger_usec: u64,
    /// Monotonic timestamp (usec since boot) when this timer last triggered its service unit
    pub last_trigger_usec_monotonic: u64,
    /// Monotonic timestamp (usec since boot) when this timer will next elapse
    pub next_elapse_usec_monotonic: u64,
    /// Realtime timestamp (usec since epoch) when this timer will next elapse
    pub next_elapse_usec_realtime: u64,
    /// Whether Persistent= is set; when true, missed timer runs (e.g. during downtime) are triggered on next boot
    pub persistent: bool,
    /// RandomizedDelaySec property in microseconds; a random delay up to this value is added before each trigger
    pub randomized_delay_usec: u64,
    /// Whether RemainAfterElapse= is set; when true, the timer stays loaded after all triggers have elapsed
    pub remain_after_elapse: bool,
    /// Realtime timestamp (usec since epoch) of the most recent state change of the triggered service unit
    pub service_unit_last_state_change_usec: u64,
    /// Monotonic timestamp (usec since boot) of the most recent state change of the triggered service unit
    pub service_unit_last_state_change_usec_monotonic: u64,
}

pub const TIMER_STATS_FIELD_NAMES: &[&str] = &TimerStats::FIELD_NAMES_AS_ARRAY;

#[tracing::instrument(level = "debug", skip(connection))]
pub async fn collect_timer_stats(
    connection: &zbus::Connection,
    unit: &crate::units::ListedUnit,
) -> Result<TimerStats, MonitordTimerError> {
    let mut timer_stats = TimerStats::default();

    let pt = crate::dbus::zbus_timer::TimerProxy::builder(connection)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .path(unit.unit_object_path.clone())?
        .build()
        .await?;
    // Get service unit name to check when it last ran to ensure
    // we are triggers the configured service with times set
    let service_unit = pt.unit().await?;
    let mut service_unit_last_state_change_usec: Result<u64, zbus::Error> = Ok(0);
    let mut service_unit_last_state_change_usec_monotonic: Result<u64, zbus::Error> = Ok(0);
    if service_unit.is_empty() {
        error!("{}: No service unit name found for timer.", unit.name);
    } else {
        // Get the object path of the service unit
        let mp = crate::dbus::zbus_systemd::ManagerProxy::builder(connection)
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await?;
        let service_unit_path = mp.get_unit(&service_unit).await?;
        // Create a UnitProxy with the unit path to async get the two counters we want
        let up = crate::dbus::zbus_unit::UnitProxy::builder(connection)
            .cache_properties(zbus::proxy::CacheProperties::No)
            .path(service_unit_path)?
            .build()
            .await?;

        (
            service_unit_last_state_change_usec,
            service_unit_last_state_change_usec_monotonic,
        ) = tokio::join!(
            up.state_change_timestamp(),
            up.state_change_timestamp_monotonic(),
        );
    }
    timer_stats.service_unit_last_state_change_usec = service_unit_last_state_change_usec?;
    timer_stats.service_unit_last_state_change_usec_monotonic =
        service_unit_last_state_change_usec_monotonic?;

    // Use tokio::join! without tokio::spawn to avoid per-task allocation overhead.
    // These all share the same D-Bus connection so spawn adds no parallelism benefit.
    let (
        accuracy_usec,
        fixed_random_delay,
        last_trigger_usec,
        last_trigger_usec_monotonic,
        persistent,
        next_elapse_usec_monotonic,
        next_elapse_usec_realtime,
        randomized_delay_usec,
        remain_after_elapse,
    ) = tokio::join!(
        pt.accuracy_usec(),
        pt.fixed_random_delay(),
        pt.last_trigger_usec(),
        pt.last_trigger_usec_monotonic(),
        pt.persistent(),
        pt.next_elapse_usec_monotonic(),
        pt.next_elapse_usec_realtime(),
        pt.randomized_delay_usec(),
        pt.remain_after_elapse(),
    );

    timer_stats.accuracy_usec = accuracy_usec?;
    timer_stats.fixed_random_delay = fixed_random_delay?;
    timer_stats.last_trigger_usec = last_trigger_usec?;
    timer_stats.last_trigger_usec_monotonic = last_trigger_usec_monotonic?;
    timer_stats.persistent = persistent?;
    timer_stats.next_elapse_usec_monotonic = next_elapse_usec_monotonic?;
    timer_stats.next_elapse_usec_realtime = next_elapse_usec_realtime?;
    timer_stats.randomized_delay_usec = randomized_delay_usec?;
    timer_stats.remain_after_elapse = remain_after_elapse?;

    Ok(timer_stats)
}

/// Merge D-Bus-collected timer stats into varlink-collected unit stats.
///
/// The varlink metrics API does not expose timer properties, so both the host
/// (lib.rs) and container (machines.rs) varlink paths backfill them via
/// `collect_all_timers_dbus` and merge the result here.
///
/// `elapsed_ms` is the wall time of that backfill. It is accounted the same way
/// the D-Bus path accounts its own timer work — duration into `per_unit_loop_ms`
/// and one `timer_dbus_fetches` per timer resolved — so the inner timings stay
/// comparable between the two paths instead of under-reporting the varlink one.
pub fn merge_timer_stats(
    target: &mut crate::units::SystemdUnitStats,
    collected: crate::units::SystemdUnitStats,
    elapsed_ms: f64,
) {
    record_backfill_duration(target, elapsed_ms);
    target.collection_timings.timer_dbus_fetches += collected.timer_stats.len() as u64;
    target.timer_stats = collected.timer_stats;
    target.timer_persistent_units = collected.timer_persistent_units;
    target.timer_remain_after_elapse = collected.timer_remain_after_elapse;
}

/// Account a timer backfill that produced no stats.
///
/// A failed backfill still spent D-Bus time — a `dbus_timeout` worth of it, if
/// that is how it failed — so the duration is recorded even though there is
/// nothing to merge and no fetch to count. Leaving it out would hide the
/// slowest case from `per_unit_loop_ms`.
pub fn record_backfill_duration(target: &mut crate::units::SystemdUnitStats, elapsed_ms: f64) {
    target.collection_timings.per_unit_loop_ms += elapsed_ms;
}

/// Collect all timer stats via D-Bus and return them ready to merge into unit stats.
///
/// Used when unit stats were collected via varlink (which doesn't yet expose timer
/// properties) so that `timers.*`, `timer_persistent_units`, and
/// `timer_remain_after_elapse` match the D-Bus output.
pub async fn collect_all_timers_dbus(
    connection: &zbus::Connection,
    config: &crate::config::Config,
) -> anyhow::Result<crate::units::SystemdUnitStats> {
    use std::collections::HashMap;
    use tracing::debug;

    if !config.timers.enabled {
        return Ok(crate::units::SystemdUnitStats::default());
    }

    let p = crate::dbus::zbus_systemd::ManagerProxy::builder(connection)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await?;
    let units = p.list_units().await?;

    let mut stats = crate::units::SystemdUnitStats::default();
    let mut timer_stats_map = HashMap::new();

    for unit_raw in units {
        let unit: crate::units::ListedUnit = unit_raw.into();
        if !unit.name.contains(".timer") {
            continue;
        }
        if config.timers.blocklist.contains(&unit.name) {
            debug!("Skipping timer stats for {} due to blocklist", &unit.name);
            continue;
        }
        if !config.timers.allowlist.is_empty() && !config.timers.allowlist.contains(&unit.name) {
            continue;
        }
        match collect_timer_stats(connection, &unit).await {
            Ok(ts) => {
                if ts.persistent {
                    stats.timer_persistent_units += 1;
                }
                if ts.remain_after_elapse {
                    stats.timer_remain_after_elapse += 1;
                }
                timer_stats_map.insert(unit.name.clone(), ts);
            }
            Err(err) => {
                error!("Failed to get {} stats: {:#?}", &unit.name, err);
            }
        }
    }

    stats.timer_stats = timer_stats_map;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merge_timer_stats() {
        let mut target = crate::units::SystemdUnitStats::default();
        let mut collected = crate::units::SystemdUnitStats::default();
        collected.timer_stats.insert(
            "foo.timer".to_string(),
            TimerStats {
                persistent: true,
                ..Default::default()
            },
        );
        collected.timer_persistent_units = 1;
        collected.timer_remain_after_elapse = 2;

        merge_timer_stats(&mut target, collected, 12.5);

        assert_eq!(target.timer_stats.len(), 1);
        assert!(
            target
                .timer_stats
                .get("foo.timer")
                .expect("foo.timer should be merged")
                .persistent
        );
        assert_eq!(target.timer_persistent_units, 1);
        assert_eq!(target.timer_remain_after_elapse, 2);
        // Untouched aggregates stay zero.
        assert_eq!(target.total_units, 0);
    }

    #[test]
    fn test_merge_timer_stats_accounts_the_backfill() {
        // The varlink path has already recorded its own parse loop, so the
        // backfill adds to per_unit_loop_ms rather than replacing it.
        let mut target = crate::units::SystemdUnitStats::default();
        target.collection_timings.per_unit_loop_ms = 2.5;
        let mut collected = crate::units::SystemdUnitStats::default();
        for name in ["foo.timer", "bar.timer"] {
            collected
                .timer_stats
                .insert(name.to_string(), TimerStats::default());
        }

        merge_timer_stats(&mut target, collected, 10.0);

        assert_eq!(target.collection_timings.per_unit_loop_ms, 12.5);
        assert_eq!(target.collection_timings.timer_dbus_fetches, 2);
    }

    #[test]
    fn test_record_backfill_duration_without_stats() {
        // A backfill that failed — a dbus_timeout worth of wall time is the
        // case worth seeing, so it is accounted with no fetch counted.
        let mut target = crate::units::SystemdUnitStats::default();
        target.collection_timings.per_unit_loop_ms = 1.5;

        record_backfill_duration(&mut target, 5000.0);

        assert_eq!(target.collection_timings.per_unit_loop_ms, 5001.5);
        assert_eq!(target.collection_timings.timer_dbus_fetches, 0);
        assert!(target.timer_stats.is_empty());
    }

    #[test]
    fn test_merge_timer_stats_with_no_timers() {
        // An empty allowlist result still costs a ListUnits call, so the time
        // is accounted even though no timer was fetched.
        let mut target = crate::units::SystemdUnitStats::default();

        merge_timer_stats(&mut target, crate::units::SystemdUnitStats::default(), 3.0);

        assert_eq!(target.collection_timings.per_unit_loop_ms, 3.0);
        assert_eq!(target.collection_timings.timer_dbus_fetches, 0);
    }
}
