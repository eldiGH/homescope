#!/usr/bin/env python3
"""Reads and validates a host's deploy.toml for deploy.sh and `homescope`.

    config.py PATH           shell assignments (CFG_*=...), for `eval`
    config.py --show PATH    the effective configuration, defaults filled in

Strict on purpose: an unknown key or table, a wrong type, or a component
combination this deploy cannot build is an error with the file's name in it.
A converge script that silently ignored `componets = [...]` would quietly
deploy something else. Values reach bash only through shlex.quote, so nothing
in the file is ever interpreted by a shell.

Needs Python >= 3.11 for tomllib (stdlib). Every distro with a Podman new
enough for quadlets has it.
"""

import ipaddress
import re
import shlex
import sys
import tomllib
from dataclasses import dataclass, fields

COMPONENTS = ("broker", "api", "gateway")
SITE_RE = re.compile(r"^[a-z0-9-]+$")  # same rule as common::site::Site and the sites CHECK
DEFAULT_DATA_DIR = "/var/lib/homescope/data"
DEFAULT_IMAGES = {
    "api": "ghcr.io/eldigh/homescope-api:latest",
    "gateway": "ghcr.io/eldigh/homescope-gateway:latest",
}

# Allowed keys per table, and the component a table belongs to. A table for a
# component this host does not run is an error rather than dead config.
TABLES = {
    "mqtt": (None, {"host", "port", "api_user", "gateway_user"}),
    "api": ("api", {"publish", "db_publish"}),
    "grafana": ("api", {"enabled", "publish", "root_url", "allow_embedding"}),
    "backup": ("api", {"dir"}),
    "images": (None, {"api", "gateway"}),
}
TOP_LEVEL = {"components", "site", "data_dir"}


class ConfigError(Exception):
    pass


@dataclass
class Config:
    components: list
    site: str
    data_dir: str
    broker_local: bool
    mqtt_host: str
    mqtt_port: int
    mqtt_api_user: str
    mqtt_gateway_user: str
    api_publish: str
    db_publish: str
    grafana_enabled: bool
    grafana_publish: str
    grafana_root_url: str
    grafana_allow_embedding: bool
    backup_dir: str
    image_api: str
    image_gateway: str

    def has(self, component):
        return component in self.components


def _get(table, key, kind, default, where):
    if key not in table:
        return default
    value = table[key]
    # bool is a subclass of int in Python; a port of `true` must not pass.
    if not isinstance(value, kind) or (kind is int and isinstance(value, bool)):
        raise ConfigError(f"{where}{key} must be {kind.__name__}, got {type(value).__name__}")
    return value


def _publish(value, where):
    """`PORT` or `IPv4:PORT`, as podman's PublishPort host side takes it."""
    host, _, port = value.rpartition(":")
    try:
        if host:
            ipaddress.IPv4Address(host)
        if not 1 <= int(port) <= 65535:
            raise ValueError
    except ValueError:
        raise ConfigError(f"{where} must be PORT or IPv4:PORT, got {value!r}") from None
    return value


def _absolute(value, where):
    if not value.startswith("/") or "\n" in value:
        raise ConfigError(f"{where} must be an absolute path, got {value!r}")
    return value.rstrip("/") or "/"


def _image(value, where):
    if not value or any(c.isspace() for c in value):
        raise ConfigError(f"{where} must be an image reference, got {value!r}")
    return value


