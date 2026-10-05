"""Tests for config.py — run with `python3 -m unittest discover deploy/lib`."""

import os
import shlex
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import config  # noqa: E402

EXAMPLES = Path(__file__).parent.parent / "examples"


def load(text):
    with tempfile.NamedTemporaryFile("w", suffix=".toml", delete=False) as f:
        f.write(text)
    try:
        return config.load(f.name)
    finally:
        os.unlink(f.name)


def rejects(test, text, fragment):
    with test.assertRaises(config.ConfigError) as caught:
        load(text)
    test.assertIn(fragment, str(caught.exception))


class Examples(unittest.TestCase):
    """The shipped examples are what `deploy.sh init` copies — they must load."""

    def test_every_example_loads(self):
        for path in sorted(EXAMPLES.glob("*.toml")):
            with self.subTest(path.name):
                config.load(path)

    def test_all_in_one_uses_the_local_broker(self):
        c = config.load(EXAMPLES / "all-in-one.toml")
        self.assertTrue(c.broker_local)
        self.assertEqual((c.mqtt_host, c.mqtt_gateway_user), ("homescope-mqtt", "homescope-home"))

    def test_server_matches_srv01(self):
        c = config.load(EXAMPLES / "server.toml")
        self.assertEqual(c.components, ["api", "gateway"])
        self.assertEqual(c.mqtt_host, "host.containers.internal")
        self.assertEqual(c.grafana_publish, "127.0.0.1:4000")
        self.assertEqual(c.backup_dir, "/srv/homescope/backups")


class Defaults(unittest.TestCase):
    def test_minimal_server(self):
        c = load('components = ["api"]\n[mqtt]\nhost = "broker"\n')
        self.assertEqual(c.data_dir, "/var/lib/homescope/data")
        self.assertEqual(c.backup_dir, "/var/lib/homescope/data/backups")
        self.assertEqual(c.api_publish, "127.0.0.1:4001")
        self.assertEqual(c.grafana_publish, "4000")
        self.assertTrue(c.grafana_enabled)
        self.assertEqual(c.image_api, "ghcr.io/eldigh/homescope-api:latest")
        self.assertEqual(c.mqtt_port, 1883)

    def test_gateway_user_defaults_from_site(self):
        c = load('components = ["gateway"]\nsite = "thor"\n[mqtt]\nhost = "b"\n')
        self.assertEqual(c.mqtt_gateway_user, "homescope-thor")

    def test_components_come_out_in_canonical_order(self):
        c = load('components = ["gateway", "api", "broker"]\nsite = "s"\n')
        self.assertEqual(c.components, ["broker", "api", "gateway"])


class Strictness(unittest.TestCase):
    def test_unknown_top_level_key(self):
        rejects(self, 'componets = ["api"]\n', "unknown key 'componets'")

    def test_unknown_table(self):
        rejects(self, 'components = ["api"]\n[mqt]\nhost = "b"\n', "unknown table [mqt]")

    def test_unknown_key_in_table(self):
        rejects(self, 'components = ["api"]\n[mqtt]\nhost = "b"\nhots = "c"\n', "hots")

    def test_wrong_type(self):
        rejects(self, 'components = ["api"]\n[mqtt]\nhost = "b"\nport = "1883"\n', "mqtt.port must be int")

    def test_bool_is_not_a_port(self):
        rejects(self, 'components = ["api"]\n[mqtt]\nhost = "b"\nport = true\n', "must be int")

    def test_backup_scheduling_is_not_ours(self):
        # Removed 2026-10-05: the host's backup job calls `homescope backup
        # --snapshot DIR`. A leftover key must fail loudly, not look honoured.
        rejects(
            self,
            'components = ["api"]\n[mqtt]\nhost = "b"\n[backup]\non_calendar = "daily"\n',
            "on_calendar",
        )

    def test_toml_syntax_error_is_reported(self):
        rejects(self, 'components = ["api"]\nsite = \n', "at line 2")


