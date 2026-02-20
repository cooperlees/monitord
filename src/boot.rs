//! # boot module
//!
//! Collects boot blame metrics showing the slowest units at boot.
//! Similar to `systemd-analyze blame` but stores N slowest units.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;
use tracing::debug;
use zbus::zvariant::ObjectPath;

use crate::config::Config;
use crate::dbus::zbus_systemd::ManagerProxy;
use crate::dbus::zbus_unit::UnitProxy;
use crate::MachineStats;

/// Boot blame statistics: maps unit name to activation time in seconds
pub type BootBlameStats = HashMap<String, f64>;

/// Calculate the activation time for a unit
/// Returns the time in seconds from InactiveExitTimestamp to ActiveEnterTimestamp
async fn get_unit_activation_time(
    connection: &zbus::Connection,
    unit_path: &ObjectPath<'_>,
) -> Result<f64> {
    let unit_proxy = UnitProxy::builder(connection)
        .path(unit_path)?
        .build()
        .await?;

    let inactive_exit = unit_proxy.inactive_exit_timestamp().await?;
    let active_enter = unit_proxy.active_enter_timestamp().await?;

    // If either timestamp is 0, the unit hasn't been activated or the timing is invalid
    if inactive_exit == 0 || active_enter == 0 {
        return Ok(0.0);
    }

    // Calculate activation time in seconds (timestamps are in microseconds)
    let activation_time_usec = active_enter.saturating_sub(inactive_exit);
    let activation_time_sec = activation_time_usec as f64 / 1_000_000.0;

    Ok(activation_time_sec)
}

/// Update boot blame statistics with the N slowest units at boot
pub async fn update_boot_blame_stats(
    config: Arc<Config>,
    connection: zbus::Connection,
    machine_stats: Arc<RwLock<MachineStats>>,
) -> Result<()> {
    debug!("Starting boot blame stats collection");

    let systemd_proxy = ManagerProxy::new(&connection).await?;
    let units = systemd_proxy.list_units().await?;

    let mut unit_times: Vec<(String, f64)> = Vec::new();

    // Collect activation times for all units
    for unit_info in units {
        let unit_name = unit_info.0;
        let unit_path = unit_info.6;

        match get_unit_activation_time(&connection, &unit_path).await {
            Ok(time) if time > 0.0 => {
                unit_times.push((unit_name, time));
            }
            Ok(_) => {
                // Unit has no activation time (0.0), skip it
            }
            Err(e) => {
                debug!("Failed to get activation time for {}: {}", unit_name, e);
            }
        }
    }

    // Sort by activation time in descending order (slowest first)
    unit_times.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Take only the N slowest units
    let num_slowest = config.boot_blame.num_slowest_units as usize;
    unit_times.truncate(num_slowest);

    // Convert to HashMap
    let boot_blame_stats: BootBlameStats = unit_times.into_iter().collect();

    debug!("Collected {} boot blame stats", boot_blame_stats.len());

    // Update machine stats
    let mut stats = machine_stats.write().await;
    stats.boot_blame = Some(boot_blame_stats);

    Ok(())
}
