#!/usr/bin/env python3
"""Unit tests for the pure logic in varlink_integration_test.py.

The container orchestration needs Docker and a Fedora Rawhide image, but config
generation and output comparison are pure functions and are what actually decide
pass/fail — so they get tested here:

    ./varlink_integration_unittests.py
"""

import unittest

import varlink_integration_test as vit

STOCK_CONF = """\
[services]
chrony.service
sshd.service

[timers.allowlist]
fstrim.timer

[units.state_stats.allowlist]
# Remove 'd' so it works when I test on ubuntu
chrony.service
sshd.service

[boot]
enabled = false
cache_enabled = true
cache_dir = /tmp
num_slowest_units = 5

[verify]
enabled = false

[verify.allowlist]
# example.service

[varlink]
enabled = false
"""


def section_body(conf: str, header: str) -> str:
    """Return one section's lines, bounded at the next section header."""
    body = conf.split(f"{header}\n", 1)[1]
    return body.split("\n[", 1)[0]


class BuildCiConfigsTest(unittest.TestCase):
    def setUp(self) -> None:
        self.dbus_conf, self.varlink_conf = vit.build_ci_configs(STOCK_CONF)

    def test_allowlist_points_at_container_units(self) -> None:
        allowlist = section_body(self.dbus_conf, "[units.state_stats.allowlist]")
        self.assertIn("kmod-static-nodes.service", allowlist)
        self.assertIn("dbus-broker.service", allowlist)
        self.assertNotIn("chrony.service", allowlist)

    def test_services_section_tracks_a_real_unit(self) -> None:
        # Both paths now produce full per-service stats, so [services] points at
        # a unit the container actually has — otherwise service_stats stays
        # empty and the comparison proves nothing about them.
        services = section_body(self.dbus_conf, "[services]")
        self.assertIn("dbus-broker.service", services)
        self.assertNotIn("sshd.service", services)

    def test_services_section_tracks_cgroup_fixture(self) -> None:
        # The all-accounting-on fixture proves the cgroupfs reader (#221)
        # against real values; it rides along dbus-broker, not instead of it.
        services = section_body(self.dbus_conf, "[services]")
        self.assertIn(vit.CGROUP_FIXTURE_SERVICE, services)
        self.assertIn("dbus-broker.service", services)

    def test_timers_allowlist_emptied(self) -> None:
        self.assertNotIn("fstrim.timer", self.dbus_conf)

    def test_boot_blame_enabled_with_cache_off(self) -> None:
        # Enabled so it is compared at all, and uncached so the second run
        # actually collects instead of reading back what the first one wrote.
        boot = section_body(self.dbus_conf, "[boot]")
        self.assertIn("enabled = true", boot)
        self.assertIn("cache_enabled = false", boot)
        self.assertNotIn("cache_enabled = true", boot)

    def test_verify_enabled_with_fixture_allowlist(self) -> None:
        # Enabled so enumeration parity is compared at all, restricted to the
        # fixture units so analyze does not run over every unit twice.
        verify = section_body(self.dbus_conf, "[verify]")
        self.assertIn("enabled = true", verify)
        allowlist = section_body(self.dbus_conf, "[verify.allowlist]")
        self.assertIn("dbus-broker.service", allowlist)
        self.assertIn("kmod-static-nodes.service", allowlist)

    def test_verify_allowlist_drops_stock_body(self) -> None:
        # The fixture allowlist replaces the stock body: neither the commented
        # examples nor any uncommented entry may leak into the test run.
        conf = STOCK_CONF.replace(
            "[verify.allowlist]\n# example.service",
            "[verify.allowlist]\n# example.service\nintruder.service",
        )
        dbus_conf, _ = vit.build_ci_configs(conf)
        allowlist = section_body(dbus_conf, "[verify.allowlist]")
        self.assertNotIn("example.service", allowlist)
        self.assertNotIn("intruder.service", allowlist)

    def test_only_varlink_config_enables_varlink(self) -> None:
        self.assertIn("[varlink]\nenabled = false", self.dbus_conf)
        self.assertIn("[varlink]\nenabled = true", self.varlink_conf)

    def test_configs_differ_only_in_varlink_toggle(self) -> None:
        # Compared line by line rather than by a blanket string replace: the
        # configs contain several `enabled = ...` keys, and a global swap would
        # also rewrite unrelated ones like [boot] cache_enabled.
        dbus_lines = self.dbus_conf.splitlines()
        varlink_lines = self.varlink_conf.splitlines()
        self.assertEqual(len(dbus_lines), len(varlink_lines))
        differing = [
            (index, dbus, varlink)
            for index, (dbus, varlink) in enumerate(zip(dbus_lines, varlink_lines))
            if dbus != varlink
        ]
        self.assertEqual(len(differing), 1, f"expected one differing line, got {differing}")
        index, dbus, varlink = differing[0]
        self.assertEqual((dbus, varlink), ("enabled = false", "enabled = true"))
        # ...and it must be the one inside [varlink].
        preceding = [line for line in dbus_lines[:index] if line.startswith("[")]
        self.assertEqual(preceding[-1], "[varlink]")

    def test_missing_units_to_rename_fails_loudly(self) -> None:
        # A monitord.conf whose allowlist stops naming the units we rename would
        # silently produce a config tracking no units at all.
        with self.assertRaises(SystemExit) as caught:
            vit.build_ci_configs("[units.state_stats.allowlist]\nfoo.service\n")
        self.assertIn("state_stats.allowlist", str(caught.exception))

    def test_fixture_units_named_elsewhere_do_not_count(self) -> None:
        # The fixture units also appear in other sections, so confirming the
        # rename by searching the whole config would pass with an empty state
        # allowlist — and the parity run would stop comparing per-unit state.
        conf = (
            "[services]\nkmod-static-nodes.service\ndbus-broker.service\n\n"
            "[units.state_stats.allowlist]\nsomething-else.service\n"
        )
        with self.assertRaises(SystemExit):
            vit.build_ci_configs(conf)

    def test_missing_varlink_toggle_fails_loudly(self) -> None:
        # With nothing to flip, both configs select the same collection path and
        # the parity comparison comes down to diffing a run against itself.
        conf = (
            "[units.state_stats.allowlist]\nchrony.service\nsshd.service\n\n"
            "[varlink]\nenabled = true\n"
        )
        with self.assertRaises(SystemExit) as caught:
            vit.build_ci_configs(conf)
        self.assertIn("varlink", str(caught.exception))


