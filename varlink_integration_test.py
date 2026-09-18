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

# Stock monitord.conf tracks units that do not exist in a Rawhide container.
ALLOWLIST_RENAMES: dict[str, str] = {
    "chrony.service": "kmod-static-nodes.service",
    "sshd.service": "dbus-broker.service",
}

# [services] gets pointed at a real unit too, so the per-service stats both
# paths now produce are actually compared. One real service is enough: the
# mapping is shared, so a second would exercise no new code.
SERVICE_RENAMES: dict[str, str] = {"sshd.service": "dbus-broker.service"}

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
#   stat_collection_run_time_ms end-to-end wall time, also varies
#   collector_timings.*         per-collector wall times, vary between runs
#   collection_timings.*        inner units phase timings vary between runs, and
#                               the *_dbus_fetches counters legitimately differ
#                               (varlink oneshot type lookups count toward
#                               service_dbus_fetches) so they must not be
#                               expected to match by design
#   time_in_state_usecs         now-relative per-unit value, varies between runs
#   services.*.cpuusage_nsec    live cgroup accounting, sampled seconds apart:
#   services.*.memory_current   CPU time only ever grows and memory moves under
#   services.*.memory_available a running service. Every other ServiceStats
#                               field is compared, including the timestamps,
#                               process/task counts and the unset sentinels.
EXCLUDED_KEY_PARTS: tuple[str, ...] = (
    "monitord.pid1.",
    "stat_collection_run_time_ms",
    "collector_timings.",
    "collection_timings.",
    "time_in_state_usecs",
    "cpuusage_nsec",
    "memory_current",
    "memory_available",
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
    timers allowlist is emptied so per-timer stats (backfilled over D-Bus on the
    varlink path) are compared too — timer timestamps are absolute and stable
    across the seconds-apart runs.
    """
    dbus_lines: list[str] = []
    renamed: set[str] = set()
    section = ""
    for line in conf_text.splitlines():
        if line.startswith("["):
            section = line.strip()
        elif section == "[services]":
            line = SERVICE_RENAMES.get(line.strip(), line)
        elif section == "[units.state_stats.allowlist]":
            # Tracked per substitution, not by searching the finished config:
            # the fixture units also appear in other sections, so a global
            # search would still pass with an empty state_stats allowlist and
            # the parity run would silently stop comparing per-unit state.
            replacement = ALLOWLIST_RENAMES.get(line.strip())
            if replacement is not None:
                line = replacement
                renamed.add(replacement)
        elif section == "[timers.allowlist]" and line.strip() == "fstrim.timer":
            continue
        dbus_lines.append(line)

    missing = set(ALLOWLIST_RENAMES.values()) - renamed
    if missing:
        raise SystemExit(
            "FAIL: [units.state_stats.allowlist] in monitord.conf no longer names "
            f"the units this test renames, so nothing tracks {sorted(missing)}"
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


def run_monitord(container: str, config_path: str) -> tuple[Stats, str]:
    """Run monitord in the container, returning its parsed stats and its log."""
    result = subprocess.run(
        [
            "docker",
            "exec",
            container,
            f"{CONTAINER_TARGET_DIR}/release/monitord",
            "-c",
            config_path,
        ],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise SystemExit(f"FAIL: monitord -c {config_path} failed\n{result.stderr}")
    return json.loads(result.stdout), result.stderr


def find_fallbacks(log: str) -> list[str]:
    """Return log lines where a collector fell back off varlink to D-Bus."""
    return [line for line in log.splitlines() if "falling back" in line]


def assert_no_varlink_fallback(log: str) -> None:
    step("Asserting the varlink run used varlink")
    # Every collector falls back to D-Bus when its varlink socket is missing or
    # errors, which would make the parity comparison below pass vacuously by
    # comparing D-Bus output against itself.
    fallbacks = find_fallbacks(log)
    if fallbacks:
        print("\n".join(fallbacks))
        raise SystemExit("FAIL: varlink run fell back to D-Bus (see above)")
    print("PASS: no collector fell back to D-Bus")


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

    step("Running monitord on both paths")
    varlink_stats, varlink_log = run_monitord(args.container, VARLINK_CONF)
    dbus_stats, _ = run_monitord(args.container, DBUS_CONF)

    assert_no_varlink_fallback(varlink_log)
    assert_time_in_state({"varlink": varlink_stats, "dbus": dbus_stats})
    compare_outputs(dbus_stats, varlink_stats)
    print(f"\nContainer {args.container} left running; --fresh recreates it.")


if __name__ == "__main__":
    sys.exit(main())
