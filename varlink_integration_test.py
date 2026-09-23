#!/usr/bin/env python3
"""Varlink vs D-Bus parity test for monitord.

Boots a Fedora Rawhide container under systemd, builds monitord inside it, then
runs it twice — once with `[varlink] enabled = true` and once with it false —
and asserts both JSON outputs match.

This is what the `varlink-integration` GitHub Action runs, so a failure there
reproduces locally with:

    ./varlink_integration_test.py

The image is rebuilt on every run (Docker's layer cache makes that a no-op
when the Dockerfile has not changed), and the container is recreated whenever
the image changes, so a local run matches the GitHub Action — which always
builds from scratch — instead of silently reusing a stale image. The container
is otherwise left running between invocations for fast iteration; use --fresh
to rebuild the image without the cache and recreate the container.
"""

import argparse
import json
import subprocess
import re
import sys
import time
from pathlib import Path
from typing import Any, Optional, Sequence

IMAGE = "monitord-dev"
CONTAINER = "monitord-test"
# Cargo target dir inside the container: keeps Linux build artifacts out of the
# repo's target/, which on a developer machine belongs to the host toolchain.
CONTAINER_TARGET_DIR = "/ctarget"
METRICS_SOCKET = "/run/systemd/report/io.systemd.Manager"
NETWORK_SOCKET = "/run/systemd/netif/io.systemd.Network"
MANAGER_SOCKET = "/run/systemd/io.systemd.Manager"
DBUS_CONF = "/tmp/monitord-ci.conf"
VARLINK_CONF = "/tmp/monitord-varlink-ci.conf"

Stats = dict[str, Any]

# nspawn fixture machine booted inside the test container so the machines
# module has a real container to collect from. The name must match the
# [machines.allowlist] entry install_machine_fixture writes (stock
# monitord.conf names units that do not exist here, so the config is
# rewritten the same way [services] already is for the cgroup fixture).
MACHINE_FIXTURE_NAME = "testbox"
# The fixture machine boots a pinned Fedora release, not Rawhide: Rawhide has
# no stable repo snapshot to install from (only the rolling `rawhide` repo),
# so a Rawhide installroot silently tracks whatever Rawhide is that day and
# ages out immediately. A pinned release installs from versioned repos and
# only needs a bump when it goes EOL — pick the newest stable at the time.
MACHINE_FIXTURE_RELEASEVER = "44"
# gpgcheck=0 because the Rawhide image carries Rawhide's key, not the
# release key — acceptable inside this throwaway privileged test
# container, do not copy this stanza anywhere it matters.
MACHINE_FIXTURE_REPO = """[fedora44]
name=Fedora 44 - x86_64
metalink=https://mirrors.fedoraproject.org/metalink?repo=fedora-44&arch=x86_64
enabled=1
gpgcheck=0
"""

# Second fixture machine: a transient copy of the test container's own Rawhide
# root (systemd's TEST-87 pattern: --volatile=yes --directory=/ with /etc
# bound read-only), so container collection is also exercised against current
# systemd. The pinned machine above runs systemd 259, older than the Metrics
# socket (v260+), so its units collection falls back to D-Bus; this one carries
# every endpoint and must be collected over varlink without any D-Bus fetches.
MACHINE_CURRENT_NAME = "testbox-current"
MACHINE_NAMES: tuple[str, ...] = (MACHINE_FIXTURE_NAME, MACHINE_CURRENT_NAME)

# Stock monitord.conf tracks units that do not exist in a Rawhide container.
ALLOWLIST_RENAMES: dict[str, str] = {
    "chrony.service": "kmod-static-nodes.service",
    "sshd.service": "dbus-broker.service",
}

# [services] gets pointed at a real unit too, so the per-service stats both
# paths now produce are actually compared. One real service is enough: the
# mapping is shared, so a second would exercise no new code.
SERVICE_RENAMES: dict[str, str] = {"sshd.service": "dbus-broker.service"}

# Fixture service installed into the container with every accounting switch on
# plus memory limits, so the cgroupfs reader (#221) is proven against real
# (non-`[not set]`) values rather than only against unset sentinels. The name
# must not collide with anything in stock monitord.conf, and it is appended
# to [services] (not renamed) so dbus-broker stays covered too.
CGROUP_FIXTURE_SERVICE = "monitord-cgroup-test.service"
CGROUP_FIXTURE_UNIT = """[Unit]
Description=monitord cgroupfs fixture: all accounting on plus memory limits

[Service]
Type=simple
ExecStart=/usr/bin/sleep infinity
CPUAccounting=yes
MemoryAccounting=yes
IOAccounting=yes
TasksAccounting=yes
MemoryMax=1G
MemoryHigh=512M
"""

# Sentinel D-Bus/systemd uses for "not set" (u64::MAX, see README's "Large
# u64 values" section). Every cgroup-derived field of the fixture service
# must differ from it on both paths: the fixture has all accounting on, so a
# sentinel means the reader (or its fallback) failed to find real data.
NOT_SET = 18446744073709551615

# The seven ServiceStats fields read from cgroupfs (#221). cpuusage_nsec,
# memory_current and memory_available stay excluded from the dbus-vs-varlink
# parity comparison (volatile across the seconds-apart runs) but are asserted
# present here; the other four are stable and compared by both checks.
CGROUP_FIELDS: tuple[str, ...] = (
    "cpuusage_nsec",
    "memory_current",
    "memory_available",
    "tasks_current",
    "processes",
    "ioread_bytes",
    "ioread_operations",
)

# Real Rawhide units the generated config tracks, and the states the parity
# comparison assumes. kmod-static-nodes is inactive + oneshot (exercises the
# oneshot health override); dbus-broker is active (guards the other direction).
FIXTURE_UNITS: dict[str, dict[str, str]] = {
    "kmod-static-nodes.service": {
        "LoadState": "loaded",
        "ActiveState": "inactive",
        "Type": "oneshot",
    },
    "dbus-broker.service": {"LoadState": "loaded", "ActiveState": "active"},
}

# Keys excluded from the parity comparison:
#   monitord.pid1.*             procfs values, change between sequential runs
#   machines.*.pid1.*           same, inside containers (fd counts move as
#                               connections come and go between the runs)
#   stat_collection_run_time_ms end-to-end wall time, also varies
#   collector_timings.*         per-collector wall times, vary between runs
#   collection_timings.*        inner units phase timings vary between runs, and
#                               the *_dbus_fetches counters legitimately differ
#                               (varlink oneshot type lookups count toward
#                               service_dbus_fetches) so they must not be
#                               expected to match by design
#   varlink_usage.*             asserted separately (see
#                               assert_varlink_usage): the D-Bus run reports all
#                               zeros and the varlink run all ones, so they must
#                               not be expected to match by design
#   time_in_state_usecs         now-relative per-unit value, varies between runs
#   services.*.cpuusage_nsec    live cgroup accounting, sampled seconds apart:
#   services.*.memory_current   CPU time only ever grows and memory moves under
#   services.*.memory_available a running service. Every other ServiceStats
#                               field is compared, including the timestamps,
#                               process/task counts and the unset sentinels.
EXCLUDED_KEY_PARTS: tuple[str, ...] = (
    ".pid1.",
    "stat_collection_run_time_ms",
    "collector_timings.",
    "collection_timings.",
    "varlink_usage.",
    "time_in_state_usecs",
    "cpuusage_nsec",
    "memory_current",
    "memory_available",
)

