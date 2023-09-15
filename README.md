# monitord

monitord ... know how happy your systemd is! 😊

We offer the following run modes:

- systemd-timer (legacy cron would work too)
  - Refer to monitord.timer and monitord.service unit files
  - Ensure no `daemon:` mode options are set in `monitord.conf`
- daemon mode
  - Enable daemon mode in configuration file
  - Stats will be written to stdout every `daemon_stats_refresh_secs`

Open to more formats / run methods ... Open an issue to discuss. Depends on the dependencies basically.

`monitord` is a config driven binary. We plan to keep CLI arguments to a minimum.

**INFO** level logging is enabled to stderr by default. Use `-v` to increase (DEBUG) and `-q` to reduce logging.

## Install

Install via cargo or use as a dependency in your `Cargo.toml`.

- `cargo install monitord`
- Create (copy from repo) a `monitord.conf`
  - Defaults to looking for it at /etc/monitord.conf
- `monitord --help`

```console
crl-linux:monitord cooper$ monitord --help
monitord 0.5.0
Cooper Ry Lees <me@cooperlees.com>
monitord: Know how happy your systemd is! 😊

Usage: monitord [OPTIONS]

Options:
  -c, --config <CONFIG>
          Location of your monitord config

          [default: /etc/monitord.conf]

  -l, --log-level <LOG_LEVEL>
          Adjust the console log-level

          [default: Info]
          [possible values: error, warn, info, debug, trace]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```


### Config

