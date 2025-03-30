//! # timers module
//!
//! All timer related logic goes here. This will be hitting timer specific
//! dbus / varlink etc.

use anyhow::Result;
use tracing::error;
use zbus::zvariant::OwnedObjectPath;

use crate::units::SystemdUnitStats;

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
) -> Result<()> {
    let pt = crate::dbus::zbus_timer::TimerProxy::builder(connection)
        .path(unit.6.clone())?
        .build()
        .await?;
    match pt.persistent().await {
        Ok(persistent_bool) => {
            if persistent_bool {
                stats.timer_persistent_units += 1;
            }
        }
        Err(err) => error!(
            "Failed to check if {} is persistent: {:?}",
            &unit.0, err
        ),
    }
    match pt.remain_after_elapse().await {
        Ok(remain_after_elapse) => {
            if remain_after_elapse {
                stats.timer_remain_after_elapse += 1;
            }
        }
        Err(err) => error!(
            "Failed to check if {} remains after elapse: {:?}",
            &unit.0, err
        ),
    }
    Ok(())
}