# Every varlink-capable collector the CI configs enable, and the gauge value
# each run is expected to report: 1 when the varlink attempt served the
# collector, 0 when it fell back to D-Bus. The Rawhide container runs systemd
# 262+, past every endpoint's minimum version, so the varlink run must use
# varlink everywhere — an older-system regression that silently falls back
# (e.g. a renamed socket or a stricter version gate) fails here instead of
# passing vacuously. The D-Bus run must report all zeros: anything else means
# varlink leaked in with [varlink] disabled, and the comparison would no
# longer be D-Bus vs varlink.
EXPECTED_VARLINK_COLLECTORS: tuple[str, ...] = (
    "version",
    "system_state",
    "units",
    "networkd",
    "machines",  # enumeration is D-Bus-only, so always 0 (see #37)
    "boot_blame",
    "verify",
)

EXPECTED_VARLINK_USAGE: dict[str, dict[str, int]] = {
    "dbus": {collector: 0 for collector in EXPECTED_VARLINK_COLLECTORS},
    "varlink": {
        collector: 0 if collector == "machines" else 1
        for collector in EXPECTED_VARLINK_COLLECTORS
    },
}

# Per-machine collector transports (machines.<name>.varlink_usage.*), which
# the parity comparison excludes like the host gauges. Container varlink goes
# through a per-machine connector inside the machine's PID namespace (see
# src/varlink/machine_connector.rs; the test runs monitord as root, so it has
# the CAP_SYS_ADMIN that needs). On the systemd 259 machine PID 1's Describe
# works, but there is no Metrics socket (v260+) and networkd is not enabled,
# so units and networkd fall back; the current machine is varlink throughout.
MACHINE_COLLECTORS: tuple[str, ...] = ("networkd", "system_state", "units", "version")
EXPECTED_MACHINE_VARLINK_USAGE: dict[str, dict[str, dict[str, int]]] = {
    "dbus": {
        machine: {collector: 0 for collector in MACHINE_COLLECTORS}
        for machine in MACHINE_NAMES
    },
    "varlink": {
        MACHINE_FIXTURE_NAME: {
            "networkd": 0,
            "system_state": 1,
            "units": 0,
            "version": 1,
        },
        MACHINE_CURRENT_NAME: {collector: 1 for collector in MACHINE_COLLECTORS},
    },
}

# The container varlink attempts that must fall back in the varlink run, as
# (machine, collector) — exactly these: any other fallback (host or container)
# means a collector silently stopped using varlink, and a missing one means
# the fixture changed or the log phrasing did and the check went blind.
EXPECTED_CONTAINER_FALLBACKS: frozenset[tuple[str, str]] = frozenset(
    {(MACHINE_FIXTURE_NAME, "units"), (MACHINE_FIXTURE_NAME, "networkd")}
)


def step(message: str) -> None:
    print(f"\n=== {message} ===", flush=True)


def run(cmd: Sequence[str], capture: bool = True, stdin: Optional[str] = None) -> str:
    result = subprocess.run(cmd, capture_output=capture, text=True, input=stdin)
    if result.returncode != 0:
        output = (result.stdout or "") + (result.stderr or "") if capture else ""
        raise SystemExit(f"FAIL: command failed: {' '.join(cmd)}\n{output}")
    return result.stdout if capture else ""


def docker(*args: str, capture: bool = True, stdin: Optional[str] = None) -> str:
    return run(["docker", *args], capture=capture, stdin=stdin)


def docker_exec(container: str, *args: str, capture: bool = True) -> str:
    return docker("exec", container, *args, capture=capture)


def write_container_file(container: str, path: str, content: str) -> None:
    """Write a file inside the container over stdin.

    Not `docker cp`: systemd mounts a tmpfs over /tmp during boot, and `docker
    cp` writes to the image layer underneath it, where the file is invisible to
    anything running in the container.
    """
    docker("exec", "-i", container, "tee", path, stdin=content)


def container_running(container: str) -> bool:
    return container in docker("ps", "--format", "{{.Names}}").split()


def remove_container(container: str) -> None:
    """Remove the container if there is one to remove.

    `docker rm -f` on a name that does not exist is a no-op on current Docker,
    but older versions and podman treat it as an error, which would take --fresh
    down on the first run of a clean machine.
    """
    if container in docker("ps", "-a", "--format", "{{.Names}}").split():
        docker("rm", "-f", container)


def container_matches(container: str, repo: Path, image: str) -> bool:
    """Whether the running container was started from `image` with `repo` mounted.

    Containers are reused by name, so without this a `monitord-test` left
    running by another checkout would be reused — building and testing that
    checkout's source while reporting on this one.
    """
    image_used = docker("inspect", "-f", "{{.Config.Image}}", container).strip()
    # Started from the image by that name, but is it still the current build?
    # An image rebuilt since (e.g. a Dockerfile change adding a tool the test
    # needs) leaves the old container running the old filesystem.
    image_id = docker("inspect", "-f", "{{.Image}}", container).strip()
    current_id = docker("image", "inspect", "-f", "{{.Id}}", image).strip()
    mounted = docker(
        "inspect",
        "-f",
        '{{range .Mounts}}{{if eq .Destination "/workspace"}}{{.Source}}{{end}}{{end}}',
        container,
    ).strip()
    # Docker Desktop reports host bind mounts under a /host_mnt prefix.
    return (
        image_used == image
        and image_id == current_id
        and mounted.removeprefix("/host_mnt") == str(repo)
    )


# Binaries the test needs inside the container, with what needs them. Checked
# up front so a drifted image fails with one clear message here, rather than
# halfway through with whatever error the missing tool happens to produce.
REQUIRED_CONTAINER_TOOLS: dict[str, str] = {
    "systemd-nspawn": "booting the fixture machines",
    "machinectl": "registering and inspecting the fixture machines",
    "systemd-run": "the capability matrix and the nspawn transient units",
    "useradd": "the unprivileged user of the capability matrix",
    "dnf": "installing the pinned fixture machine root",
    "cargo": "building monitord",
}


def assert_container_tools(container: str) -> None:
    step("Checking the image carries the tools the test needs")
    missing = {
        tool: why
        for tool, why in REQUIRED_CONTAINER_TOOLS.items()
        if subprocess.run(
            ["docker", "exec", container, "sh", "-c", f"command -v {tool}"],
            capture_output=True,
        ).returncode
        != 0
    }
    if missing:
        detail = ", ".join(f"{tool} ({why})" for tool, why in sorted(missing.items()))
        raise SystemExit(
            f"FAIL: the container image is missing: {detail} — add it to the "
            "Dockerfile (the GitHub Action builds the image from scratch, so a "
            "local-only failure here means a stale image: rerun with --fresh)"
        )
    print(f"PASS: {', '.join(sorted(REQUIRED_CONTAINER_TOOLS))} present")


