#!/usr/bin/env python3
"""Varlink vs D-Bus parity test for monitord.

Boots a Fedora Rawhide container under systemd, builds monitord inside it, then
runs it twice — once with `[varlink] enabled = true` and once with it false —
and asserts both JSON outputs match.

This is what the `varlink-integration` GitHub Action runs, so a failure there
reproduces locally with:

    ./varlink_integration_test.py

The container is left running between invocations for fast iteration; use
--fresh to rebuild it from scratch.
"""

import argparse
import json
import subprocess
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
    "monitord.pid1.",
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
    mounted = docker(
        "inspect",
        "-f",
        '{{range .Mounts}}{{if eq .Destination "/workspace"}}{{.Source}}{{end}}{{end}}',
        container,
    ).strip()
    # Docker Desktop reports host bind mounts under a /host_mnt prefix.
    return image_used == image and mounted.removeprefix("/host_mnt") == str(repo)


def image_exists(image: str) -> bool:
    return bool(docker("images", "-q", image).strip())


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
                line = MACHINE_FIXTURE_NAME
                renamed[MACHINE_FIXTURE_NAME] = "[machines.allowlist]"
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
    wanted[MACHINE_FIXTURE_NAME] = "[machines.allowlist]"
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
    # systemd-nspawn/machinectl come from the systemd-container package,
    # baked into the image (Dockerfile) — fail loudly here rather than
    # halfway through the install if a stale image predates it.
    docker_exec(container, "test", "-x", "/usr/bin/systemd-nspawn")
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
            "install", "-y", "systemd",
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

