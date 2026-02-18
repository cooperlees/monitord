//! # verify module
//!
//! Collects systemd unit verification errors by running `systemd-analyze verify`
//! on all unit files and parsing the output. Tracks counts of failing units by type.

use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::RwLock;
use tracing::warn;

use crate::MachineStats;

#[derive(Error, Debug)]
pub enum MonitordVerifyError {
    #[error("Failed to execute systemd-analyze: {0}")]
    CommandError(String),
    #[error("Unable to connect to D-Bus via zbus: {0:#}")]
    ZbusError(#[from] zbus::Error),
}

/// Statistics about unit verification errors, aggregated by unit type (service, slice, timer, etc.)
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq)]
pub struct VerifyStats {
    /// Total count of units with verification failures
    pub total: u64,
    /// Count of failing units per type (e.g., "service", "slice", "timer")
    /// Only includes types that have at least one failure
    #[serde(flatten)]
    pub by_type: HashMap<String, u64>,
}

/// Extract unit type from a unit name (e.g., "foo.service" -> "service")
fn get_unit_type(unit_name: &str) -> Option<String> {
    unit_name.rsplit('.').next().map(|s| s.to_string())
}

/// Run systemd-analyze verify on a single unit and parse output for errors
async fn verify_unit(unit_name: &str) -> Result<bool, MonitordVerifyError> {
    let unit_name = unit_name.to_string(); // Clone the string to avoid lifetime issues
    let output = tokio::task::spawn_blocking(move || {
        Command::new("systemd-analyze")
            .arg("verify")
            .arg(&unit_name)
            .output()
    })
    .await
    .map_err(|e| MonitordVerifyError::CommandError(e.to_string()))?
    .map_err(|e| MonitordVerifyError::CommandError(e.to_string()))?;

    // systemd-analyze verify returns non-zero exit code on errors
    // Errors are written to stderr
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Only count as error if there's actual error content (not just warnings)
        if !stderr.is_empty() {
            return Ok(true); // Has errors
        }
    }
    
    Ok(false) // No errors
}

/// Collect verification stats for all units in the system
pub async fn get_verify_stats(
    connection: &zbus::Connection,
    allowlist: &std::collections::HashSet<String>,
    blocklist: &std::collections::HashSet<String>,
) -> Result<VerifyStats, MonitordVerifyError> {
    let mut stats = VerifyStats::default();

    // Get list of all units from systemd
    let manager_proxy = crate::dbus::zbus_systemd::ManagerProxy::new(connection).await?;
    let units = manager_proxy.list_units().await?;

    for unit in units {
        let unit_name = unit.0;

        // Apply allowlist/blocklist filtering
        if !allowlist.is_empty() && !allowlist.contains(&unit_name) {
            continue;
        }
        if blocklist.contains(&unit_name) {
            continue;
        }

        // Run verify check
        match verify_unit(&unit_name).await {
            Ok(has_errors) => {
                if has_errors {
                    stats.total += 1;
                    
                    // Extract unit type and increment counter
                    if let Some(unit_type) = get_unit_type(&unit_name) {
                        *stats.by_type.entry(unit_type).or_insert(0) += 1;
                    }
                }
            }
            Err(e) => {
                warn!("Failed to verify unit {}: {}", unit_name, e);
            }
        }
    }

    Ok(stats)
}

/// Async wrapper that updates verify stats when passed a locked struct
pub async fn update_verify_stats(
    connection: zbus::Connection,
    locked_machine_stats: Arc<RwLock<MachineStats>>,
    allowlist: std::collections::HashSet<String>,
    blocklist: std::collections::HashSet<String>,
) -> anyhow::Result<()> {
    let verify_stats = get_verify_stats(&connection, &allowlist, &blocklist)
        .await
        .map_err(|e| anyhow::anyhow!("Error getting verify stats: {:?}", e))?;

    let mut machine_stats = locked_machine_stats.write().await;
    machine_stats.verify_stats = Some(verify_stats);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_unit_type() {
        assert_eq!(get_unit_type("foo.service"), Some("service".to_string()));
        assert_eq!(get_unit_type("bar.slice"), Some("slice".to_string()));
        assert_eq!(get_unit_type("baz.timer"), Some("timer".to_string()));
        assert_eq!(get_unit_type("test"), Some("test".to_string()));
    }

    #[test]
    fn test_verify_stats_default() {
        let stats = VerifyStats::default();
        assert_eq!(stats.total, 0);
        assert_eq!(stats.by_type.len(), 0);
    }
}
