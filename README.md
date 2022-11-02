# monitord

monitord ... know how happy your systemd is! 😊

We offer the following run modes:

- systemd-timer (legacy cron would work too)
  - Refer to monitord.timer and monitord.service unit files
  - Ensure no `daemon:` mode options are set in `monitord.conf`
- daemon mode
  - Enable daemon mode in configuration file
  - Stats will be written to stdout every `daemon_stats_refresh_secs`
- [promethus-exporter](https://prometheus.io/docs/instrumenting/exporters/) daemon
  - Recommended to use monitord.service + set a non 0 prometheus_exporter in `monitord.conf`

Open to more formats / run methods ... Open a PR.

`monitord` is a config driven binary. We plan to keep CLI arguments to a minimum.

**INFO** level logging is enabled to stderr by default. Use `-v` to increase (DEBUG) and `-q` to reduce logging.

## Install

Install via cargo or use as a dependency in your `Cargo.toml`.

- `cargo install monitord`
- Create (copy from repo) a `monitord.conf`
  - Defaults to looking for it at /etc/monitord.conf
- `monitord --help`

```console
crl-m1:monitord cooper$ monitord --help
monitord 0.6.9
monitord: Know how happy your systemd is! 😊

USAGE:
    monitord [OPTIONS]

OPTIONS:
    -c, --config <CONFIG>
            Location of your monitord config

            [default: /etc/monitord.conf]

    -h, --help
            Print help information

    -q, --quiet
            Less output per occurrence

    -v, --verbose
            More output per occurrence

    -V, --version
            Print version information
```


### Config

monitord can have the different components monitored. To enable / disabled set the 
following in our monitord.conf. This file is [ini format](https://en.wikipedia.org/wiki/INI_file)
to match systemd unit files.

```ini
[monitord]
debug=false
# onetime run (cron/systemd timer) output format
# Supported: json, json-flat, json-pretty
output_format=json
# Run as a daemon or now
# Time to refresh systemd stats in seconds
# Daemon mode only
daemon_stats_refresh_secs=60

[journald]
enabled=no

[networkd]
enabled=true
link_state_dir=/run/systemd/netif/links

[units]
enabled=no

# Daemon Mode options

[daemon:prometheus_exporter]
# We recommend TCP port 1 due to this monitoring PID 1 :)
# Set a port >= 1 to listen
port = 1
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
  "networkd.wg0.required_for_online": 1
}
```

### json-pretty

Normal `serde_json` pretty representations of each componoents structs.

## Development

To do test runs (requires `systemd` and `systemd-networkd` *installed*)
- Pending what you have enabled in your config ...

- `cargo run -- -c monitord.conf -v`

Ensure the following pass before submitting a PR (CI checks):

- `cargo test`
- `cargo clippy`
- `cargo fmt`