def start_container(repo: Path, image: str, container: str) -> None:
    step(f"Starting {image} container with systemd")
    docker(
        "run",
        "-d",
        "--name",
        container,
        "--privileged",
        "--cgroupns=host",
        "-v",
        "/sys/fs/cgroup:/sys/fs/cgroup:rw",
        "-v",
        f"{repo}:/workspace",
        image,
        "/sbin/init",
    )
    for poll in range(1, 61):
        state = subprocess.run(
            ["docker", "exec", container, "systemctl", "is-system-running"],
            capture_output=True,
            text=True,
        ).stdout.strip()
        if state in ("running", "degraded"):
            print(f"systemd is {state} after {poll} polls")
            return
        print(f"Waiting for systemd ({poll}/60) status={state or 'unknown'}...")
        time.sleep(3)
    raise SystemExit("FAIL: timed out waiting for systemd")


def build_ci_configs(conf_text: str) -> tuple[str, str]:
    """Derive the (D-Bus, varlink) test configs from the stock monitord.conf.

    The stock config tracks units that do not exist in the container, so point
    the state_stats allowlist and [services] at real fixture units instead. The
    timers allowlist is emptied so per-timer stats are compared too, boot blame
    is switched on with its cache off, and verify is switched on restricted to
    the fixture units so analyze stays fast. Full enumeration parity is checked
    separately from the `verify enumerated ...` debug lines, so the comparison
    still covers enumeration without analyze running over every unit. All of
    those values are absolute boot-time measurements or deterministic analyze
    output, so they are stable across the seconds-apart runs.
    """
    dbus_lines: list[str] = []
    renamed: dict[str, str] = {}
    section = ""
    for line in conf_text.splitlines():
        if line.startswith("["):
            section = line.strip()
            if section == "[verify.allowlist]":
                # Restrict analyze to the fixture units: full verify runs
                # analyze over every unit and would dominate the test's
                # runtime. Enumeration parity over the full set is checked
                # separately from the debug log.
                dbus_lines.append(line)
                dbus_lines.append("dbus-broker.service")
                dbus_lines.append("kmod-static-nodes.service")
                continue
        elif section == "[verify.allowlist]":
            # Drop the stock body: the fixture allowlist above replaces it,
            # and an uncommented entry must not silently widen the run.
            continue
        elif section == "[services]":
            line = SERVICE_RENAMES.get(line.strip(), line)
            # Track the cgroup fixture alongside the renamed real service:
            # appending (not renaming) keeps dbus-broker covered too.
            if line.strip() == "dbus-broker.service":
                dbus_lines.append(line)
                line = CGROUP_FIXTURE_SERVICE
        elif section == "[machines.allowlist]":
            # Stock monitord.conf names machines that do not exist here;
            # point the allowlist at the nspawn fixture instead. Tracked
            # the same way as the state_stats allowlist below: a global
            # search would still pass with an empty allowlist and the
            # machine assertions would silently cover nothing.
            if line.strip() == "fedora38":
                dbus_lines.append(MACHINE_FIXTURE_NAME)
                line = MACHINE_CURRENT_NAME
                for machine in MACHINE_NAMES:
                    renamed[machine] = "[machines.allowlist]"
        elif section == "[units.state_stats.allowlist]":
            # Tracked per substitution, not by searching the finished config:
            # the fixture units also appear in other sections, so a global
            # search would still pass with an empty state_stats allowlist and
            # the parity run would silently stop comparing per-unit state.
            replacement = ALLOWLIST_RENAMES.get(line.strip())
            if replacement is not None:
                line = replacement
                renamed[replacement] = "[units.state_stats.allowlist]"
        elif section == "[timers.allowlist]" and line.strip() == "fstrim.timer":
            continue
        elif section == "[boot]" and line.strip() == "enabled = false":
            # Boot blame ships disabled, so neither path collects it by default
            # and the comparison would say nothing about it.
            line = "enabled = true"
        elif section == "[boot]" and line.strip() == "cache_enabled = true":
            # Both runs share cache_dir, so leaving this on would have the D-Bus
            # run read back the cache the varlink run just wrote — the two
            # outputs would match because they came from the same collection.
            line = "cache_enabled = false"
        elif section == "[verify]" and line.strip() == "enabled = false":
            # Verify ships disabled, so neither path collects it by default
            # and the comparison would say nothing about enumeration parity.
            line = "enabled = true"
        dbus_lines.append(line)

    # renamed maps name -> section, so the message says which section to
    # go fix for each missing name — not just the successfully tracked ones.
    wanted: dict[str, str] = {v: "[units.state_stats.allowlist]" for v in ALLOWLIST_RENAMES.values()}
    wanted.update({machine: "[machines.allowlist]" for machine in MACHINE_NAMES})
    missing = {name: wanted[name] for name in sorted(set(wanted) - set(renamed))}
    if missing:
        detail = ", ".join(f"{name} ({section})" for name, section in missing.items())
        raise SystemExit(
            "FAIL: monitord.conf no longer names the units/machines this test renames, "
            f"so nothing tracks them (missing: {detail})"
        )

    varlink_lines: list[str] = []
    section = ""
    enabled_varlink = False
    for line in dbus_lines:
        if line.startswith("["):
            section = line.strip()
        elif section == "[varlink]" and line.strip() == "enabled = false":
            line = "enabled = true"
            enabled_varlink = True
        varlink_lines.append(line)

    if not enabled_varlink:
        # Both configs would then select the same collection path and the parity
        # comparison would pass by comparing a run against itself.
        raise SystemExit(
            "FAIL: no '[varlink] enabled = false' in monitord.conf to flip, so the "
            "varlink run would repeat the D-Bus run"
        )

    return "\n".join(dbus_lines) + "\n", "\n".join(varlink_lines) + "\n"


def generate_configs(repo: Path, container: str) -> None:
    step("Generating CI configs")
    dbus_config, varlink_config = build_ci_configs((repo / "monitord.conf").read_text())
    write_container_file(container, DBUS_CONF, dbus_config)
    write_container_file(container, VARLINK_CONF, varlink_config)
    print(f"Wrote {DBUS_CONF} and {VARLINK_CONF}")


def assert_fixture_units(container: str) -> None:
    step("Asserting fixture units have expected states")
    # The comparison only exercises per-unit state (incl. the oneshot health
    # override) while these rolling-Rawhide fixtures exist with the assumed
    # states. Fail loudly rather than passing vacuously.
    for unit, expected in FIXTURE_UNITS.items():
        for prop, want in expected.items():
            got = docker_exec(
                container, "systemctl", "show", unit, "-p", prop, "--value"
            ).strip()
            if got != want:
                raise SystemExit(
                    f"FAIL: {unit} {prop} is {got!r}, expected {want!r} — "
                    "fixture unit changed state, type, or disappeared"
                )
    print(f"PASS: {', '.join(FIXTURE_UNITS)} have expected states")


