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

**INFO** level logging is enabled to stderr by default. Use `-l LEVEL` to increase or decrease logging.

## Install

Install via cargo or use as a dependency in your `Cargo.toml`.

- `cargo install monitord`
- Create (copy from repo) a `monitord.conf`
  - Defaults to looking for it at /etc/monitord.conf
- `monitord --help`

```console
crl-linux:monitord cooper$ monitord --help
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

# Grab networkd stats from files + networkctl
[networkd]
enabled = true
link_state_dir = /run/systemd/netif/links

# Enable grabbing PID 1 stats via procfs
[pid1]
enabled = true

# Services to grab extra stats for
# .service is important as that's what DBus returns from `list_units`
[services]
foo.service

[timers]
enabled = true

[timers.allowlist]
foo.timer

[timers.blocklist]
bar.timer

# Grab unit status counts via dbus
[units]
enabled = true
state_stats = true

# Filter what services you want collect state stats for
# If both lists are configured blocklist is preferred
# If neither exist all units state will generate counters
[units.state_stats.allowlist]
foo.service

[units.state_stats.blocklist]
bar.service

# machines config
[machines]
enabled = true

# Same rules apply as state_stats lists above
[machines.allowlist]
foo

[machines.blocklist]
bar
```

## Machines support

From version `>=0.11` monitord supports obtaining the same set of key from
systemd 'machines' (i.e. `machinectl --list`).

The keys are the same format as below in `json_flat` output but are prefixed with
the `machine` keyword and machine name. For example:

```json
# $KEY_PREFIX.machine.$MACHINE_NAME
{
  ...
  "monitord.machine.foo.pid1.fd_count": 69,
  ...
}
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
  "networkd.managed_interfaces": 2,
  "networkd.wg0.address_state": 3,
  "networkd.wg0.admin_state": 4,
  "networkd.wg0.carrier_state": 5,
  "networkd.wg0.ipv4_address_state": 3,
  "networkd.wg0.ipv6_address_state": 3,
  "networkd.wg0.oper_state": 9,
  "networkd.wg0.required_for_online": 1,
  "pid1.cpu_time_kernel": 48,
  "pid1.cpu_user_kernel": 41,
  "pid1.fd_count": 245,
  "pid1.memory_usage_bytes": 19165184,
  "pid1.tasks": 1,
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
  "system-state": 3,
  "timers.fstrim.timer.accuracy_usec": 3600000000,
  "timers.fstrim.timer.fixed_random_delay": 0,
  "timers.fstrim.timer.last_trigger_usec": 1743397269608978,
  "timers.fstrim.timer.last_trigger_usec_monotonic": 0,
  "timers.fstrim.timer.next_elapse_usec_monotonic": 0,
  "timers.fstrim.timer.next_elapse_usec_realtime": 1744007133996149,
  "timers.fstrim.timer.persistent": 1,
  "timers.fstrim.timer.randomized_delay_usec": 6000000000,
  "timers.fstrim.timer.remain_after_elapse": 1,
  "timers.fstrim.timer.service_unit_last_state_change_usec": 1743517244700135,
  "timers.fstrim.timer.service_unit_last_state_change_usec_monotonic": 639312703,
  "unit_states.chronyd.service.active_state": 1,
  "unit_states.chronyd.service.loaded_state": 1,
  "unit_states.chronyd.service.unhealthy": 0,
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
  "units.total_units": 562,
  "version": "255.7-1.fc40"
}
```

### json-pretty

Normal `serde_json` pretty representations of each components structs.

## Development

To do test runs (requires `systemd` and `systemd-networkd` _installed_)

- Pending what you have enabled in your config ...

- `cargo run -- -c monitord.conf -l debug`

Ensure the following pass before submitting a PR (CI checks):

- `cargo test`
- `cargo clippy`
- `cargo fmt`

### Generate codegen APIs

