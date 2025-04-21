//! # timers module
//!
//! All timer related logic goes here. This will be hitting timer specific
//! dbus / varlink etc.

use std::sync::Arc;

use anyhow::Result;
use struct_field_names_as_array::FieldNamesAsArray;
use tracing::error;

use crate::units::SystemdUnitStats;

#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, FieldNamesAsArray, PartialEq,
)]

/// Struct with all the timer specific statistics
pub struct TimerStats {
    pub accuracy_usec: u64,
    pub fixed_random_delay: bool,
    pub last_trigger_usec: u64,
    pub last_trigger_usec_monotonic: u64,
    pub next_elapse_usec_monotonic: u64,
    pub next_elapse_usec_realtime: u64,
    pub persistent: bool,
    pub randomized_delay_usec: u64,
    pub remain_after_elapse: bool,
    pub service_unit_last_state_change_usec: u64,
    pub service_unit_last_state_change_usec_monotonic: u64,
}

pub const TIMER_STATS_FIELD_NAMES: &[&str] = &TimerStats::FIELD_NAMES_AS_ARRAY;

pub async fn collect_timer_stats(
    connection: &zbus::Connection,
    stats: &mut SystemdUnitStats,
    unit: &crate::units::ListedUnit,
) -> Result<TimerStats> {
    let mut timer_stats = TimerStats::default();

    let pt = Arc::new(
        crate::dbus::zbus_timer::TimerProxy::builder(connection)
            .path(unit.unit_object_path.clone())?
            .build()
            .await?,
    );
    // Get service unit name to check when it last ran to ensure
    // we are triggers the configured service with times set
    let service_unit = pt.unit().await?;
    let mut service_unit_last_state_change_usec: Result<u64, zbus::Error> = Ok(0);
    let mut service_unit_last_state_change_usec_monotonic: Result<u64, zbus::Error> = Ok(0);
    if service_unit.is_empty() {
        error!("{}: No service unit name found for timer.", unit.name);
    } else {
        // Get the object path of the service unit
        let mp = crate::dbus::zbus_systemd::ManagerProxy::new(connection).await?;
        let service_unit_path = mp.get_unit(&service_unit).await?;
        // Create a UnitProxy with the unit path to async get the two counters we want
        let up = crate::dbus::zbus_unit::UnitProxy::builder(connection)
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

    // Grab all the other DBUS data async
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
        tokio::spawn({
            let spawn_pt = pt.clone();
            async move { spawn_pt.accuracy_usec().await }
        }),
        tokio::spawn({
            let spawn_pt = pt.clone();
            async move { spawn_pt.fixed_random_delay().await }
        }),
        tokio::spawn({
            let spawn_pt = pt.clone();
            async move { spawn_pt.last_trigger_usec().await }
        }),
        tokio::spawn({
            let spawn_pt = pt.clone();
            async move { spawn_pt.last_trigger_usec_monotonic().await }
        }),
        tokio::spawn({
            let spawn_pt = pt.clone();
            async move { spawn_pt.persistent().await }
        }),
        tokio::spawn({
            let spawn_pt = pt.clone();
            async move { spawn_pt.next_elapse_usec_monotonic().await }
        }),
        tokio::spawn({
            let spawn_pt = pt.clone();
            async move { spawn_pt.next_elapse_usec_realtime().await }
        }),
        tokio::spawn({
            let spawn_pt = pt.clone();
            async move { spawn_pt.randomized_delay_usec().await }
        }),
        tokio::spawn({
            let spawn_pt = pt.clone();
            async move { spawn_pt.remain_after_elapse().await }
        }),
    );

    timer_stats.accuracy_usec = accuracy_usec??;
    timer_stats.fixed_random_delay = fixed_random_delay??;
    timer_stats.last_trigger_usec = last_trigger_usec??;
    timer_stats.last_trigger_usec_monotonic = last_trigger_usec_monotonic??;
    timer_stats.persistent = persistent??;
    timer_stats.next_elapse_usec_monotonic = next_elapse_usec_monotonic??;
    timer_stats.next_elapse_usec_realtime = next_elapse_usec_realtime??;
    timer_stats.randomized_delay_usec = randomized_delay_usec??;
    timer_stats.remain_after_elapse = remain_after_elapse??;

    if timer_stats.persistent {
        stats.timer_persistent_units += 1;
    }

    if timer_stats.remain_after_elapse {
        stats.timer_remain_after_elapse += 1;
    }

    Ok(timer_stats)
}