def install_machine_fixture(container: str) -> None:
    step("Booting nspawn fixture machine")
    # A real container for the machines module to collect from, so the
    # machine fetch-counter assertions below pin actual D-Bus traffic
    # instead of asserting on an empty machine set. Built from the host's
    # own Rawhide root (no image rebuild), booted with PrivateUsers=no so
    # the host can read its cgroup tree via /proc/<leader>/root. Kept
    # minimal on purpose: every unit inside it multiplies the D-Bus calls
    # the assertions count.
    root = f"/var/lib/machines/{MACHINE_FIXTURE_NAME}"
    # Pinned-release repos (see MACHINE_FIXTURE_RELEASEVER): --use-host-config
    # would pull the Rawhide repo definition instead, so write a dedicated
    # repo file and point dnf at a conf dir containing only it.
    write_container_file(container, "/tmp/fixture44.repo", MACHINE_FIXTURE_REPO)
    try:
        docker_exec(
            container,
            "dnf",
            f"--releasever={MACHINE_FIXTURE_RELEASEVER}",
            "--setopt=reposdir=/tmp",
            "--installroot=" + root,
            "install", "-y", "systemd", "systemd-networkd",
        )
    except SystemExit:
        # A generic dnf failure here almost always means the pinned
        # release went EOL and its metalink stopped resolving — bump
        # MACHINE_FIXTURE_RELEASEVER rather than debugging dnf.
        raise SystemExit(
            f"FAIL: fixture root install failed — if Fedora "
            f"{MACHINE_FIXTURE_RELEASEVER} is EOL, bump "
            "MACHINE_FIXTURE_RELEASEVER"
        ) from None
    docker_exec(container, "mkdir", "-p", "/etc/systemd/nspawn")
    write_container_file(
        container,
        f"/etc/systemd/nspawn/{MACHINE_FIXTURE_NAME}.nspawn",
        "[Exec]\nPrivateUsers=no\n",
    )
    # --keep-unit dies with the exec session, and a second boot fails
    # while the first tree is busy: shut down any leftover instance, then
    # boot detached under a transient unit so it survives this exec call.
    # Terminate/reset tolerate absence: on a fresh container there is
    # nothing to clean up yet.
    subprocess.run(
        ["docker", "exec", container, "machinectl", "terminate", MACHINE_FIXTURE_NAME],
        capture_output=True,
    )
    # Drop the previous boot's transient unit too, or systemd-run refuses
    # to reuse the name ("was already loaded or has a fragment file").
    # `stop` clears the failed state with it, so no bare `reset-failed`
    # (which would reset every failed unit in the container) is needed.
    # Both tolerate absence: on a fresh container there is nothing to
    # clean up yet.
    subprocess.run(
        ["docker", "exec", container, "systemctl", "stop", f"nspawn-{MACHINE_FIXTURE_NAME}"],
        capture_output=True,
    )
    docker_exec(
        container,
        "systemd-run",
        "--unit", f"nspawn-{MACHINE_FIXTURE_NAME}",
        "--property=Type=notify",
        "systemd-nspawn",
        f"--machine={MACHINE_FIXTURE_NAME}",
        "-D", root,
        "--boot",
    )
    for poll in range(1, 31):
        listed = docker_exec(container, "machinectl", "list")
        if MACHINE_FIXTURE_NAME in listed:
            print(f"machine {MACHINE_FIXTURE_NAME} registered after {poll} polls")
            return
        time.sleep(3)
    raise SystemExit(f"FAIL: machine {MACHINE_FIXTURE_NAME} never registered")


def install_current_machine_fixture(container: str) -> None:
    step("Booting current-systemd fixture machine")
    # See MACHINE_CURRENT_NAME. No image install: the machine is the test
    # container's own root, so it runs the same current systemd as the host.
    # --private-network gives it its own netns, so networkd runs inside it
    # (with only lo) without touching host interfaces the parity check reads.
    # The cgroup fixture unit is host-only: hidden from the machine so its
    # [services] entry is absent there on both paths alike.
    name = MACHINE_CURRENT_NAME
    write_container_file(
        container, f"/etc/systemd/nspawn/{name}.nspawn", "[Exec]\nPrivateUsers=no\n"
    )
    subprocess.run(
        ["docker", "exec", container, "machinectl", "terminate", name],
        capture_output=True,
    )
    subprocess.run(
        ["docker", "exec", container, "systemctl", "stop", f"nspawn-{name}"],
        capture_output=True,
    )
    docker_exec(
        container,
        "systemd-run",
        "--unit", f"nspawn-{name}",
        "--property=Type=notify",
        "systemd-nspawn",
        f"--machine={name}",
        "--directory=/",
        "--volatile=yes",
        "--bind-ro=/etc",
        "--inaccessible=/etc/machine-id",
        f"--inaccessible=/etc/systemd/system/{CGROUP_FIXTURE_SERVICE}",
        "--private-network",
        "--boot",
    )
    for poll in range(1, 31):
        state = subprocess.run(
            ["docker", "exec", container, "systemctl", "-M", name, "is-system-running"],
            capture_output=True,
            text=True,
        ).stdout.strip()
        if state in ("running", "degraded"):
            print(f"machine {name} is {state} after {poll} polls")
            return
        time.sleep(2)
    raise SystemExit(f"FAIL: machine {name} never finished booting")


def install_cgroup_fixture(container: str) -> None:
    step("Installing cgroup fixture service")
    # Written at test time (not baked into the image) so iterating on the
    # fixture needs no image rebuild. Installed with all accounting on plus
    # memory limits, it is the service that proves the cgroupfs reader (#221)
    # returns real values on both paths.
    write_container_file(
        container,
        f"/etc/systemd/system/{CGROUP_FIXTURE_SERVICE}",
        CGROUP_FIXTURE_UNIT,
    )
    docker_exec(container, "systemctl", "daemon-reload")
    docker_exec(container, "systemctl", "start", CGROUP_FIXTURE_SERVICE)
    state = docker_exec(
        container, "systemctl", "is-active", CGROUP_FIXTURE_SERVICE
    ).strip()
    if state != "active":
        raise SystemExit(f"FAIL: {CGROUP_FIXTURE_SERVICE} is {state!r}, not active")
    print(f"PASS: {CGROUP_FIXTURE_SERVICE} active")


def assert_cgroup_fixture_values(outputs: dict[str, Stats]) -> None:
    step("Asserting cgroup fixture reports real values on both paths")
    # The parity comparison proves the two paths agree; this proves they
    # agree on something real. With all accounting on, every cgroup-derived
    # field must be populated — a NOT_SET sentinel means the cgroupfs reader
    # found nothing and its fallback (D-Bus props / Unit.List reply) did not
    # cover it either. `sleep infinity` is one single-threaded process doing
    # no IO, so processes/tasks are exactly 1 and the IO counters exactly 0;
    # cpu/memory/IO just need to be present, not [not set] (volatile across
    # runs — and the execve can charge first-touch page-cache misses to the
    # fixture's cgroup on a cold container, so the IO counters are not
    # exactly deterministic either).
    expected_exact: dict[str, int] = {
        "processes": 1,
        "tasks_current": 1,
    }
    for path_name, stats in outputs.items():
        for field in CGROUP_FIELDS:
            key = f"monitord.services.{CGROUP_FIXTURE_SERVICE}.{field}"
            value = stats.get(key, "<missing>")
            if field in expected_exact:
                if value != expected_exact[field]:
                    raise SystemExit(
                        f"FAIL: {path_name}: {key}={value!r}, "
                        f"expected {expected_exact[field]!r}"
                    )
            elif not isinstance(value, int) or value == NOT_SET:
                raise SystemExit(
                    f"FAIL: {path_name}: {key}={value!r} — "
                    "expected a real cgroupfs value, not [not set]"
                )
        shown = ", ".join(
            f"{f}={stats[f'monitord.services.{CGROUP_FIXTURE_SERVICE}.{f}']}"
            for f in CGROUP_FIELDS
        )
        print(f"PASS: {path_name}: {shown}")