# Exact per-transport container traffic, filmed live (host 0/0/0, container
# 1/2/1 on both paths). Exact match, not a lower bound: upward drift (more
# D-Bus as collectors move around) is the likelier regression, and if #211
# is ever fixed upstream these move toward zero — update them then.
EXPECTED_MACHINE_FETCHES: dict[str, int] = {
    "service_dbus_fetches": 1,
    "state_dbus_fetches": 2,
    "timer_dbus_fetches": 1,
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
    # The host varlink run must be D-Bus-free (zero fetches), while the
    # container behind #211 cannot use varlink IPC at all: its timer
    # backfill and oneshot type override are D-Bus by necessity, and the
    # full D-Bus path additionally spends per-service fetches. The
    # assertion pins the current split per transport so a regression —
    # host-side D-Bus creeping back in, or container calls silently
    # disappearing along with their data — fails loudly instead of
    # drifting. If #211 is ever fixed upstream these numbers move toward
    # zero; update them then, not before.
    machine = MACHINE_FIXTURE_NAME
    dbus = machine_fetch_counters(dbus_stats, machine)
    varlink = machine_fetch_counters(varlink_stats, machine)
    print(f"dbus path machine counters: {dbus}")
    print(f"varlink path machine counters: {varlink}")
    # Same presence guard as the machine counters: an empty dict would
    # make any() vacuously False and print a PASS over nothing.
    host = fetch_counters(varlink_stats, "monitord.collection_timings.", "host")
    if any(value != 0 for value in host.values()):
        raise SystemExit(
            f"FAIL: host varlink run is not D-Bus-free: {host} "
            "(see #37; containers are pinned separately below)"
        )
    print(f"PASS: host varlink run is D-Bus-free {host}")
    # The container cannot avoid D-Bus (see #211), so its traffic is
    # pinned exactly: drift in either direction fails, since upward drift
    # (more D-Bus as collectors move) is the likelier regression and zero
    # would mean the data went missing, not that the bus went quiet.
    for path_name, counters in (("dbus", dbus), ("varlink", varlink)):
        if counters != EXPECTED_MACHINE_FETCHES:
            raise SystemExit(
                f"FAIL: {path_name} path machine counters drifted: "
                f"{counters} != {EXPECTED_MACHINE_FETCHES}"
            )
    print(f"PASS: container D-Bus traffic pinned (dbus={dbus}, varlink={varlink})")


def assert_dead_bus_run(container: str) -> None:
    step("Asserting a varlink-only run survives a dead bus (lazy D-Bus)")
    # The headline claim of lazy D-Bus: with the bus pointed at a socket
    # that does not exist, every enabled collector must still succeed —
    # varlink/fs/procfs paths never connect — and the run must exit 0.
    # Only collectors with a varlink or non-D-Bus path are enabled here:
    # networkd goes file-based with its ifindex map from sysfs (no bus),
    # machines/dbus_stats are D-Bus-only by design, and verify's
    # `systemd-analyze` subprocess talks to the bus itself.
    dead_conf = (
        docker_exec(container, "cat", VARLINK_CONF)
        .replace(
            "dbus_address = unix:path=/run/dbus/system_bus_socket",
            "dbus_address = unix:path=/nonexistent/monitord-test-bus",
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


def find_fallbacks(log: str) -> list[str]:
    """Return log lines where a host collector fell back off varlink.

    Container fallbacks (`Varlink container <name> ...`) are excluded: the
    container behind #211 cannot use varlink IPC at all, so its D-Bus
    traffic is expected and pinned separately by
    `assert_machine_fetch_counters`. Only a host-side fallback makes the
    parity comparison pass vacuously.
    """
    return [
        line
        for line in log.splitlines()
        if "falling back" in line and "Varlink container " not in line
    ]


def assert_no_varlink_fallback(log: str) -> None:
    step("Asserting the varlink run used varlink")
    # Every collector falls back to D-Bus when its varlink socket is missing or
    # errors, which would make the parity comparison below pass vacuously by
    # comparing D-Bus output against itself.
    fallbacks = find_fallbacks(log)
    if fallbacks:
        print("\n".join(fallbacks))
        raise SystemExit("FAIL: varlink run fell back to D-Bus (see above)")
    # The container filter above must actually be doing something: #211
    # guarantees the container falls back, so a log with no container
    # lines means the phrasing changed and the filter is silently dead.
    if not any("Varlink container " in line for line in log.splitlines()):
        raise SystemExit(
            "FAIL: no container fallback lines in the log — the "
            "find_fallbacks filter may have stopped matching"
        )
    print("PASS: no collector fell back to D-Bus")


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
    if args.fresh or not image_exists(args.image):
        step(f"Building {args.image} image")
        docker("build", "-t", args.image, str(repo), capture=False)
    if container_running(args.container) and not container_matches(
        args.container, repo, args.image
    ):
        step(f"Replacing {args.container}: it holds a different repo or image")
        remove_container(args.container)
    if not container_running(args.container):
        remove_container(args.container)
        start_container(repo, args.image, args.container)

    print(docker_exec(args.container, "systemctl", "--version").splitlines()[0])

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

    step("Running monitord on both paths")
    # The fixture machine boots asynchronously: wait until monitord
    # actually collects it before the measured runs, or the machine
    # assertions would cover an empty set. Polls the varlink config run
    # (cheapest signal: the machine key set appears in the JSON).
    for poll in range(1, 20):
        probe, _ = run_monitord(args.container, VARLINK_CONF)
        if any(
            key.startswith(f"monitord.machines.{MACHINE_FIXTURE_NAME}.")
            for key in probe
        ):
            print(f"fixture machine collected after {poll} polls")
            break
        print(f"Waiting for fixture machine collection ({poll}/20)...")
        time.sleep(3)
    else:
        raise SystemExit(
            f"FAIL: fixture machine {MACHINE_FIXTURE_NAME} never collected"
        )
    varlink_stats, varlink_log = run_monitord(args.container, VARLINK_CONF)
    dbus_stats, dbus_log = run_monitord(args.container, DBUS_CONF)

    assert_no_varlink_fallback(varlink_log)
    assert_varlink_usage({"varlink": varlink_stats, "dbus": dbus_stats})
    assert_verify_enumeration_parity(dbus_log, varlink_log)
    assert_time_in_state({"varlink": varlink_stats, "dbus": dbus_stats})
    assert_cgroup_fixture_values({"varlink": varlink_stats, "dbus": dbus_stats})
    compare_outputs(dbus_stats, varlink_stats)
    assert_machine_fetch_counters(dbus_stats, varlink_stats)
    assert_dead_bus_run(args.container)
    print(f"\nContainer {args.container} left running; --fresh recreates it.")


if __name__ == "__main__":
    sys.exit(main())
