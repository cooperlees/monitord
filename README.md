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

INFO level logging is enabled to stderr by default.

## Install

Will work on this, but at the moment it's a manual clone and build from this repository.

### Config

monitord can have the different components monitoted. To enable / disabled set the 
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

Move all key value pairs to the top level and . notate compononet + sub values.
Is semi pretty too + custom. It's all unittested ...

```json
{
  'networkd.eth0.address_state': 3
  'networkd.eth0.admin_state': 4
  'networkd.eth0.carrier_state': 5
  'networkd.eth0.ipv4_address_state': 3
  'networkd.eth0.ipv6_address_state': 2
  'networkd.eth0.oper_state': 9
  'networkd.eth0.required_for_online': 1
  'networkd.managed_interfaces': 1
}
```

### json-pretty

Normal `serde_json` pretty representations of each componoents structs.

## Development

- `cargo run -- --help`
  - `-v` will enable debug logging

```console
crl-m1:monitord cooper$ cargo run -- --help
   Compiling monitord v0.0.1 (/Users/cooper/repos/monitord)
    Finished dev [unoptimized + debuginfo] target(s) in 0.45s
     Running `target/debug/monitord --help`
monitord 0.0.1
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