# The three per-scope D-Bus fetch counters every collection (host or
# machine) emits. Hoisted so both the presence guard and the value pins
# below check the same set — a renamed counter must fail loudly, not pass
# vacuously over a partial set.
FETCH_COUNTERS: tuple[str, ...] = (
    "service_dbus_fetches",
    "state_dbus_fetches",
    "timer_dbus_fetches",
)

# Exact per-machine, per-path container D-Bus traffic, filmed live. Exact
# match, not a lower bound: upward drift (more D-Bus as collectors move
# around) is the likelier regression, and zero where D-Bus is expected would
# mean the data went missing, not that the bus went quiet. The systemd 259
# machine's varlink run redoes units over D-Bus (no Metrics socket there), so
# it matches its D-Bus run; the current machine's varlink run is D-Bus-free.
EXPECTED_MACHINE_FETCHES: dict[str, dict[str, dict[str, int]]] = {
    MACHINE_FIXTURE_NAME: {
        "dbus": {"service_dbus_fetches": 1, "state_dbus_fetches": 2, "timer_dbus_fetches": 1},
        "varlink": {"service_dbus_fetches": 1, "state_dbus_fetches": 2, "timer_dbus_fetches": 1},
    },
    MACHINE_CURRENT_NAME: {
        "dbus": {"service_dbus_fetches": 1, "state_dbus_fetches": 2, "timer_dbus_fetches": 2},
        "varlink": {"service_dbus_fetches": 0, "state_dbus_fetches": 0, "timer_dbus_fetches": 0},
    },
}


def fetch_counters(stats: Stats, prefix: str, machine: str) -> dict[str, int]:
    """Return the `*_dbus_fetches` counters under one key prefix.

    Fails loudly unless all three counters are present: a missing counter
    means the scope was not collected at all, and asserting on a partial
    set would pass vacuously.
    """
    counters = {
        key.split(".")[-1]: value
        for key, value in stats.items()
        if key.startswith(prefix) and key.endswith("_dbus_fetches")
    }
    if set(counters) != set(FETCH_COUNTERS):
        raise SystemExit(
            f"FAIL: {machine} is missing fetch counters "
            f"(found: {sorted(counters)}) — not collected?"
        )
    return counters


def machine_fetch_counters(stats: Stats, machine: str) -> dict[str, int]:
    """Return the per-machine `*_dbus_fetches` counters for one machine."""
    return fetch_counters(
        stats, f"monitord.machines.{machine}.collection_timings.", machine
    )


def assert_machine_fetch_counters(dbus_stats: Stats, varlink_stats: Stats) -> None:
    step("Asserting machine fetch counters")
    # The host varlink run must be D-Bus-free (zero fetches); each machine's
    # traffic is pinned exactly per path (see EXPECTED_MACHINE_FETCHES).
    # Same presence guard as the machine counters: an empty dict would make
    # any() vacuously False and print a PASS over nothing.
    host = fetch_counters(varlink_stats, "monitord.collection_timings.", "host")
    if any(value != 0 for value in host.values()):
        raise SystemExit(
            f"FAIL: host varlink run is not D-Bus-free: {host} "
            "(see #37; containers are pinned separately below)"
        )
    print(f"PASS: host varlink run is D-Bus-free {host}")
    for machine in MACHINE_NAMES:
        for path_name, stats in (("dbus", dbus_stats), ("varlink", varlink_stats)):
            counters = machine_fetch_counters(stats, machine)
            want = EXPECTED_MACHINE_FETCHES[machine][path_name]
            if counters != want:
                raise SystemExit(
                    f"FAIL: {machine} {path_name} path machine counters drifted: "
                    f"{counters} != {want}"
                )
            print(f"PASS: {machine} {path_name} path D-Bus traffic pinned {counters}")


def assert_dead_bus_run(container: str) -> None:
    step("Asserting a varlink-only run survives a dead bus (lazy D-Bus)")
    # The headline claim of lazy D-Bus: with the bus pointed at a socket
    # that does not exist, every enabled collector must still succeed —
    # varlink/fs/procfs paths never connect — and the run must exit 0.
    # Only collectors with a varlink or non-D-Bus path are enabled here:
    # networkd is forced onto the file fallback (`varlink = false`) with
    # its ifindex map from sysfs (no bus), machines/dbus_stats are
    # D-Bus-only by design, and verify's `systemd-analyze` subprocess
    # talks to the bus itself.
    dead_conf = (
        docker_exec(container, "cat", VARLINK_CONF)
        .replace(
            "dbus_address = unix:path=/run/dbus/system_bus_socket",
            "dbus_address = unix:path=/nonexistent/monitord-test-bus",
        )
        .replace(
            "[networkd]\nenabled = true\nvarlink = true",
            "[networkd]\nenabled = true\nvarlink = false",
        )
        .replace("[verify]\nenabled = true", "[verify]\nenabled = false")
        .replace("[machines]\nenabled = true", "[machines]\nenabled = false")
        # no_fallback=true turns every fallback into a loud failure, so
        # this run proves the enabled collectors are varlink-clean rather
        # than silently D-Bus-served. The dead bus address doubles the
        # proof: any attempted fallback would fail to connect. Replaces
        # the existing block rather than inserting a duplicate key:
        # configparser lets the later assignment win, so an insert would
        # silently leave no_fallback=false and the proof would be vacuous.
        .replace(
            "[varlink]\nenabled = true\nno_fallback = false",
            "[varlink]\nenabled = true\nno_fallback = true",
        )
    )
    dead_path = "/tmp/monitord-dead-bus-ci.conf"
    write_container_file(container, dead_path, dead_conf)
    stats, _log = run_monitord(container, dead_path)
    failures = [
        key.removesuffix(".success")
        for key, value in stats.items()
        if key.startswith("monitord.collector_timings.")
        and key.endswith(".success")
        and value != 1
    ]
    if failures:
        raise SystemExit(
            f"FAIL: dead-bus run had failing collectors: {sorted(failures)}"
        )
    # The file fallback is the path under test here (varlink would prove
    # nothing about it), so the run must actually have collected
    # interfaces — an empty-but-successful collection would pass above
    # while proving nothing.
    managed = stats.get("monitord.networkd.managed_interfaces", 0)
    if not isinstance(managed, int) or managed < 1:
        raise SystemExit(
            f"FAIL: dead-bus run collected no networkd interfaces: {managed!r}"
        )
    print(
        "PASS: dead-bus run exited 0 with every enabled collector at success=1 "
        f"({managed} networkd interfaces via the file path)"
    )