- `cargo install zbus_xmlgen`
- `zbus-xmlgen system org.freedesktop.systemd1 /org/freedesktop/systemd1/unit/chronyd_2eservice`

Then add the following macros to tell clippy to go away:

```rust
#![allow(warnings)]
#![allow(clippy)]
```

### Non Linux development

Sometimes I develop from my Mac OS X laptop. So I thought I'd document and
add the way I build a Fedora Rawhide container and mount the local repo to /repo
in the container to run monitord and test.

- Build the image (w/git, rust tools and systemd)
  - `docker build -t monitord-dev .`
- Start via systemd and mount the monitord repo to /repo
  - `docker run --rm --name monitord-dev -it --privileged --tmpfs /run --tmpfs /tmp -v $(pwd):/repo monitord-dev /sbin/init`
    - `--rm` is optional but will remove the container when stopped

You can now log into the container to build + run tests and run the binary now against systemd.

- `docker exec -it monitord-dev bash`
- `cd /repo ; cargo run -- -c monitord`
  - networkd etc. are not running my default but can be started ...
  - `systemctl start systemd-networkd`
    - No interfaces will be managed tho by default in the container ...

## API Usage

## DBus

All monitord's dbus is done via async (tokio) [zbus](https://crates.io/crates/zbus) crate.

systemd Dbus APIs are in use in the following modules:

- machines
  - `MangerProxy::list_machines()`
  - Can do most other calls then on the machine's systemd/dbus
- networkd
  - `ManagerProxy::list_links()`
  - Would love to stop parsing `/run/systemd/netif/links` and replace via varlink API
    - https://github.com/systemd/systemd/issues/36877
- system
  - `ManagerProxy::get_version()`
  - `ManagerProxy::system_state()`
- timer
  - `TimerProxy::unit()` - Find service unit of timer
  - `ManagerProxy::get_unit()`
  - `UnitProxy::state_change_timestamp()`
  - `UnitProxy::state_chage_timestamp_monotonic()`
- units
  - `ManagerProxy::list_units()` - Main counting of unit stats
  - `ServiceProxy::cpuusage_nsec()`
  - `ServiceProxy::ioread_bytes()`
  - `ServiceProxy::ioread_operations()`
  - `ServiceProxy::memory_current()`
  - `ServiceProxy::memory_available()`
  - `ServiceProxy::nrestarts()`
  - `ServiceProxy::get_processes()`
  - `ServiceProxy::restart_usec()`
  - `ServiceProxy::status_errno()`
  - `ServiceProxy::tasks_current()`
  - `ServiceProxy::timeout_clean_usec()`
  - `ServiceProxy::watchdog_usec()`
  - `UnitProxy::active_enter_timestamp`
  - `UnitProxy::active_exit_timestamp`
  - `UnitProxy::inactive_exit_timestamp()`
  - `UnitProxy::state_change_timestamp())` - Used for raw stat + time_in_state

Some of these modules can be disabled via configuration. Due to this, monitord might not
always be running / calling all these DBus calls per run.

## Varlink

None yet :(.

### varlink 101

varlink might one day replace our DBUS usage. Here are some notes on how to work with systemd varlink
as there isn't really documentation outside `man` pages.

#### Checking interfaces

- varlinkctl is your friend - https://man7.org/linux/man-pages/man1/varlinkctl.1.html

Here is an example with networkd's interfaces:

```
varlinkctl info unix:/run/systemd/netif/io.systemd.Network
varlinkctl introspect unix:/run/systemd/netif/io.systemd.Network io.systemd.Network

cooper@au:~$ varlinkctl call unix:/run/systemd/netif/io.systemd.Network io.systemd.Network.GetStates '{}' -j | jq
{
  "AddressState": "routable",
  "IPv4AddressState": "routable",
  "IPv6AddressState": "routable",
  "CarrierState": "carrier",
  "OnlineState": "online",
  "OperationalState": "routable"
}
```
