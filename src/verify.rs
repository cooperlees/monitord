//! # verify module
//!
//! Collects systemd unit verification errors by running `systemd-analyze verify`
//! on all unit files and parsing the output. Tracks counts of failing units by type.

use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::RwLock;

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
    // Filter out obviously invalid unit names
    if unit_name.len() < 3 {
        return None;
    }

    // Check if it starts with a valid character (alphanumeric, dash, underscore, backslash for escapes)
    let first_char = unit_name.chars().next()?;
    if !first_char.is_alphanumeric() && first_char != '-' && first_char != '\\' {
        return None;
    }

    unit_name.rsplit('.').next().map(|s| s.to_string())
}

/// Parse systemd-analyze verify output to extract failing unit names
/// Output format examples:
/// - "Unit foo.service not found."
/// - "/path/to/foo.service:5: Unknown key..."
/// - "foo.service: Command ... failed..."
fn parse_verify_output(stderr: &str) -> HashSet<String> {
    let mut failing_units = HashSet::new();

    for line in stderr.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Skip "Failed to prepare filename" lines - these are input errors, not unit errors
        if trimmed.contains("Failed to prepare filename") {
            continue;
        }

        let mut found_in_line = false;

        // Format 1: "/path/file.service:line: message" - extract just the filename
        if line.starts_with('/') {
            if let Some(pos) = line.find(':') {
                let path_part = &line[..pos];
                if let Some(filename) = path_part.rsplit('/').next() {
                    if filename.contains('.') && get_unit_type(filename).is_some() {
                        failing_units.insert(filename.to_string());
                        found_in_line = true;
                    }
                }
            }
        }

        // Format 2: "Unit foo.service ..." or "foo.service: ..." - only if not already found from path
        if !found_in_line {
            for word in line.split_whitespace() {
                let cleaned = word.trim_end_matches(':').trim_end_matches('.');
                // Only consider it a unit name if it has a valid extension and looks reasonable
                if cleaned.contains('.')
                    && cleaned.len() > 2 // Minimum reasonable length
                    && !cleaned.contains('(') // Skip things like "foo(8)"
                    && get_unit_type(cleaned).is_some()
                {
                    failing_units.insert(cleaned.to_string());
                    break; // Only take first unit name per line
                }
            }
        }
    }

    failing_units
}

/// Collect verification stats for all units in the system
pub async fn get_verify_stats(
    connection: &zbus::Connection,
    allowlist: &HashSet<String>,
    blocklist: &HashSet<String>,
) -> Result<VerifyStats, MonitordVerifyError> {
    let mut stats = VerifyStats::default();

    // Get list of all units from systemd
    let manager_proxy = crate::dbus::zbus_systemd::ManagerProxy::new(connection).await?;
    let all_units = manager_proxy.list_units().await?;

    // Filter units based on allowlist/blocklist
    let units_to_check: Vec<String> = all_units
        .into_iter()
        .map(|unit| unit.0)
        .filter(|unit_name| {
            // Apply allowlist
            if !allowlist.is_empty() && !allowlist.contains(unit_name) {
                return false;
            }
            // Apply blocklist
            if blocklist.contains(unit_name) {
                return false;
            }
            true
        })
        .collect();

    if units_to_check.is_empty() {
        return Ok(stats);
    }

    // Run systemd-analyze verify on all units at once for better performance
    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = Command::new("systemd-analyze");
        cmd.arg("verify");
        for unit_name in &units_to_check {
            cmd.arg(unit_name);
        }
        cmd.output()
    })
    .await
    .map_err(|e| MonitordVerifyError::CommandError(e.to_string()))?
    .map_err(|e| MonitordVerifyError::CommandError(e.to_string()))?;

    // Parse stderr for failing units
    let stderr = String::from_utf8_lossy(&output.stderr);
    let failing_units = parse_verify_output(&stderr);

    // Count failures by type
    for unit_name in failing_units {
        stats.total += 1;

        if let Some(unit_type) = get_unit_type(&unit_name) {
            *stats.by_type.entry(unit_type).or_insert(0) += 1;
        }
    }

    Ok(stats)
}

/// Async wrapper that updates verify stats when passed a locked struct
pub async fn update_verify_stats(
    connection: zbus::Connection,
    locked_machine_stats: Arc<RwLock<MachineStats>>,
    allowlist: HashSet<String>,
    blocklist: HashSet<String>,
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

    #[test]
    fn test_parse_verify_output() {
        let stderr = r#"
/usr/lib/systemd/system/foo.service:4: Unknown section 'Service'. Ignoring.
bar.slice: Command /bin/foo is not executable: No such file or directory
Unit baz.timer not found.
test-with-error.target: Some error message here
"#;
        let failing = parse_verify_output(stderr);
        // Debug output
        let mut sorted: Vec<_> = failing.iter().collect();
        sorted.sort();
        for unit in &sorted {
            eprintln!("Found unit: {}", unit);
        }

        assert!(failing.contains("foo.service"));
        assert!(failing.contains("bar.slice"));
        assert!(failing.contains("baz.timer"));
        assert!(failing.contains("test-with-error.target"));
        assert_eq!(failing.len(), 4);
    }
}