def run_monitord(container: str, config_path: str) -> tuple[Stats, str]:
    """Run monitord in the container, returning its parsed stats and its log.

    Debug logging is on so the log carries the `verify enumerated ...` line
    the enumeration parity check scrapes.
    """
    result = subprocess.run(
        [
            "docker",
            "exec",
            container,
            f"{CONTAINER_TARGET_DIR}/release/monitord",
            "-c",
            config_path,
            "-l",
            "debug",
        ],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise SystemExit(f"FAIL: monitord -c {config_path} failed\n{result.stderr}")
    return json.loads(result.stdout), result.stderr


CONTAINER_FALLBACK_RE = re.compile(r"Varlink container (\S+) (.+?) failed, falling back")


def container_fallbacks(log: str) -> set[tuple[str, str]]:
    """Return the (machine, collector) pairs whose varlink attempt fell back."""
    return {
        (match.group(1), match.group(2).replace(" ", "_"))
        for match in map(CONTAINER_FALLBACK_RE.search, log.splitlines())
        if match
    }


def find_fallbacks(log: str) -> list[str]:
    """Return log lines where a collector fell back off varlink unexpectedly.

    Host fallbacks are always unexpected: they make the parity comparison pass
    vacuously. Container fallbacks are unexpected unless listed in
    EXPECTED_CONTAINER_FALLBACKS (older systemd in the pinned machine).
    """
    unexpected = []
    for line in log.splitlines():
        if "falling back" not in line:
            continue
        match = CONTAINER_FALLBACK_RE.search(line)
        key = (match.group(1), match.group(2).replace(" ", "_")) if match else None
        if key not in EXPECTED_CONTAINER_FALLBACKS:
            unexpected.append(line)
    return unexpected


def assert_no_varlink_fallback(log: str) -> None:
    step("Asserting the varlink run used varlink")
    # Every collector falls back to D-Bus when its varlink socket is missing or
    # errors, which would make the parity comparison below pass vacuously by
    # comparing D-Bus output against itself.
    fallbacks = find_fallbacks(log)
    if fallbacks:
        print("\n".join(fallbacks))
        raise SystemExit("FAIL: varlink run fell back to D-Bus (see above)")
    # The expected container fallbacks must actually show up: if they don't,
    # the fixture changed (update EXPECTED_CONTAINER_FALLBACKS) or the log
    # phrasing did and the filter above silently stopped matching.
    seen = container_fallbacks(log)
    if seen != EXPECTED_CONTAINER_FALLBACKS:
        raise SystemExit(
            "FAIL: container fallbacks differ from the expected set "
            f"(seen: {sorted(seen)}, expected: {sorted(EXPECTED_CONTAINER_FALLBACKS)})"
        )
    print(
        "PASS: no unexpected varlink fallback "
        f"(expected container fallbacks: {sorted(seen)})"
    )


def enumerated_verify_units(log: str) -> set[str]:
    """Return the unit set from the run's `verify enumerated ...` debug line.

    Fails loudly unless the log carries exactly one such line with a
    self-consistent, non-empty set: anything else would make the parity
    comparison below pass vacuously.
    """
    matches = [line for line in log.splitlines() if "verify enumerated " in line]
    if len(matches) != 1:
        raise SystemExit(
            "FAIL: expected one verify enumeration line in the log, "
            f"found {len(matches)}"
        )
    rest = matches[0].split("verify enumerated ", 1)[1]
    count_text, sep, names = rest.partition(" units: ")
    if not sep:
        raise SystemExit(f"FAIL: malformed verify enumeration line: {matches[0]!r}")
    try:
        count = int(count_text)
    except ValueError:
        raise SystemExit(
            f"FAIL: malformed verify enumeration count: {count_text!r}"
        ) from None
    units = set(filter(None, names.split(",")))
    if len(units) != count:
        raise SystemExit(
            f"FAIL: verify enumerated {count} units but {len(units)} unique names"
        )
    if not units:
        raise SystemExit(
            "FAIL: verify enumerated no units — a running systemd always has some"
        )
    return units


def assert_verify_enumeration_parity(dbus_log: str, varlink_log: str) -> None:
    step("Asserting verify enumeration parity")
    # Both paths enumerate every unit before the fixture allowlist narrows
    # analyze down, so diffing the logged sets proves full enumeration parity
    # without paying for analyze over every unit twice. Runs after the
    # fallback check: a varlink run that fell back would log the D-Bus set and
    # match trivially.
    dbus_units = enumerated_verify_units(dbus_log)
    varlink_units = enumerated_verify_units(varlink_log)
    if dbus_units != varlink_units:
        only_dbus = sorted(dbus_units - varlink_units)
        only_varlink = sorted(varlink_units - dbus_units)
        raise SystemExit(
            "FAIL: verify enumerated sets differ "
            f"(only D-Bus: {only_dbus}, only varlink: {only_varlink})"
        )
    print(f"PASS: both paths enumerated the same {len(dbus_units)} units")


def assert_time_in_state(outputs: dict[str, Stats]) -> None:
    step("Asserting time-in-state is populated on both paths")
    # time_in_state_usecs is excluded from the parity comparison (now-relative),
    # so assert presence separately: dbus-broker.service has a real state change
    # timestamp and must report a positive value on both paths.
    key = "monitord.unit_states.dbus-broker.service.time_in_state_usecs"
    for path_name, stats in outputs.items():
        value = stats.get(key)
        if not isinstance(value, int) or value <= 0:
            raise SystemExit(
                f"FAIL: {path_name}: time_in_state_usecs missing or zero: {value!r}"
            )
        print(f"PASS: {path_name} time_in_state_usecs={value}")


def varlink_usage(stats: Stats) -> dict[str, int]:
    """Return the {collector: 0/1} varlink usage gauges from a run's stats."""
    return {
        key.removeprefix("monitord.varlink_usage."): value
        for key, value in stats.items()
        if key.startswith("monitord.varlink_usage.")
    }


def assert_varlink_usage(outputs: dict[str, Stats]) -> None:
    step("Asserting varlink usage gauges")
    # Besides proving which transport served each collector, this is the
    # adoption-ratio input Grafana charts (sum/count over these gauges), so
    # both runs assert the exact expected set: a missing gauge would silently
    # shrink the denominator there, and an unexpected one would inflate it.
    for path_name, stats in outputs.items():
        usage = varlink_usage(stats)
        expected = EXPECTED_VARLINK_USAGE[path_name]
        missing = sorted(set(expected) - set(usage))
        extra = sorted(set(usage) - set(expected))
        if missing or extra:
            raise SystemExit(
                f"FAIL: {path_name}: varlink usage gauges wrong "
                f"(missing: {missing}, unexpected: {extra})"
            )
        wrong = sorted(
            collector
            for collector, want in expected.items()
            if usage[collector] != want
        )
        if wrong:
            detail = ", ".join(
                f"{collector}={usage[collector]} (want {expected[collector]})"
                for collector in wrong
            )
            raise SystemExit(f"FAIL: {path_name}: {detail}")
        adopted = sum(usage.values())
        print(
            f"PASS: {path_name}: {adopted}/{len(usage)} collectors on varlink "
            f"({', '.join(f'{c}={usage[c]}' for c in sorted(usage))})"
        )


def machine_varlink_usage(stats: Stats, machine: str) -> dict[str, int]:
    """Return one machine's {collector: 0/1} varlink usage gauges."""
    prefix = f"monitord.machines.{machine}.varlink_usage."
    return {
        key.removeprefix(prefix): value
        for key, value in stats.items()
        if key.startswith(prefix)
    }


def check_machine_varlink_usage(
    path_name: str, stats: Stats, expected: dict[str, dict[str, int]]
) -> None:
    for machine, want in expected.items():
        usage = machine_varlink_usage(stats, machine)
        if usage != want:
            raise SystemExit(
                f"FAIL: {path_name}: machine {machine} varlink usage {usage} != {want}"
            )
        print(f"PASS: {path_name}: machine {machine} varlink usage {usage}")


def assert_machine_varlink_usage(outputs: dict[str, Stats]) -> None:
    step("Asserting per-machine varlink usage gauges")
    for path_name, stats in outputs.items():
        check_machine_varlink_usage(
            path_name, stats, EXPECTED_MACHINE_VARLINK_USAGE[path_name]
        )


def shipped_unit_properties(repo: Path) -> list[str]:
    """`systemd-run -p` arguments reproducing monitord.service's hardening.

    Everything in the shipped [Service] section except what each run sets
    itself, so the capability runs below exercise the real deployment shape
    (NoNewPrivileges=yes, ProtectSystem=strict, ...) plus the documented
    AmbientCapabilities= drop-in.
    """
    args: list[str] = []
    section = ""
    for line in (repo / "monitord.service").read_text().splitlines():
        line = line.strip()
        if line.startswith("["):
            section = line
        elif section == "[Service]" and line and not line.startswith("#"):
            key = line.split("=", 1)[0]
            if key not in ("ExecStart", "Type", "User", "Group", "RuntimeDirectory"):
                args += ["-p", line]
    return args


CAPS_USER = "monitord-ci"
CAPS_CONF = "/run/monitord-caps-ci.conf"


def run_monitord_as_user(
    container: str, repo: Path, caps: str
) -> tuple[Stats, str]:
    """Run the varlink config as an unprivileged user with only `caps`."""
    result = subprocess.run(
        [
            "docker", "exec", container,
            "systemd-run", "-q", "--wait", "--pipe",
            "-p", f"User={CAPS_USER}", "-p", f"Group={CAPS_USER}",
            *shipped_unit_properties(repo),
            "-p", f"AmbientCapabilities={caps}",
            "-p", f"CapabilityBoundingSet={caps}",
            f"{CONTAINER_TARGET_DIR}/release/monitord", "-c", CAPS_CONF, "-l", "debug",
        ],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise SystemExit(
            f"FAIL: monitord with caps {caps!r} failed\n{result.stderr}"
        )
    return json.loads(result.stdout), result.stderr


def assert_capability_matrix(container: str, repo: Path) -> None:
    step("Asserting machine collection per capability set (README Permissions)")
    # The documented contract, run under the shipped unit's hardening:
    #   no caps                          -> machines unreachable
    #   CAP_SYS_PTRACE                   -> machines over D-Bus, one warning,
    #                                       no per-collector fallback noise
    #   CAP_SYS_PTRACE + CAP_SYS_ADMIN   -> machines over varlink
    subprocess.run(
        ["docker", "exec", container, "useradd", "--system", "--no-create-home", CAPS_USER],
        capture_output=True,
    )
    # /tmp is private under the shipped unit (PrivateTmp=yes).
    docker_exec(container, "cp", VARLINK_CONF, CAPS_CONF)

    stats, log = run_monitord_as_user(container, repo, "")
    collected = [m for m in MACHINE_NAMES if machine_varlink_usage(stats, m)]
    if collected:
        raise SystemExit(f"FAIL: no-caps run unexpectedly collected machines: {collected}")
    if log.count("Failed to connect to container") != len(MACHINE_NAMES):
        raise SystemExit("FAIL: no-caps run did not report every machine as unreachable")
    print("PASS: no capabilities: machines are unreachable, host collection unaffected")

    stats, log = run_monitord_as_user(container, repo, "CAP_SYS_PTRACE")
    check_machine_varlink_usage(
        "CAP_SYS_PTRACE",
        stats,
        {m: {c: 0 for c in MACHINE_COLLECTORS} for m in MACHINE_NAMES},
    )
    warnings = log.count("needs CAP_SYS_ADMIN and CAP_SYS_PTRACE")
    if warnings != 1:
        raise SystemExit(f"FAIL: CAP_SYS_PTRACE run warned {warnings} times, want once")
    if container_fallbacks(log) or "Spawned varlink connector" in log:
        raise SystemExit(
            "FAIL: CAP_SYS_PTRACE run kept trying container varlink after the warning"
        )
    print("PASS: CAP_SYS_PTRACE: machines over D-Bus after a single warning")

    stats, log = run_monitord_as_user(container, repo, "CAP_SYS_PTRACE CAP_SYS_ADMIN")
    check_machine_varlink_usage(
        "CAP_SYS_PTRACE+CAP_SYS_ADMIN", stats, EXPECTED_MACHINE_VARLINK_USAGE["varlink"]
    )
    print("PASS: CAP_SYS_PTRACE + CAP_SYS_ADMIN: machines over varlink")


CONNECTOR_PROBE = r"""
helpers() {
    for d in /proc/[0-9]*; do
        [ "$(tr '\0' '\n' < "$d/cmdline" 2>/dev/null | sed -n 2p)" = "__monitord-machine-connector" ] \
            && echo "${d#/proc/}"
    done
}
timeout 10 "$1" -c "$2" -l debug >/dev/null 2>/tmp/monitord-daemon-ci.log &
sleep 6
for pid in $(helpers); do
    printf 'helper %s %s pidns=%s\n' "$pid" \
        "$(grep -E '^(Uid|CapPrm|CapEff|CapAmb|NoNewPrivs):' "/proc/$pid/status" | tr -s '\t\n' '  ')" \
        "$(readlink "/proc/$pid/ns/pid")"
done
wait
sleep 1
echo "leftover $(helpers | wc -l)"
"""


def assert_connector_caching(container: str) -> None:
    step("Asserting machine connectors are spawned once and run unprivileged")
    # Daemon mode for several cycles: each machine's connector must be spawned
    # exactly once and reused, run as nobody (monitord runs as root here)
    # without any capabilities inside its machine's PID namespace, and be gone
    # once monitord exits.
    conf = re.sub(r"(?m)^daemon = false$", "daemon = true", docker_exec(container, "cat", VARLINK_CONF))
    conf = re.sub(r"(?m)^daemon_stats_refresh_secs = .*$", "daemon_stats_refresh_secs = 1", conf)
    if "daemon = true" not in conf:
        raise SystemExit("FAIL: could not switch the CI config to daemon mode")
    daemon_conf = "/tmp/monitord-daemon-ci.conf"
    write_container_file(container, daemon_conf, conf)
    probe = docker(
        "exec", "-i", container, "bash", "-s", "--",
        f"{CONTAINER_TARGET_DIR}/release/monitord", daemon_conf,
        stdin=CONNECTOR_PROBE,
    )
    log = docker_exec(container, "cat", "/tmp/monitord-daemon-ci.log")
    print(probe.strip())

    for machine in MACHINE_NAMES:
        cycles = log.count(f"Collecting container: machine: {machine} ")
        spawns = log.count(f"Spawned varlink connector for machine {machine} ")
        if cycles < 3 or spawns != 1:
            raise SystemExit(
                f"FAIL: {machine}: {spawns} connector spawns over {cycles} cycles "
                "(want exactly 1 over at least 3)"
            )
        print(f"PASS: {machine}: 1 connector spawn over {cycles} cycles")

    helpers = [line for line in probe.splitlines() if line.startswith("helper ")]
    if len(helpers) != len(MACHINE_NAMES):
        raise SystemExit(f"FAIL: expected {len(MACHINE_NAMES)} helpers, saw {helpers}")
    leader_ns = {
        docker_exec(
            container, "sh", "-c",
            f"readlink /proc/$(machinectl show {machine} -p Leader --value)/ns/pid",
        ).strip()
        for machine in MACHINE_NAMES
    }
    helper_ns = set()
    for line in helpers:
        uids = re.search(r"Uid: (\d+) (\d+) (\d+) (\d+)", line)
        caps = dict(re.findall(r"(CapPrm|CapEff|CapAmb): ([0-9a-f]+)", line))
        no_new_privs = re.search(r"NoNewPrivs: (\d)", line)
        pidns = re.search(r"pidns=(\S+)", line)
        if (
            not uids
            or set(uids.groups()) != {"65534"}
            or set(caps) != {"CapPrm", "CapEff", "CapAmb"}
            or any(int(value, 16) for value in caps.values())
            or not no_new_privs
            or no_new_privs.group(1) != "1"
            or not pidns
        ):
            raise SystemExit(f"FAIL: machine connector is not unprivileged: {line}")
        helper_ns.add(pidns.group(1))
    if helper_ns != leader_ns:
        raise SystemExit(
            f"FAIL: helpers not in the machines' PID namespaces: {helper_ns} != {leader_ns}"
        )
    print("PASS: connectors run as nobody, capability-free, in their machines' PID namespaces")

    leftover = probe.strip().splitlines()[-1]
    if leftover != "leftover 0":
        raise SystemExit(f"FAIL: machine connectors outlived monitord ({leftover})")
    print("PASS: no connector outlives monitord")


def comparable(stats: Stats) -> Stats:
    return {
        key: value
        for key, value in stats.items()
        if not any(part in key for part in EXCLUDED_KEY_PARTS)
    }


def diff_outputs(dbus_stats: Stats, varlink_stats: Stats) -> list[str]:
    """Return one human-readable line per key the two paths disagree on."""
    dbus_stats, varlink_stats = comparable(dbus_stats), comparable(varlink_stats)
    missing = "<missing>"
    return [
        f"  {key}: dbus={dbus_stats.get(key, missing)!r} "
        f"varlink={varlink_stats.get(key, missing)!r}"
        for key in sorted(set(dbus_stats) | set(varlink_stats))
        if dbus_stats.get(key, missing) != varlink_stats.get(key, missing)
    ]


def compare_outputs(dbus_stats: Stats, varlink_stats: Stats) -> None:
    step("Comparing D-Bus and varlink output")
    differences = diff_outputs(dbus_stats, varlink_stats)
    if differences:
        print("\n".join(differences))
        raise SystemExit(f"FAIL: {len(differences)} key(s) differ (see above)")
    print(
        f"PASS: D-Bus and varlink outputs are identical "
        f"({len(comparable(dbus_stats))} keys compared)"
    )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo",
        type=Path,
        default=Path(__file__).resolve().parent,
        help="monitord checkout to build and test (default: this script's directory)",
    )
    parser.add_argument("--image", default=IMAGE, help="container image to build/use")
    parser.add_argument("--container", default=CONTAINER, help="container name")
    parser.add_argument(
        "--fresh",
        action="store_true",
        help="rebuild the image and recreate the container instead of reusing them",
    )
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    repo: Path = args.repo.resolve()

    if args.fresh:
        remove_container(args.container)
    # Always build: with the layer cache this is a no-op when the Dockerfile
    # has not changed, and it keeps local runs on the same image the Action
    # builds from scratch rather than on whatever was built months ago.
    step(f"Building {args.image} image")
    docker(
        "build",
        *(["--no-cache"] if args.fresh else []),
        "-t",
        args.image,
        str(repo),
        capture=False,
    )
    if container_running(args.container) and not container_matches(
        args.container, repo, args.image
    ):
        step(f"Replacing {args.container}: it holds a different repo or image build")
        remove_container(args.container)
    if not container_running(args.container):
        remove_container(args.container)
        start_container(repo, args.image, args.container)

    print(docker_exec(args.container, "systemctl", "--version").splitlines()[0])
    assert_container_tools(args.container)

    step("Building monitord (release)")
    docker(
        "exec",
        "-w",
        "/workspace",
        "-e",
        f"CARGO_TARGET_DIR={CONTAINER_TARGET_DIR}",
        args.container,
        "cargo",
        "build",
        "--release",
        capture=False,
    )

    step("Starting systemd-networkd")
    docker_exec(args.container, "systemctl", "enable", "--now", "systemd-networkd")

    step("Checking varlink sockets")
    for socket in (METRICS_SOCKET, NETWORK_SOCKET, MANAGER_SOCKET):
        found = subprocess.run(
            ["docker", "exec", args.container, "test", "-S", socket]
        ).returncode
        print(f"{'PASS' if found == 0 else 'WARN'}: {socket}")

    generate_configs(repo, args.container)
    assert_fixture_units(args.container)
    install_cgroup_fixture(args.container)
    install_machine_fixture(args.container)
    install_current_machine_fixture(args.container)

    step("Running monitord on both paths")
    # The fixture machine boots asynchronously: wait until monitord
    # actually collects it before the measured runs, or the machine
    # assertions would cover an empty set. Polls the varlink config run
    # (cheapest signal: the machine key set appears in the JSON).
    for poll in range(1, 20):
        probe, _ = run_monitord(args.container, VARLINK_CONF)
        if all(
            any(key.startswith(f"monitord.machines.{machine}.") for key in probe)
            for machine in MACHINE_NAMES
        ):
            print(f"fixture machines collected after {poll} polls")
            break
        print(f"Waiting for fixture machine collection ({poll}/20)...")
        time.sleep(3)
    else:
        raise SystemExit(f"FAIL: fixture machines {MACHINE_NAMES} never all collected")
    varlink_stats, varlink_log = run_monitord(args.container, VARLINK_CONF)
    dbus_stats, dbus_log = run_monitord(args.container, DBUS_CONF)

    assert_no_varlink_fallback(varlink_log)
    assert_varlink_usage({"varlink": varlink_stats, "dbus": dbus_stats})
    assert_machine_varlink_usage({"varlink": varlink_stats, "dbus": dbus_stats})
    assert_verify_enumeration_parity(dbus_log, varlink_log)
    assert_time_in_state({"varlink": varlink_stats, "dbus": dbus_stats})
    assert_cgroup_fixture_values({"varlink": varlink_stats, "dbus": dbus_stats})
    compare_outputs(dbus_stats, varlink_stats)
    assert_machine_fetch_counters(dbus_stats, varlink_stats)
    assert_dead_bus_run(args.container)
    assert_capability_matrix(args.container, repo)
    assert_connector_caching(args.container)
    print(
        f"\nContainer {args.container} left running; --fresh rebuilds the image "
        "without the cache and recreates it."
    )


if __name__ == "__main__":
    sys.exit(main())