class DiffOutputsTest(unittest.TestCase):
    def test_identical_outputs_have_no_differences(self) -> None:
        stats = {"monitord.units.active_units": 42, "monitord.version.major": 262}
        self.assertEqual(vit.diff_outputs(stats, dict(stats)), [])

    def test_differing_value_is_reported(self) -> None:
        differences = vit.diff_outputs(
            {"monitord.units.active_units": 42}, {"monitord.units.active_units": 41}
        )
        self.assertEqual(len(differences), 1)
        self.assertIn("monitord.units.active_units", differences[0])
        self.assertIn("dbus=42", differences[0])
        self.assertIn("varlink=41", differences[0])

    def test_key_missing_from_one_path_is_reported(self) -> None:
        differences = vit.diff_outputs({"monitord.units.jobs_queued": 0}, {})
        self.assertEqual(len(differences), 1)
        self.assertIn("varlink='<missing>'", differences[0])

    def test_volatile_keys_are_excluded(self) -> None:
        dbus_stats = {
            "monitord.pid1.cpu_user_time": 1,
            "monitord.stat_collection_run_time_ms": 10.0,
            "monitord.collector_timings.0.elapsed_ms": 1.0,
            "monitord.units.collection_timings.timer_dbus_fetches": 4,
            "monitord.varlink_usage.units": 0,
            "monitord.unit_states.foo.service.time_in_state_usecs": 100,
            "monitord.services.foo.service.cpuusage_nsec": 5,
            "monitord.services.foo.service.memory_current": 5,
            "monitord.services.foo.service.memory_available": 5,
        }
        varlink_stats = {key: 999 for key in dbus_stats}
        self.assertEqual(vit.diff_outputs(dbus_stats, varlink_stats), [])

    def test_stable_service_fields_are_compared(self) -> None:
        # The live cgroup counters are excluded, but the rest of ServiceStats
        # must still be diffed — that is what proves the varlink mapping,
        # including the unset sentinels systemd omits.
        for field in (
            "status_errno",
            "processes",
            "tasks_current",
            "restart_usec",
            "timeout_clean_usec",
            "ioread_bytes",
            "active_enter_timestamp",
        ):
            key = f"monitord.services.foo.service.{field}"
            self.assertEqual(
                len(vit.diff_outputs({key: 1}, {key: 2})), 1, f"{field} not compared"
            )


