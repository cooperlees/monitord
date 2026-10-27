// Proxies are async-only: zbus 6 removed the blocking API entirely, so there
// is no `gen_blocking` knob for zbus-xmlgen to emit anymore.
pub mod zbus_machine;
pub mod zbus_machines;
pub mod zbus_networkd;
pub mod zbus_service;
pub mod zbus_systemd;
pub mod zbus_timer;
pub mod zbus_unit;
