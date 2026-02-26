# monitord

[![Crates.io](https://img.shields.io/crates/v/monitord)](https://crates.io/crates/monitord)
[![Documentation](https://img.shields.io/badge/docs-monitord.xyz-blue)](https://monitord.xyz/monitord/index.html)
[![License: GPL-2.0-or-later](https://img.shields.io/crates/l/monitord)](https://github.com/cooperlees/monitord/blob/main/LICENSE)

monitord ... know how happy your systemd is! 😊

## Requirements

- **Linux** with **systemd** (monitord uses D-Bus and procfs APIs that are Linux-specific)
- systemd-networkd installed (for networkd metrics; the collector can be disabled in config)
- PID 1 stats require procfs (`/proc`) — available on all standard Linux systems
- D-Bus system bus accessible (default: `unix:path=/run/dbus/system_bus_socket`)
- Varlink metrics require systemd v260+ (optional; falls back to D-Bus automatically)

## What does monitord monitor?

monitord collects systemd health metrics via D-Bus (and optionally Varlink) and outputs them as JSON. It provides visibility into:

- **Unit counts** — totals by type (service, mount, socket, timer, etc.) and state (active, failed, inactive, loaded, masked)
- **Per-service stats** — CPU usage, memory, I/O, restart count, task count, watchdog status, and state timestamps for specific services
- **Unit state tracking** — active state, load state, and health for individual units (with allowlist/blocklist filtering)
- **systemd-networkd** — per-interface operational, carrier, admin, and address states
- **PID 1 health** — CPU time, memory usage, file descriptor count, and task count for systemd (PID 1) via procfs
- **Timers** — trigger times, accuracy, delays, and associated service state for systemd timers
- **Boot blame** — the N slowest units at boot, similar to `systemd-analyze blame`
- **D-Bus daemon stats** — connection counts, match rules, and per-peer/per-cgroup/per-user breakdowns (dbus-broker and dbus-daemon)
- **Containers / machines** — recursively collects the same metrics from systemd-nspawn containers and VMs via `systemd-machined`
- **Unit verification** — runs `systemd-analyze verify` and reports failing unit counts by type

## Run Modes

We offer the following run modes:

- systemd-timer (legacy cron would work too)
  - Refer to [monitord.timer](monitord.timer) and [monitord.service](monitord.service) unit files
  - Ensure no `daemon:` mode options are set in `monitord.conf`
- daemon mode
  - Enable daemon mode in configuration file
  - Stats will be written to stdout every `daemon_stats_refresh_secs`

Open to more formats / run methods ... Open an issue to discuss. Depends on the dependencies basically.

`monitord` is a config driven binary. We plan to keep CLI arguments to a minimum.

**INFO** level logging is enabled to stderr by default. Use `-l LEVEL` to increase or decrease logging.

## Quick Start

1. Install monitord:
   ```bash
   cargo install monitord
   ```

2. Create a minimal config at `/etc/monitord.conf`:
   ```ini
   [monitord]
   output_format = json-pretty

   [units]
   enabled = true

   [pid1]
   enabled = true
   ```

3. Run it:
   ```bash
   monitord
   ```

This will collect unit counts and PID 1 stats, then print JSON to stdout and exit. Enable additional collectors in the config as needed (see [Configuration](#config) below).

## Install

### Pre-built binaries

Download pre-built binaries from [GitHub Releases](https://github.com/cooperlees/monitord/releases):

- `monitord-linux-amd64` — x86_64
- `monitord-linux-aarch64` — ARM64

```bash
# Example: download and install the latest release (x86_64)
curl -L -o /usr/local/bin/monitord \
  https://github.com/cooperlees/monitord/releases/latest/download/monitord-linux-amd64
chmod +x /usr/local/bin/monitord
```

### From crates.io

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
# Timeout in seconds for dbus connection/collections
# OPTIONAL: default is 30 seconds
dbus_timeout = 30
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

# Grab as much stats from DBus GetStats call
# we can from running dbus daemon
# More tested on dbus-broker daemon
[dbus]
# Summary counters - both dbus-broker + dbus-daemon
enabled = false
# dbus.user.* metrics: user stats as reported by dbus-broker
user_stats = false
# dbus.oeer.* metrics: peer stats as reported by dbus-broker
peer_stats = false
# dbus.cgroup.* stats is an aggregation of peer_stats by cgroup
# by dbus-broker
cgroup_stats = false

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

# Boot blame metrics - shows the N slowest units at boot
# Similar to `systemd-analyze blame`
# Disabled by default
[boot]
enabled = false
# Number of slowest units to report
num_slowest_units = 5

# Optional: only include specific units in boot blame (if empty, all units are checked)
# Same rules apply as state_stats lists above
[boot.allowlist]
# slow-startup.service

# Optional: exclude specific units from boot blame
[boot.blocklist]
# noisy-but-expected.service

# Unit verification using systemd-analyze verify
# Disabled by default as it can be slow on large systems
[verify]
enabled = false

# Optional: only verify specific units (if empty, all units are checked)
[verify.allowlist]
# example.service
# example.timer

# Optional: skip verification for specific units
[verify.blocklist]
# noisy.service
# broken.timer
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
  "boot.blame.dnf5-automatic.service": 204.159,
  "boot.blame.cpe_chef.service": 103.05,
  "boot.blame.sys-module-fuse.device": 16.21,
  "boot.blame.dev-ttyS0.device": 15.809,
  "boot.blame.systemd-networkd-wait-online.service": 1.674,
  "dbus.active_connections": 10,
  "dbus.bus_names": 16,
  "dbus.incomplete_connections": 0,
  "dbus.match_rules": 26,
  "dbus.peak_bus_names": 33,
  "dbus.peak_bus_names_per_connection": 2,
  "dbus.peak_match_rules": 33,
  "dbus.peak_match_rules_per_connection": 13,
  "dbus.cgroup.system.slice-systemd-logind.service.activation_request_bytes": 0,
  "dbus.cgroup.system.slice-systemd-logind.service.activation_request_fds": 0,
  "dbus.cgroup.system.slice-systemd-logind.service.incoming_bytes": 16,
  "dbus.cgroup.system.slice-systemd-logind.service.incoming_fds": 0,
  "dbus.cgroup.system.slice-systemd-logind.service.match_bytes": 6942,
  "dbus.cgroup.system.slice-systemd-logind.service.matches": 5,
  "dbus.cgroup.system.slice-systemd-logind.service.name_objects": 1,
  "dbus.cgroup.system.slice-systemd-logind.service.outgoing_bytes": 0,
  "dbus.cgroup.system.slice-systemd-logind.service.outgoing_fds": 0,
  "dbus.cgroup.system.slice-systemd-logind.service.reply_objects": 0,
  "dbus.peer.org.freedesktop.systemd1.activation_request_bytes": 0,
  "dbus.peer.org.freedesktop.systemd1.activation_request_fds": 0,
  "dbus.peer.org.freedesktop.systemd1.incoming_bytes": 16,
  "dbus.peer.org.freedesktop.systemd1.incoming_fds": 0,
  "dbus.peer.org.freedesktop.systemd1.match_bytes": 46533,
  "dbus.peer.org.freedesktop.systemd1.matches": 33,
  "dbus.peer.org.freedesktop.systemd1.name_objects": 1,
  "dbus.peer.org.freedesktop.systemd1.outgoing_bytes": 0,
  "dbus.peer.org.freedesktop.systemd1.outgoing_fds": 0,
  "dbus.peer.org.freedesktop.systemd1.reply_objects": 0,
  "dbus.user.cooper.bytes": 919236,
  "dbus.user.cooper.fds": 78,
  "dbus.user.cooper.matches": 510,
  "dbus.user.cooper.objects": 80,
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
  "units.activating_units": 0,
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
  "verify.failing.device": 43,
  "verify.failing.mount": 15,
  "verify.failing.service": 31,
  "verify.failing.slice": 1,
  "verify.failing.total": 97,
  "version": "255.7-1.fc40"
}
```

### json-pretty

Normal `serde_json` pretty representations of each components structs.

### Metric Value Reference

Many metrics are serialized as integers. Here are the enum mappings:

**system-state**

| Value | State |
|-------|-------|
| 0 | unknown |
| 1 | initializing |
| 2 | starting |
| 3 | running |
| 4 | degraded |
| 5 | maintenance |
| 6 | stopping |
| 7 | offline |

**active_state** (unit_states.\*.active_state)

| Value | State |
|-------|-------|
| 0 | unknown |
| 1 | active |
| 2 | reloading |
| 3 | inactive |
| 4 | failed |
| 5 | activating |
| 6 | deactivating |

**loaded_state** (unit_states.\*.loaded_state)

| Value | State |
|-------|-------|
| 0 | unknown |
| 1 | loaded |
| 2 | error |
| 3 | masked |
| 4 | not-found |

**networkd address_state / ipv4_address_state / ipv6_address_state**

| Value | State |
|-------|-------|
| 0 | unknown |
| 1 | off |
| 2 | degraded |
| 3 | routable |

**networkd admin_state**

| Value | State |
|-------|-------|
| 0 | unknown |
| 1 | pending |
| 2 | failed |
| 3 | configuring |
| 4 | configured |
| 5 | unmanaged |
| 6 | linger |

**networkd carrier_state**

| Value | State |
|-------|-------|
| 0 | unknown |
| 1 | off |
| 2 | no-carrier |
| 3 | dormant |
| 4 | degraded-carrier |
| 5 | carrier |
| 6 | enslaved |

**networkd oper_state**

| Value | State |
|-------|-------|
| 0 | unknown |
| 1 | missing |
| 2 | off |
| 3 | no-carrier |
| 4 | dormant |
| 5 | degraded-carrier |
| 6 | carrier |
| 7 | degraded |
| 8 | enslaved |
| 9 | routable |

## dbus stats

You're going to need to be root or allow permissiong to pull dbus stats.
For dbus-broker here is example config allow a user `monitord` to query
`getStats`

```xml
[cooper@l33t ~]# cat /etc/dbus-1/system.d/allow_monitord_stats.conf
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE busconfig PUBLIC
 "-//freedesktop//DTD D-BUS Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <policy user="monitord">
    <allow send_destination="org.freedesktop.DBus"
           send_interface="org.freedesktop.DBus.Debug.Stats"
           send_member="GetStats"
           send_path="/org/freedesktop/DBus"
           send_type="method_call"/>
  </policy>
</busconfig>
```

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

## Troubleshooting

**"Connection refused" or D-Bus connection errors**

Ensure the system D-Bus daemon is running and the socket exists at `/run/dbus/system_bus_socket`. If using a custom address, set `dbus_address` in `[monitord]` config. Increase `dbus_timeout` if running on slow systems.

**Empty or missing networkd metrics**

systemd-networkd must be installed and running (`systemctl start systemd-networkd`). If networkd is not in use on your system, disable the collector with `enabled = false` in `[networkd]`.

**Permission denied for D-Bus stats**

The `[dbus]` collector requires permission to call `org.freedesktop.DBus.Debug.Stats.GetStats`. Either run monitord as root or add a D-Bus policy file — see the [dbus stats](#dbus-stats) section.

**PID 1 stats unavailable**

PID 1 stats require Linux with procfs mounted at `/proc`. This collector is compiled out on non-Linux targets. If `/proc` is not available (some container runtimes), disable with `enabled = false` in `[pid1]`.

**Collector errors don't crash monitord**

When an individual collector fails (e.g., networkd not running, D-Bus timeout), monitord logs a warning and continues with the remaining collectors. Check stderr output or increase the log level (`-l debug`) to see which collectors had issues.

**Large u64 values (18446744073709551615) in output**

These represent `u64::MAX` and mean "not available" or "not tracked" for that metric. This is how systemd reports fields that are unsupported or not configured for the unit (e.g., `memory_available` when `MemoryMax=` is not set).

## Library API

monitord can be used as a Rust library. See the full API documentation at [monitord.xyz](https://monitord.xyz/monitord/index.html).

## DBus

All monitord's dbus is done via async (tokio) [zbus](https://crates.io/crates/zbus) crate.

systemd Dbus APIs are in use in the following modules:

- machines
  - `ManagerProxy::list_machines()`
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
  - `UnitProxy::state_change_timestamp_monotonic()`
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
  - `UnitProxy::state_change_timestamp()` - Used for raw stat + time_in_state

Some of these modules can be disabled via configuration. Due to this, monitord might not
always be running / calling all these DBus calls per run.

## Varlink

monitord supports collecting unit statistics via systemd's [Varlink metrics API](https://github.com/systemd/systemd/pull/39202),
available in systemd v260+. When enabled, monitord connects to the `io.systemd.Metrics` interface
at `/run/systemd/report/io.systemd.Manager` to collect unit counts, active/load states, and restart counts.

### Enabling Varlink

Set `enabled = true` in the `[varlink]` section of `monitord.conf`:

```ini
[varlink]
enabled = true
```

When varlink is enabled, monitord will attempt to collect unit stats via the metrics API first.
If the varlink socket is unavailable (e.g., systemd < v260), it automatically falls back to D-Bus collection.

### Metrics collected via Varlink

- Unit counts by type (service, mount, socket, target, device, automount, timer, path, slice, scope)
- Unit counts by state (active, failed, inactive)
- Per-unit active state and load state (with allowlist/blocklist filtering)
- Per-unit health status (computed from active + load state)
- Per-service restart counts (`nrestarts`)

### Containers

For systemd-nspawn containers, monitord connects to the container's varlink socket via
`/proc/<leader_pid>/root/run/systemd/report/io.systemd.Manager`, similar to how D-Bus uses
the container-scoped bus socket.

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
