//! # timers module
//!
//! All timer related logic goes here. This will be hitting timer specific
//! dbus / varlink etc.

use anyhow::Result;
use struct_field_names_as_array::FieldNamesAsArray;
use tracing::error;
use zbus::zvariant::OwnedObjectPath;

use crate::units::SystemdUnitStats;

#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, FieldNamesAsArray, PartialEq,
)]

/// Struct with all the timer specific statistics
pub struct TimerStats {
    pub accruacy_usec: u64,
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
    unit: &(
        String,          // The primary unit name as string
        String,          // The human readable description string
        String,          // The load state (i.e. whether the unit file has been loaded successfully)
        String,          // The active state (i.e. whether the unit is currently started or not)
        String,          // The sub state (i.e. unit type more specific state)
        String, // A unit that is being followed in its state by this unit, if there is any, otherwise the empty string
        OwnedObjectPath, // The unit object path
        u32,    // If there is a job queued for the job unit, the numeric job id, 0 otherwise
        String, // The job type as string
        OwnedObjectPath, // The job object path
    ),
) -> Result<TimerStats> {
    let pt = crate::dbus::zbus_timer::TimerProxy::builder(connection)
        .path(unit.6.clone())?
        .build()
        .await?;
    let mut timer_stats = TimerStats::default();

    // Get service unit name to check when it last ran to ensure
    // we are triggers the configured service with times set
    let service_unit = pt.unit().await?;
    if service_unit.is_empty() {
        error!("{}: No service unit name found for timer", unit.0);
        return Ok(timer_stats);
    }

    // Get the object path of the service unit
    let mp = crate::dbus::zbus_systemd::ManagerProxy::new(connection).await?;
    let service_unit_path = mp.get_unit(&service_unit).await?;

    let up = crate::dbus::zbus_unit::UnitProxy::builder(connection)
        .path(service_unit_path)?
        .build()
        .await?;

    let persistent_bool = pt.persistent().await?;
    if persistent_bool {
        stats.timer_persistent_units += 1;
    }
    timer_stats.persistent = persistent_bool;

    let remain_after_elapse = pt.remain_after_elapse().await?;
    if remain_after_elapse {
        stats.timer_remain_after_elapse += 1;
    }
    timer_stats.remain_after_elapse = remain_after_elapse;

    // Add all non counted stats
    timer_stats.accruacy_usec = pt.accuracy_usec().await?;
    timer_stats.fixed_random_delay = pt.fixed_random_delay().await?;
    timer_stats.last_trigger_usec = pt.last_trigger_usec().await?;
    timer_stats.last_trigger_usec_monotonic = pt.last_trigger_usec_monotonic().await?;
    timer_stats.next_elapse_usec_monotonic = pt.next_elapse_usec_monotonic().await?;
    timer_stats.next_elapse_usec_realtime = pt.next_elapse_usec_realtime().await?;
    timer_stats.randomized_delay_usec = pt.randomized_delay_usec().await?;
    timer_stats.service_unit_last_state_change_usec = up.state_change_timestamp().await?;
    timer_stats.service_unit_last_state_change_usec_monotonic =
        up.state_change_timestamp_monotonic().await?;

    Ok(timer_stats)
}
