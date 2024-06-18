//! # system module
//!
//! Handle systemd's overall "system" state. Basically says if we've successfully
//! booted, stated all units or have been asked to stop, be offline etc.

use std::fmt;
use std::time::Duration;

use dbus::blocking::Connection;
use int_enum::IntEnum;
use serde_repr::Deserialize_repr;
use serde_repr::Serialize_repr;
use strum_macros::EnumString;
use tracing::error;

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
impl From<String> for SystemdVersion {
    fn from(s: String) -> Self {
        let mut parts = s.split('.');
        let split_count = parts.clone().count();
        let major = parts
            .next()
            .unwrap_or("0")
            .parse::<u32>()
            .expect("Major version element isn't a valid u32");
        let minor = parts
            .next()
            .unwrap_or("")
            .parse::<String>()
            .expect("Minor isn't a valid String");
        let mut revision = None;
        if split_count > 3 {
            revision = parts.next().and_then(|s| s.parse::<u32>().ok());
        }
        let remaining_elements: Vec<&str> = parts.collect();
        let os = remaining_elements.join(".").to_string();
        SystemdVersion {
            major,
            minor,
            revision,
            os,
        }
    }
}

pub fn get_system_state(dbus_address: &str) -> Result<SystemdSystemState, dbus::Error> {
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

pub fn get_version(dbus_address: &str) -> Result<SystemdVersion, dbus::Error> {
    std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", dbus_address);
    let c = Connection::new_system()?;
    let p = c.with_proxy(
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        Duration::new(5, 0),
    );
    use crate::dbus::systemd::OrgFreedesktopSystemd1Manager;
    let version_string = p.version()?;
    Ok(version_string.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_struct() {
        assert_eq!(
            format!("{}", SystemdSystemState::running),
            String::from("running"),
        )
    }

    #[test]
    fn test_parsing_systemd_versions() {
        let parsed: SystemdVersion = "969.1.69.fc69".to_string().into();
        assert_eq!(
            SystemdVersion::new(969, String::from("1"), Some(69), String::from("fc69")),
            parsed
        );

        // No revision
        let parsed: SystemdVersion = "969.1.fc69".to_string().into();
        assert_eq!(
            SystemdVersion::new(969, String::from("1"), None, String::from("fc69")),
            parsed
        );

        // #bigCompany string
        let parsed: SystemdVersion = "969.6-9.9.hs+fb.el9".to_string().into();
        assert_eq!(
            SystemdVersion::new(969, String::from("6-9"), Some(9), String::from("hs+fb.el9")),
            parsed
        );
    }
}