def load(path):
    try:
        with open(path, "rb") as f:
            raw = tomllib.load(f)
    except FileNotFoundError:
        raise ConfigError("no such file — create it with: deploy.sh init <role>") from None
    except tomllib.TOMLDecodeError as err:
        raise ConfigError(str(err)) from None

    for key, value in raw.items():
        if isinstance(value, dict):
            if key not in TABLES:
                raise ConfigError(f"unknown table [{key}]")
            unknown = set(value) - TABLES[key][1]
            if unknown:
                raise ConfigError(f"unknown key(s) in [{key}]: {', '.join(sorted(unknown))}")
        elif key not in TOP_LEVEL:
            raise ConfigError(f"unknown key {key!r}")

    components = raw.get("components")
    if not isinstance(components, list) or not components:
        raise ConfigError(f"components must be a non-empty list of {', '.join(COMPONENTS)}")
    for c in components:
        if c not in COMPONENTS:
            raise ConfigError(f"unknown component {c!r} (expected one of {', '.join(COMPONENTS)})")
    if len(set(components)) != len(components):
        raise ConfigError("components lists a component twice")

    has = set(components).__contains__

    # A local broker today serves this host's own api and gateway. As the
    # central broker for remote sites it would need their users and ACL lines,
    # and as a remote site's spool it needs the bridge — both deferred, see
    # docs/design/deployment-topology.md.
    if has("broker") and set(components) != set(COMPONENTS):
        raise ConfigError(
            'a local "broker" currently needs "api" and "gateway" beside it '
            "(the all-in-one shape); the central-broker and bridge shapes are not built yet"
        )

    for table, (owner, _) in TABLES.items():
        if table in raw and owner and not has(owner):
            raise ConfigError(f"[{table}] has no effect without the {owner!r} component")

    site = _get(raw, "site", str, "", "")
    if has("gateway"):
        if not site:
            raise ConfigError("site is required with the gateway component")
        if not SITE_RE.match(site):
            raise ConfigError(f"site {site!r} may only contain a-z, 0-9 and '-'")
    elif site:
        raise ConfigError("site has no effect without the gateway component")

    data_dir = _absolute(_get(raw, "data_dir", str, DEFAULT_DATA_DIR, ""), "data_dir")

    mqtt = raw.get("mqtt", {})
    if has("broker"):
        if mqtt:
            raise ConfigError("omit [mqtt] when this host runs its own broker")
        mqtt_host, mqtt_port = "homescope-mqtt", 1883
        api_user, gateway_user = "homescope-api", f"homescope-{site}"
    else:
        mqtt_host = _get(mqtt, "host", str, "", "mqtt.")
        if not mqtt_host:
            raise ConfigError('[mqtt] host is required unless "broker" is a component')
        mqtt_port = _get(mqtt, "port", int, 1883, "mqtt.")
        if not 1 <= mqtt_port <= 65535:
            raise ConfigError(f"mqtt.port out of range: {mqtt_port}")
        api_user = _get(mqtt, "api_user", str, "homescope-api", "mqtt.")
        gateway_user = _get(mqtt, "gateway_user", str, f"homescope-{site}", "mqtt.")
        if "api_user" in mqtt and not has("api"):
            raise ConfigError("mqtt.api_user has no effect without the api component")
        if "gateway_user" in mqtt and not has("gateway"):
            raise ConfigError("mqtt.gateway_user has no effect without the gateway component")

    api = raw.get("api", {})
    grafana = raw.get("grafana", {})
    backup = raw.get("backup", {})
    images = raw.get("images", {})
    for name in images:
        if not has(name):
            raise ConfigError(f"images.{name} has no effect without the {name!r} component")

    return Config(
        components=[c for c in COMPONENTS if has(c)],
        site=site,
        data_dir=data_dir,
        broker_local=has("broker"),
        mqtt_host=mqtt_host,
        mqtt_port=mqtt_port,
        mqtt_api_user=api_user,
        mqtt_gateway_user=gateway_user,
        api_publish=_publish(_get(api, "publish", str, "127.0.0.1:4001", "api."), "api.publish"),
        db_publish=_publish(_get(api, "db_publish", str, "127.0.0.1:5432", "api."), "api.db_publish"),
        grafana_enabled=_get(grafana, "enabled", bool, True, "grafana."),
        grafana_publish=_publish(_get(grafana, "publish", str, "4000", "grafana."), "grafana.publish"),
        grafana_root_url=_get(grafana, "root_url", str, "", "grafana."),
        grafana_allow_embedding=_get(grafana, "allow_embedding", bool, False, "grafana."),
        backup_dir=_absolute(_get(backup, "dir", str, f"{data_dir}/backups", "backup."), "backup.dir"),
        image_api=_image(_get(images, "api", str, DEFAULT_IMAGES["api"], "images."), "images.api"),
        image_gateway=_image(
            _get(images, "gateway", str, DEFAULT_IMAGES["gateway"], "images."), "images.gateway"
        ),
    )


def shell(config):
    lines = []
    for field in fields(config):
        value = getattr(config, field.name)
        if isinstance(value, bool):
            value = "true" if value else "false"
        elif isinstance(value, list):
            value = " ".join(value)
        lines.append(f"CFG_{field.name.upper()}={shlex.quote(str(value))}")
    for c in COMPONENTS:
        lines.append(f"CFG_HAS_{c.upper()}={'true' if config.has(c) else 'false'}")
    return "\n".join(lines)


def show(config):
    width = max(len(f.name) for f in fields(config))
    return "\n".join(f"{f.name:<{width}}  {getattr(config, f.name)}" for f in fields(config))


def main(argv):
    args = argv[1:]
    as_show = args[:1] == ["--show"]
    if as_show:
        args = args[1:]
    if len(args) != 1:
        print("\n".join(__doc__.strip().splitlines()[2:4]), file=sys.stderr)
        return 2
    path = args[0]
    try:
        config = load(path)
    except ConfigError as err:
        print(f"{path}: {err}", file=sys.stderr)
        return 1
    print(show(config) if as_show else shell(config))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
