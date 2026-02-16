//! # system module
//!
//! Handle systemd's overall "system" state. Basically says if we've successfully
//! booted, stated all units or have been asked to stop, be offline etc.

use std::convert::TryInto;
use std::fmt;
use std::sync::Arc;

use anyhow::anyhow;
use anyhow::Context;
use int_enum::IntEnum;
use serde_repr::Deserialize_repr;
use serde_repr::Serialize_repr;
use strum_macros::EnumIter;
use strum_macros::EnumString;
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::error;

use crate::MachineStats;

#[derive(Error, Debug)]
pub enum MonitordSystemError {
    #[error("monitord::system failed: {0:#}")]
    GenericError(#[from] anyhow::Error),
    #[error("Unable to connect to DBUS via zbus: {0:#}")]
    ZbusError(#[from] zbus::Error),
}

/// Overall system state reported by the systemd manager (PID 1).
/// Reflects whether the system has fully booted, is shutting down, or has failures.
/// Queried via the SystemState property on org.freedesktop.systemd1.Manager.
#[allow(non_camel_case_types)]
#[derive(
    Serialize_repr,
    Deserialize_repr,
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    PartialEq,
    EnumIter,
    EnumString,
    IntEnum,
    strum_macros::Display,
)]
#[repr(u8)]
pub enum SystemdSystemState {
    /// System state could not be determined
    #[default]
    unknown = 0,
    /// systemd is loading and setting up its internal state early in the boot process
    initializing = 1,
    /// systemd is starting units as part of the boot sequence
    starting = 2,
    /// All units have been started successfully and the system is fully operational
    running = 3,
    /// System is operational but one or more units have failed
    degraded = 4,
    /// System is in rescue or emergency mode (single-user maintenance)
    maintenance = 5,
    /// System is shutting down
    stopping = 6,
    /// systemd is not running (seen on non-booted containers or during very early boot)
    offline = 7,
}

/// Parsed systemd version from the Version property on org.freedesktop.systemd1.Manager.
/// Format: "major.minor[.revision].os" (e.g. "256.1.fc40", "255.6-9.9.hs+fb.el9")
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq)]
pub struct SystemdVersion {
    /// Major version number (e.g. 256)
    major: u32,
    /// Minor version string; may contain hyphens for distro-patched versions (e.g. "6-9")
    minor: String,
    /// Optional patch/revision number, present when the version string has 4+ dot-separated parts
    revision: Option<u32>,
    /// OS/distribution suffix (e.g. "fc40", "hs+fb.el9")
    os: String,
}
impl SystemdVersion {
    pub fn new(major: u32, minor: String, revision: Option<u32>, os: String) -> SystemdVersion {
        Self {
            major,
            minor,
            revision,
            os,
        }
    }
}
impl fmt::Display for SystemdVersion {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if let Some(revision) = self.revision {
            return write!(f, "{}.{}.{}.{}", self.major, self.minor, revision, self.os);
        }
        write!(f, "{}.{}.{}", self.major, self.minor, self.os)
    }
}
impl TryFrom<String> for SystemdVersion {
    type Error = MonitordSystemError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        let no_v_version = s.strip_prefix('v').unwrap_or(&s);
        let mut parts = no_v_version.split('.');
        let split_count = parts.clone().count();
        let major = parts
            .next()
            .with_context(|| "No valid major version")?
            .parse::<u32>()
            .with_context(|| format!("Failed to parse major version: {:?}", s))?;
        let minor = parts
            .next()
            .with_context(|| "No valid minor version")?
            .to_string();
        let mut revision = None;
        if split_count > 3 {
            revision = parts.next().and_then(|s| s.parse::<u32>().ok());
        }
        let os = parts.collect::<Vec<&str>>().join(".");
        Ok(SystemdVersion {
            major,
            minor,
            revision,
            os,
        })
    }
}

//pub fn get_system_state(dbus_address: &str) -> Result<SystemdSystemState, dbus::Error> {
pub async fn get_system_state(
    connection: &zbus::Connection,
) -> Result<SystemdSystemState, MonitordSystemError> {
    let p = crate::dbus::zbus_systemd::ManagerProxy::new(connection)
        .await
        .map_err(MonitordSystemError::ZbusError)?;

    let state = match p.system_state().await {
        Ok(system_state) => match system_state.as_str() {
            "initializing" => crate::system::SystemdSystemState::initializing,
            "starting" => crate::system::SystemdSystemState::starting,
            "running" => crate::system::SystemdSystemState::running,
            "degraded" => crate::system::SystemdSystemState::degraded,
            "maintenance" => crate::system::SystemdSystemState::maintenance,
            "stopping" => crate::system::SystemdSystemState::stopping,
            "offline" => crate::system::SystemdSystemState::offline,
            _ => crate::system::SystemdSystemState::unknown,
        },
        Err(err) => {
            error!("Failed to get system-state: {:?}", err);
            crate::system::SystemdSystemState::unknown
        }
    };
    Ok(state)
}

/// Async wrapper than can update system stats when passed a locked struct
pub async fn update_system_stats(
    connection: zbus::Connection,
    locked_machine_stats: Arc<RwLock<MachineStats>>,
) -> anyhow::Result<()> {
    let mut machine_stats = locked_machine_stats.write().await;
    machine_stats.system_state = crate::system::get_system_state(&connection)
        .await
        .map_err(|e| anyhow!("Error getting system state: {:?}", e))?;
    Ok(())
}

pub async fn get_version(
    connection: &zbus::Connection,
) -> Result<SystemdVersion, MonitordSystemError> {
    let p = crate::dbus::zbus_systemd::ManagerProxy::new(connection)
        .await
        .map_err(MonitordSystemError::ZbusError)?;
    let version_string = p
        .version()
        .await
        .with_context(|| "Unable to get systemd version string".to_string())?;
    version_string.try_into()
}

/// Async wrapper than can update system stats when passed a locked struct
pub async fn update_version(
    connection: zbus::Connection,
    locked_machine_stats: Arc<RwLock<MachineStats>>,
) -> anyhow::Result<()> {
    let mut machine_stats = locked_machine_stats.write().await;
    machine_stats.version = crate::system::get_version(&connection)
        .await
        .map_err(|e| anyhow!("Error getting systemd version: {:?}", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    #[test]
    fn test_display_struct() {
        assert_eq!(
            format!("{}", SystemdSystemState::running),
            String::from("running"),
        )
    }

    #[test]
    fn test_parsing_systemd_versions() -> Result<()> {
        let parsed: SystemdVersion = "969.1.69.fc69".to_string().try_into()?;
        assert_eq!(
            SystemdVersion::new(969, String::from("1"), Some(69), String::from("fc69")),
            parsed
        );

        // No revision
        let parsed: SystemdVersion = "969.1.fc69".to_string().try_into()?;
        assert_eq!(
            SystemdVersion::new(969, String::from("1"), None, String::from("fc69")),
            parsed
        );

        // #bigCompany strings
        let parsed: SystemdVersion = String::from("969.6-9.9.hs+fb.el9").try_into()?;
        assert_eq!(
            SystemdVersion::new(969, String::from("6-9"), Some(9), String::from("hs+fb.el9")),
            parsed
        );

        let parsed: SystemdVersion = String::from("v299.6-9.9.hs+fb.el9").try_into()?;
        assert_eq!(
            SystemdVersion::new(299, String::from("6-9"), Some(9), String::from("hs+fb.el9")),
            parsed
        );

        Ok(())
    }
}