monitord can have the different components monitored. To enable / disabled set the 
following in our monitord.conf. This file is [ini format](https://en.wikipedia.org/wiki/INI_file)
to match systemd unit files.

```ini
# Pure ini - no yes/no for bools

[monitord]
# Set a custom dbus address to connect to
# OPTIONAL: If not set, we default to the Unix socket below
dbus_address = unix:path=/run/dbus/system_bus_socket
# Run as a daemon or 1 time
daemon = false
# Time to refresh systemd stats in seconds
# Daemon mode only
daemon_stats_refresh_secs = 60
# Prefix flat-json key with this value
# The value automatically gets a '.' appended (so don't put here)
key_prefix = monitord
# cron/systemd timer output format
# Supported: json, json-flat, json-pretty
output_format = json

# TODO: implement
[journald]
enabled = false

# Grab networkd stats from files + networkctl
[networkd]
enabled = true
link_state_dir = /run/systemd/netif/links

# Services to grab extra stats for
# .service is important as that's what DBus returns from `list_units`
[services]
foo.service

# Grab unit status counts via dbus
[units]
enabled = true
state_stats = true

# Filter what services you want collect state stats for
# If both lists are configured blocklist is preferred
# If neither exist all units state will generate counters
[units.state_stats.allowed]
foo.service

[units.state_stats.disallowed]
bar.service
```

## Output Formats

### json

Normal `serde_json` non pretty JSON. All on one line. Most compact format.

### json-flat

Move all key value pairs to the top level and . notate compononets + sub values.
Is semi pretty too + custom. All unittested ...

```json
{
  "networkd.eno4.address_state": 3,
  "networkd.eno4.admin_state": 4,
  "networkd.eno4.carrier_state": 5,
  "networkd.eno4.ipv4_address_state": 3,
  "networkd.eno4.ipv6_address_state": 2,
  "networkd.eno4.oper_state": 9,
  "networkd.eno4.required_for_online": 1,
  "networkd.ens2f0.address_state": 1,
  "networkd.ens2f0.admin_state": 3,
  "networkd.ens2f0.carrier_state": 1,
  "networkd.ens2f0.ipv4_address_state": 1,
  "networkd.ens2f0.ipv6_address_state": 1,
  "networkd.ens2f0.oper_state": 2,
  "networkd.ens2f0.required_for_online": 0,
  "networkd.managed_interfaces": 6,
  "networkd.spectrum.address_state": 3,
  "networkd.spectrum.admin_state": 4,
  "networkd.spectrum.carrier_state": 5,
  "networkd.spectrum.ipv4_address_state": 3,
  "networkd.spectrum.ipv6_address_state": 3,
  "networkd.spectrum.oper_state": 9,
  "networkd.spectrum.required_for_online": 1,
  "networkd.vlan69.address_state": 3,
  "networkd.vlan69.admin_state": 4,
  "networkd.vlan69.carrier_state": 5,
  "networkd.vlan69.ipv4_address_state": 3,
  "networkd.vlan69.ipv6_address_state": 3,
  "networkd.vlan69.oper_state": 9,
  "networkd.vlan69.required_for_online": 1,
  "networkd.vlan70.address_state": 3,
  "networkd.vlan70.admin_state": 4,
  "networkd.vlan70.carrier_state": 5,
  "networkd.vlan70.ipv4_address_state": 1,
  "networkd.vlan70.ipv6_address_state": 3,
  "networkd.vlan70.oper_state": 9,
  "networkd.vlan70.required_for_online": 1,
  "networkd.wg0.address_state": 3,
  "networkd.wg0.admin_state": 4,
  "networkd.wg0.carrier_state": 5,
  "networkd.wg0.ipv4_address_state": 3,
  "networkd.wg0.ipv6_address_state": 3,
  "networkd.wg0.oper_state": 9,
  "networkd.wg0.required_for_online": 1,
  "services.chronyd.service.active_enter_timestamp": 1683556542382710,
  "services.chronyd.service.active_exit_timestamp": 0,
  "services.chronyd.service.cpuusage_nsec": 328951000,
  "services.chronyd.service.inactive_exit_timestamp": 1683556541360626,
  "services.chronyd.service.ioread_bytes": 18446744073709551615,
  "services.chronyd.service.ioread_operations": 18446744073709551615,
  "services.chronyd.service.memory_available": 18446744073709551615,
  "services.chronyd.service.memory_current": 5214208,
  "services.chronyd.service.nrestarts": 0,
  "services.chronyd.service.restart_usec": 100000,
  "services.chronyd.service.state_change_timestamp": 1683556542382710,
  "services.chronyd.service.status_errno": 0,
  "services.chronyd.service.tasks_current": 1,
  "services.chronyd.service.timeout_clean_usec": 18446744073709551615,
  "services.chronyd.service.watchdog_usec": 0,
  "monitord.unit_states.chronyd.service.active_state": 1,
  "monitord.unit_states.chronyd.service.loaded_state": 1,
  "units.active_units": 403,
  "units.automount_units": 1,
  "units.device_units": 150,
  "units.failed_units": 0,
  "units.inactive_units": 159,
  "units.jobs_queued": 0,
  "units.loaded_units": 497,
  "units.masked_units": 25,
  "units.mount_units": 52,
  "units.not_found_units": 38,
  "units.path_units": 4,
  "units.scope_units": 17,
  "units.service_units": 199,
  "units.slice_units": 7,
  "units.socket_units": 28,
  "units.target_units": 54,
  "units.timer_units": 20,
  "units.total_units": 562
}
```

### json-pretty

Normal `serde_json` pretty representations of each componoents structs.

## Development

To do test runs (requires `systemd` and `systemd-networkd` *installed*)
- Pending what you have enabled in your config ...

- `cargo run -- -c monitord.conf -l debug`

Ensure the following pass before submitting a PR (CI checks):

- `cargo test`
- `cargo clippy`
- `cargo fmt`

### Generate codegen APIs

- `cargo install dbus-codegen`
- `dbus-codegen-rust -s -g -m None -d org.freedesktop.systemd1 -p /org/freedesktop/systemd1/unit/chronyd_2eservice > unit_dbus.rs`

Then add the following macros to tell clippy to go away:

```rust
#![allow(warnings)]
#![allow(clippy)]
#![allow(unknown_lints)]
```
