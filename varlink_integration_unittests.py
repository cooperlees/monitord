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

    def test_services_section_left_alone(self) -> None:
        # [services] must keep pointing at units the container does not have:
        # full ServiceStats has no varlink parity yet, so both paths are only
        # comparable while service_stats stays empty.
        services = section_body(self.dbus_conf, "[services]")
        self.assertIn("chrony.service", services)
        self.assertIn("sshd.service", services)

    def test_timers_allowlist_emptied(self) -> None:
        self.assertNotIn("fstrim.timer", self.dbus_conf)

    def test_only_varlink_config_enables_varlink(self) -> None:
        self.assertIn("[varlink]\nenabled = false", self.dbus_conf)
        self.assertIn("[varlink]\nenabled = true", self.varlink_conf)

    def test_configs_differ_only_in_varlink_toggle(self) -> None:
        self.assertEqual(
            self.dbus_conf.replace("enabled = false", "enabled = true"),
            self.varlink_conf,
        )

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
            "monitord.unit_states.foo.service.time_in_state_usecs": 100,
        }
        varlink_stats = {key: 999 for key in dbus_stats}
        self.assertEqual(vit.diff_outputs(dbus_stats, varlink_stats), [])


class FindFallbacksTest(unittest.TestCase):
    def test_fallback_line_detected(self) -> None:
        log = (
            "I0918 monitord: starting\n"
            "W0918 src/lib.rs:348] Varlink units stats failed, "
            "falling back to D-Bus: Os { code: 2 }\n"
        )
        self.assertEqual(len(vit.find_fallbacks(log)), 1)

    def test_clean_log_has_no_fallbacks(self) -> None:
        self.assertEqual(vit.find_fallbacks("I0918 monitord: starting\n"), [])


if __name__ == "__main__":
    unittest.main()
