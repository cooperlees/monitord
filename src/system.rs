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
    strum_macros::ToString,
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
