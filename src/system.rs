//! # system module
//!
//! Handle systemd's overall "system" state. Basically says if we've successfully
//! booted, stated all units or have been asked to stop, be offline etc.

use std::convert::TryInto;
use std::fmt;
use std::time::Duration;

use anyhow::Context;
use dbus::blocking::Connection;
use int_enum::IntEnum;
use serde_repr::Deserialize_repr;
use serde_repr::Serialize_repr;
use strum_macros::EnumIter;
use strum_macros::EnumString;
use thiserror::Error;
use tracing::error;

#[derive(Error, Debug)]
pub enum MonitordSystemError {
    #[error("monitord::system failed: {0:#}")]
    GenericError(#[from] anyhow::Error),
    #[error("Unable to connect to DBUS: {0:#}")]
    DbusError(#[from] dbus::Error),
}

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
    #[default]
    unknown = 0,
    initializing = 1,
    starting = 2,
    running = 3,
    degraded = 4,
    maintenance = 5,
    stopping = 6,
    offline = 7,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq)]
pub struct SystemdVersion {
    major: u32,
    minor: String,
    revision: Option<u32>,
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
        let mut parts = s.split('.');
        let split_count = parts.clone().count();
        let major = parts
            .next()
            .with_context(|| "No valid major version")?
            .parse::<u32>()
            .with_context(|| format!("Failed to parse major version: {:?}", s))?;
        let minor = parts
            .next()
            .with_context(|| "No valid minor version")?
            .parse::<String>()
            .with_context(|| format!("Failed to parse minor version: {:?}", s))?;
        let mut revision = None;
        if split_count > 3 {
            revision = parts.next().and_then(|s| s.parse::<u32>().ok());
        }
        let remaining_elements: Vec<&str> = parts.collect();
        let os = remaining_elements.join(".").to_string();
        Ok(SystemdVersion {
            major,
            minor,
            revision,
            os,
        })
    }
}

pub fn get_system_state(dbus_address: &str) -> Result<SystemdSystemState, MonitordSystemError> {
    std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", dbus_address);
    let c = Connection::new_system()?;
    let p = c.with_proxy(
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        Duration::new(5, 0),
    );
    use crate::dbus::systemd::OrgFreedesktopSystemd1Manager;
    let state = match p.system_state() {
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

pub fn get_version(dbus_address: &str) -> Result<SystemdVersion, MonitordSystemError> {
    std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", dbus_address);
    let c = Connection::new_system()?;
    let p = c.with_proxy(
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        Duration::new(5, 0),
    );
    use crate::dbus::systemd::OrgFreedesktopSystemd1Manager;
    let version_string = p
        .version()
        .with_context(|| "Unable to get systemd version string".to_string())?;
    version_string.try_into()
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

        // #bigCompany string
        let parsed: SystemdVersion = "969.6-9.9.hs+fb.el9".to_string().try_into()?;
        assert_eq!(
            SystemdVersion::new(969, String::from("6-9"), Some(9), String::from("hs+fb.el9")),
            parsed
        );

        Ok(())
    }
}