class EnumeratedVerifyUnitsTest(unittest.TestCase):
    LOG_LINE = (
        "D0918 12:00:00.000000 1 verify.rs:123] "
        "verify enumerated 3 units: a.service,b.service,c.service\n"
    )

    def test_extracts_unit_set(self) -> None:
        log = "I0918 monitord: starting\n" + self.LOG_LINE
        self.assertEqual(
            vit.enumerated_verify_units(log),
            {"a.service", "b.service", "c.service"},
        )

    def test_missing_line_fails_loudly(self) -> None:
        with self.assertRaises(SystemExit):
            vit.enumerated_verify_units("I0918 monitord: starting\n")

    def test_duplicate_lines_fail_loudly(self) -> None:
        with self.assertRaises(SystemExit):
            vit.enumerated_verify_units(self.LOG_LINE + self.LOG_LINE)

    def test_count_mismatch_fails_loudly(self) -> None:
        log = self.LOG_LINE.replace("3 units", "4 units")
        with self.assertRaises(SystemExit):
            vit.enumerated_verify_units(log)

    def test_empty_set_fails_loudly(self) -> None:
        with self.assertRaises(SystemExit):
            vit.enumerated_verify_units("verify enumerated 0 units: \n")


class VarlinkUsageTest(unittest.TestCase):
    STATS = {
        "monitord.units.active_units": 42,
        "monitord.varlink_usage.version": 1,
        "monitord.varlink_usage.system_state": 1,
        "monitord.varlink_usage.units": 1,
        "monitord.varlink_usage.networkd": 1,
        "monitord.varlink_usage.machines": 0,
        "monitord.varlink_usage.boot_blame": 1,
        "monitord.varlink_usage.verify": 1,
    }

    def test_extracts_gauges(self) -> None:
        usage = vit.varlink_usage(self.STATS)
        self.assertEqual(len(usage), len(vit.EXPECTED_VARLINK_COLLECTORS))
        self.assertEqual(usage["version"], 1)
        # Host machine enumeration is D-Bus-only, so always 0.
        self.assertEqual(usage["machines"], 0)

    def test_varlink_run_fully_adopted(self) -> None:
        # Must not raise: every collector but enumeration on varlink.
        vit.assert_varlink_usage(
            {"varlink": dict(self.STATS), "dbus": self.dbus_stats()}
        )

    def test_varlink_fallback_fails_loudly(self) -> None:
        stats = dict(self.STATS, **{"monitord.varlink_usage.units": 0})
        with self.assertRaises(SystemExit) as caught:
            vit.assert_varlink_usage({"varlink": stats, "dbus": self.dbus_stats()})
        self.assertIn("units=0", str(caught.exception))

    def test_dbus_run_leaking_varlink_fails_loudly(self) -> None:
        stats = dict(self.dbus_stats(), **{"monitord.varlink_usage.units": 1})
        with self.assertRaises(SystemExit) as caught:
            vit.assert_varlink_usage({"varlink": dict(self.STATS), "dbus": stats})
        self.assertIn("units=1", str(caught.exception))

    def test_missing_gauge_fails_loudly(self) -> None:
        # A missing gauge would silently shrink Grafana's count() denominator.
        stats = {
            key: value for key, value in self.STATS.items() if "verify" not in key
        }
        with self.assertRaises(SystemExit) as caught:
            vit.assert_varlink_usage({"varlink": stats, "dbus": self.dbus_stats()})
        self.assertIn("missing", str(caught.exception))

    def test_unexpected_gauge_fails_loudly(self) -> None:
        stats = dict(self.STATS, **{"monitord.varlink_usage.dbus_stats": 0})
        with self.assertRaises(SystemExit) as caught:
            vit.assert_varlink_usage({"varlink": stats, "dbus": self.dbus_stats()})
        self.assertIn("unexpected", str(caught.exception))

    @staticmethod
    def dbus_stats() -> dict:
        return {
            key: 0
            for key in VarlinkUsageTest.STATS
            if key.startswith("monitord.varlink_usage.")
        }


class FindFallbacksTest(unittest.TestCase):
    def test_fallback_line_detected(self) -> None:
        log = (
            "I0918 monitord: starting\n"
            "W0918 src/machines.rs:375] Varlink units stats failed for container demo, "
            "falling back to D-Bus: Os { code: 2 }\n"
        )
        self.assertEqual(len(vit.find_fallbacks(log)), 1)

    def test_clean_log_has_no_fallbacks(self) -> None:
        self.assertEqual(vit.find_fallbacks("I0918 monitord: starting\n"), [])


if __name__ == "__main__":
    unittest.main()