class Components(unittest.TestCase):
    def test_empty(self):
        rejects(self, "components = []\n", "non-empty list")

    def test_unknown(self):
        rejects(self, 'components = ["grafana"]\n', "unknown component 'grafana'")

    def test_duplicate(self):
        rejects(self, 'components = ["api", "api"]\n[mqtt]\nhost = "b"\n', "twice")

    def test_broker_needs_the_all_in_one_shape(self):
        rejects(self, 'components = ["broker", "gateway"]\nsite = "s"\n', "all-in-one")

    def test_broker_and_external_mqtt_conflict(self):
        rejects(
            self,
            'components = ["broker", "api", "gateway"]\nsite = "s"\n[mqtt]\nhost = "b"\n',
            "omit [mqtt]",
        )

    def test_external_broker_needs_a_host(self):
        rejects(self, 'components = ["api"]\n', "host is required")

    def test_table_for_an_absent_component(self):
        rejects(
            self,
            'components = ["gateway"]\nsite = "s"\n[mqtt]\nhost = "b"\n[grafana]\nenabled = false\n',
            "[grafana] has no effect",
        )

    def test_image_for_an_absent_component(self):
        rejects(
            self,
            'components = ["gateway"]\nsite = "s"\n[mqtt]\nhost = "b"\n[images]\napi = "x"\n',
            "images.api has no effect",
        )


class Site(unittest.TestCase):
    def test_required_with_gateway(self):
        rejects(self, 'components = ["gateway"]\n[mqtt]\nhost = "b"\n', "site is required")

    def test_grammar(self):
        for bad in ["Thor", "a/b", "a b", "+"]:
            with self.subTest(bad):
                rejects(
                    self,
                    f'components = ["gateway"]\nsite = "{bad}"\n[mqtt]\nhost = "b"\n',
                    "may only contain",
                )

    def test_meaningless_without_gateway(self):
        rejects(self, 'components = ["api"]\nsite = "s"\n[mqtt]\nhost = "b"\n', "site has no effect")


class Values(unittest.TestCase):
    def test_publish_forms(self):
        for ok in ["4000", "127.0.0.1:4000", "0.0.0.0:80"]:
            with self.subTest(ok):
                load(f'components = ["api"]\n[mqtt]\nhost = "b"\n[api]\npublish = "{ok}"\n')
        for bad in ["localhost:4000", "4000:3000", "127.0.0.1:0", "70000", ""]:
            with self.subTest(bad):
                rejects(
                    self,
                    f'components = ["api"]\n[mqtt]\nhost = "b"\n[api]\npublish = "{bad}"\n',
                    "api.publish must be PORT or IPv4:PORT",
                )

    def test_relative_data_dir(self):
        rejects(self, 'components = ["api"]\ndata_dir = "srv"\n[mqtt]\nhost = "b"\n', "absolute path")


class Shell(unittest.TestCase):
    """The output is eval'd by bash: every value must come back unchanged."""

    def test_round_trips_through_bash(self):
        c = load(
            'components = ["api"]\n'
            "[mqtt]\n"
            "host = \"b'; touch /tmp/pwned; echo '\"\n"
            "[grafana]\n"
            'root_url = "https://g/$HOME/`id`"\n'
        )
        script = config.shell(c) + '\nprintf "%s\\n%s" "$CFG_MQTT_HOST" "$CFG_GRAFANA_ROOT_URL"'
        out = subprocess.run(["bash", "-c", script], capture_output=True, text=True, check=True)
        self.assertEqual(out.stdout.split("\n"), [c.mqtt_host, c.grafana_root_url])

    def test_component_flags(self):
        c = load('components = ["api"]\n[mqtt]\nhost = "b"\n')
        assignments = dict(line.split("=", 1) for line in config.shell(c).splitlines())
        self.assertEqual(assignments["CFG_HAS_API"], "true")
        self.assertEqual(assignments["CFG_HAS_GATEWAY"], "false")
        self.assertEqual(shlex.split(assignments["CFG_COMPONENTS"]), ["api"])


if __name__ == "__main__":
    unittest.main()
