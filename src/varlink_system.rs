//! # varlink_system module
//!
//! systemd version and overall system state via the `io.systemd.Manager`
//! varlink API on PID 1's socket. Available from systemd v258+.
//!
//! `Describe` returns both values in a single call, so each collector asks for
//! the one it needs rather than streaming the whole `io.systemd.Metrics` list
//! (which also carries `Version`/`SystemState`, but costs ~40x the wall time
//! because it enumerates every unit).

use std::sync::Arc;

use tokio::sync::RwLock;

use crate::system::{parse_system_state, SystemdSystemState, SystemdVersion};
use crate::varlink::manager::{Manager, ManagerRuntime};
use crate::MachineStats;

pub use crate::varlink::manager::MANAGER_SOCKET_PATH;

/// Call `io.systemd.Manager.Describe` and return its runtime section.
///
/// Runs on a blocking thread with a dedicated runtime because the zlink
/// connection is `!Send` and cannot be held across `await` points in a `Send`
/// future.
async fn describe_runtime(socket_path: &str) -> anyhow::Result<ManagerRuntime> {
    let socket_path = socket_path.to_string();
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(async move {
            let mut conn = zlink::unix::connect(&socket_path).await?;
            match conn.describe().await? {
                Ok(output) => output
                    .runtime
                    .ok_or_else(|| anyhow::anyhow!("io.systemd.Manager.Describe has no runtime")),
                Err(e) => Err(anyhow::anyhow!("io.systemd.Manager.Describe error: {}", e)),
            }
        })
    })
    .await?
}

pub async fn get_system_state(socket_path: &str) -> anyhow::Result<SystemdSystemState> {
    let runtime = describe_runtime(socket_path).await?;
    let system_state = runtime
        .system_state
        .ok_or_else(|| anyhow::anyhow!("io.systemd.Manager.Describe has no SystemState"))?;
    Ok(parse_system_state(&system_state))
}

pub async fn get_version(socket_path: &str) -> anyhow::Result<SystemdVersion> {
    let runtime = describe_runtime(socket_path).await?;
    let version = runtime
        .version
        .ok_or_else(|| anyhow::anyhow!("io.systemd.Manager.Describe has no Version"))?;
    Ok(version.try_into()?)
}

/// Async wrapper that can update the system state when passed a locked struct.
///
/// Like the D-Bus equivalents in `system.rs`, the varlink call completes before
/// the write lock is taken so the shared `MachineStats` lock is never held
/// across a round trip.
pub async fn update_system_stats(
    socket_path: &str,
    locked_machine_stats: Arc<RwLock<MachineStats>>,
) -> anyhow::Result<()> {
    let system_state = get_system_state(socket_path).await?;
    let mut machine_stats = locked_machine_stats.write().await;
    machine_stats.system_state = system_state;
    Ok(())
}

/// Async wrapper that can update the systemd version when passed a locked struct.
pub async fn update_version(
    socket_path: &str,
    locked_machine_stats: Arc<RwLock<MachineStats>>,
) -> anyhow::Result<()> {
    let version = get_version(socket_path).await?;
    let mut machine_stats = locked_machine_stats.write().await;
    machine_stats.version = version;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::varlink::manager::DescribeOutput;

    #[test]
    fn test_describe_output_ignores_unconsumed_fields() {
        // Trimmed from a real systemd 262 reply: the full one is ~3KB of
        // manager configuration we have no use for, and every field of it must
        // deserialize away without error.
        let reply = r#"{
            "context": {"ShowStatus": true, "LogTarget": "journal"},
            "runtime": {
                "Version": "262~rc3-1.fc46",
                "Architecture": "arm64",
                "Virtualization": "docker",
                "SystemState": "running",
                "NNames": 295
            }
        }"#;
        let output: DescribeOutput =
            serde_json::from_str(reply).expect("Describe reply should deserialize");
        let runtime = output.runtime.expect("reply should have a runtime section");
        assert_eq!(runtime.version.as_deref(), Some("262~rc3-1.fc46"));
        assert_eq!(runtime.system_state.as_deref(), Some("running"));
    }

    #[test]
    fn test_version_string_parses_like_the_dbus_property() {
        // Both APIs hand back the same string, so the D-Bus parser is reused.
        let version: SystemdVersion = "262~rc3-1.fc46"
            .to_string()
            .try_into()
            .expect("version should parse");
        assert_eq!(
            version,
            SystemdVersion::new(262, "rc3-1".to_string(), None, "fc46".to_string())
        );
    }

    #[test]
    fn test_runtime_without_our_fields() {
        // A systemd too old to report these leaves them absent rather than null.
        let output: DescribeOutput =
            serde_json::from_str(r#"{"runtime": {}}"#).expect("Describe reply should deserialize");
        let runtime = output.runtime.expect("reply should have a runtime section");
        assert!(runtime.version.is_none());
        assert!(runtime.system_state.is_none());
    }
}
