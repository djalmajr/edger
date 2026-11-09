#!/usr/bin/env python3
"""Run the INF-34 rate-limit consumer against a pinned Traefik container.

This is deliberately an explicit integration harness, not a unittest.  The
``if __name__`` guard keeps normal ``unittest discover`` runs from starting
Docker containers. The harness reads the supplied EdgeR manifest and either
its explicit local peer fixture or an optional allowlist manifest, then builds
only the local test router around those objects. It never pulls an image or
talks to a cluster or Cloudflare.
"""

from __future__ import annotations

import argparse
import copy
import hashlib
import http.client
import json
import re
import socket
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path
from typing import Any, Iterable


IMAGE = (
    "traefik@sha256:"
    "321153f6f732f912cf5c6f0597fe38743fc7895d482d7d8804a479ae0403e151"
)
IMAGE_DIGEST = "sha256:321153f6f732f912cf5c6f0597fe38743fc7895d482d7d8804a479ae0403e151"
OWNER_LABEL = "infra.inf34.run"
MANIFEST_LABEL = "infra.inf34.manifest-sha256"
TRAEFIK_CONFIG_TARGET = "/etc/traefik"
CONTAINER_PORT = 8080
UNTRUSTED_PEER_IP = "127.0.0.1"
PROBE_PATH = "/rate-test"


class HarnessError(RuntimeError):
    """A missing prerequisite or a failed runtime assertion."""


def run_command(args: list[str], *, timeout: float = 30.0) -> subprocess.CompletedProcess[str]:
    """Run a command without invoking a shell or exposing the process environment."""

    return subprocess.run(
        args,
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )


def require_yaml() -> Any:
    try:
        import yaml  # type: ignore
        return yaml
    except ImportError as exc:  # pragma: no cover - depends on the host image
        ruby = run_command(["ruby", "-e", "print RUBY_VERSION"], timeout=5)
        if ruby.returncode != 0:
            raise HarnessError(
                "YAML parsing needs PyYAML or the Ruby standard-library yaml module; "
                "install nothing for this harness."
            ) from exc

        class RubyYamlCompat:
            YAMLError = ValueError

            @staticmethod
            def safe_load_all(text: str) -> list[dict[str, Any]]:
                result = subprocess.run(
                    ["ruby", "-ryaml", "-rjson", "-e", "puts JSON.generate(YAML.load_stream(STDIN.read))"],
                    input=text,
                    capture_output=True,
                    text=True,
                    timeout=10,
                    check=False,
                )
                if result.returncode != 0:
                    raise ValueError(result.stderr.strip())
                return json.loads(result.stdout)

            @staticmethod
            def safe_dump(value: Any, *, sort_keys: bool = False) -> str:
                del sort_keys
                encoded = json.dumps(value, separators=(",", ":"))
                result = subprocess.run(
                    ["ruby", "-ryaml", "-rjson", "-e", "puts YAML.dump(JSON.parse(STDIN.read))"],
                    input=encoded,
                    capture_output=True,
                    text=True,
                    timeout=10,
                    check=False,
                )
                if result.returncode != 0:
                    raise ValueError(result.stderr.strip())
                return result.stdout

            @staticmethod
            def safe_dump_all(values: list[Any], *, sort_keys: bool = False) -> str:
                del sort_keys
                encoded = json.dumps(values, separators=(",", ":"))
                result = subprocess.run(
                    ["ruby", "-ryaml", "-rjson", "-e", "puts YAML.dump_stream(*JSON.parse(STDIN.read))"],
                    input=encoded,
                    capture_output=True,
                    text=True,
                    timeout=10,
                    check=False,
                )
                if result.returncode != 0:
                    raise ValueError(result.stderr.strip())
                return result.stdout

        return RubyYamlCompat


def load_documents(path: Path, yaml: Any) -> list[dict[str, Any]]:
    try:
        documents = list(yaml.safe_load_all(path.read_text(encoding="utf-8")))
    except (OSError, yaml.YAMLError) as exc:
        raise HarnessError(f"cannot parse YAML manifest {path}: {exc}") from exc
    if not documents or any(not isinstance(doc, dict) for doc in documents):
        raise HarnessError(f"manifest {path} must contain non-empty YAML objects")
    return documents


def document_by(documents: Iterable[dict[str, Any]], kind: str, name: str, namespace: str) -> dict[str, Any]:
    for document in documents:
        metadata = document.get("metadata") or {}
        if (
            document.get("kind") == kind
            and metadata.get("name") == name
            and metadata.get("namespace") == namespace
        ):
            return document
    raise HarnessError(f"manifest is missing {kind} {namespace}/{name}")


def source_fingerprint(path: Path) -> dict[str, str]:
    contents = path.read_bytes()
    result = {"sha256": hashlib.sha256(contents).hexdigest()}
    git = run_command(["git", "-C", str(path.parent.parent.parent), "rev-parse", "HEAD"])
    if git.returncode == 0:
        result["head"] = git.stdout.strip()
    diff = run_command(
        ["git", "-C", str(path.parent.parent.parent), "diff", "--no-ext-diff", "--binary", "--", str(path.relative_to(path.parent.parent.parent))]
    )
    if diff.returncode == 0:
        result["diff_sha256"] = hashlib.sha256(diff.stdout.encode()).hexdigest()
    return result


def validate_proposed_manifest(
    manifest_path: Path, yaml: Any, mutation: str
) -> tuple[dict[str, Any], dict[str, Any], list[str], dict[str, Any]]:
    documents = load_documents(manifest_path, yaml)
    legacy = document_by(documents, "Middleware", "edger-admin-ratelimit", "edger")
    cloudflare = document_by(
        documents, "Middleware", "edger-admin-ratelimit-cloudflare", "edger"
    )
    route = document_by(documents, "IngressRoute", "edger", "edger")

    legacy_rate = (legacy.get("spec") or {}).get("rateLimit") or {}
    if legacy_rate != {"average": 20, "period": "1s", "burst": 100}:
        raise HarnessError(
            "legacy edger-admin-ratelimit changed; expected the untouched 20/1s/100 object"
        )
    cloudflare_rate = (cloudflare.get("spec") or {}).get("rateLimit") or {}
    expected_rate = {
        "average": 20,
        "period": "1s",
        "burst": 100,
        "sourceCriterion": {"requestHeaderName": "CF-Connecting-IP"},
    }
    if mutation == "remove-source-criterion":
        # The caller materializes this as a temporary manifest copy.  Accept
        # both an already-removed criterion and the in-memory guard used by
        # older callers, but never alter the source checkout.
        if cloudflare_rate.get("sourceCriterion") == expected_rate["sourceCriterion"]:
            cloudflare_rate = dict(cloudflare_rate)
            cloudflare_rate.pop("sourceCriterion", None)
        elif "sourceCriterion" in cloudflare_rate:
            raise HarnessError("source-criterion mutation has an unexpected value")
    elif cloudflare_rate != expected_rate:
        raise HarnessError(
            "proposed middleware must preserve average/period/burst and use "
            "sourceCriterion.requestHeaderName=CF-Connecting-IP"
        )
    if cloudflare_rate.get("average") != 20 or cloudflare_rate.get("period") != "1s" or cloudflare_rate.get("burst") != 100:
        raise HarnessError("rate-limit mutation changed 20/1s/100, which the contract forbids")

    routes = ((route.get("spec") or {}).get("routes") or [])
    if len(routes) != 1:
        raise HarnessError("the control-plane IngressRoute must keep exactly one route")
    route_spec = routes[0]
    route_middlewares = [item.get("name") for item in route_spec.get("middlewares") or []]
    expected_chain = ["cloudflare-only", "edger-admin-ratelimit-cloudflare"]
    if mutation == "remove-cloudflare-only":
        if route_middlewares == expected_chain:
            route_middlewares = ["edger-admin-ratelimit-cloudflare"]
        elif route_middlewares != ["edger-admin-ratelimit-cloudflare"]:
            raise HarnessError("cloudflare-only mutation has an unexpected route chain")
    elif route_middlewares != expected_chain:
        raise HarnessError(
            f"route middleware chain must be {expected_chain!r}, got {route_middlewares!r}"
        )
    services = route_spec.get("services") or []
    if len(services) != 1 or services[0].get("name") != "edger" or services[0].get("port") != 3000:
        raise HarnessError("the original route service and port must remain edger:3000")
    return legacy_rate, cloudflare_rate, route_middlewares, route


def materialize_mutation(manifest_path: Path, yaml: Any, mutation: str, target: Path) -> Path:
    """Write a mutated manifest copy outside every repository checkout."""

    documents = load_documents(manifest_path, yaml)
    if mutation == "remove-source-criterion":
        middleware = document_by(
            documents, "Middleware", "edger-admin-ratelimit-cloudflare", "edger"
        )
        rate = middleware["spec"]["rateLimit"]
        if "sourceCriterion" not in rate:
            raise HarnessError("source-criterion mutation target already lacks sourceCriterion")
        del rate["sourceCriterion"]
    elif mutation == "remove-cloudflare-only":
        route = document_by(documents, "IngressRoute", "edger", "edger")
        route["spec"]["routes"][0]["middlewares"] = [
            item
            for item in route["spec"]["routes"][0].get("middlewares", [])
            if item.get("name") != "cloudflare-only"
        ]
    else:
        raise HarnessError(f"unknown mutation {mutation}")
    mutated = target / "manifest-mutated.yaml"
    mutated.write_text(yaml.safe_dump_all(documents, sort_keys=False), encoding="utf-8")
    return mutated


def load_allowlist(
    allowlist_path: Path | None, allowlist_sha256: str | None, yaml: Any
) -> dict[str, Any]:
    if allowlist_path is None:
        if allowlist_sha256 is not None:
            raise HarnessError("--allowlist-sha256 requires --allowlist")
        # Public CI cannot depend on the private infra repository.  This
        # fixture deliberately models only the socket boundary; the runtime
        # replaces this placeholder with the owned Docker network gateway.
        document = {
            "kind": "Middleware",
            "metadata": {"name": "inf34-local-peer-fixture"},
            "spec": {"ipAllowList": {"sourceRange": ["PEER_PLACEHOLDER/32"]}},
        }
        source_sha256 = hashlib.sha256(
            json.dumps(document, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest()
    else:
        if allowlist_sha256 is None:
            raise HarnessError("--allowlist-sha256 is required with --allowlist")
        if len(allowlist_sha256) != 64 or any(
            char not in "0123456789abcdefABCDEF" for char in allowlist_sha256
        ):
            raise HarnessError("--allowlist-sha256 must be a 64-character hexadecimal SHA-256")
        source_sha256 = hashlib.sha256(allowlist_path.read_bytes()).hexdigest()
        if source_sha256.lower() != allowlist_sha256.lower():
            raise HarnessError(
                f"allowlist SHA-256 mismatch: expected {allowlist_sha256}, got {source_sha256}"
            )
        documents = load_documents(allowlist_path, yaml)
        if len(documents) != 1:
            raise HarnessError(f"allowlist must contain one document, got {len(documents)}")
        document = documents[0]
        metadata = document.get("metadata") or {}
        if document.get("kind") != "Middleware" or metadata.get("name") != "cloudflare-only":
            raise HarnessError("allowlist must be the cloudflare-only Middleware")
        if metadata.get("namespace") not in (None, "edger"):
            raise HarnessError("allowlist namespace must be absent or edger")
    source_spec = document.get("spec") or {}
    if not isinstance(source_spec, dict):
        raise HarnessError("allowlist spec must be an object")
    allowlist = source_spec.get("ipAllowList") or {}
    if not isinstance(allowlist, dict):
        raise HarnessError("allowlist ipAllowList must be an object")
    original_ranges = allowlist.get("sourceRange")
    if not isinstance(original_ranges, list) or not original_ranges:
        raise HarnessError("allowlist has no sourceRange")
    if "ipStrategy" in allowlist:
        raise HarnessError("cloudflare-only allowlist must not use ipStrategy")
    adapted_spec = copy.deepcopy(source_spec)
    adapted_allowlist = copy.deepcopy(allowlist)
    adapted_allowlist["sourceRange"] = ["PEER_PLACEHOLDER/32"]
    adapted_spec["ipAllowList"] = adapted_allowlist
    return {
        "source_name": document.get("metadata", {}).get("name", "<unnamed>"),
        "original_range_count": len(original_ranges),
        "original_ranges_sha256": hashlib.sha256(
            json.dumps(original_ranges, separators=(",", ":")).encode()
        ).hexdigest(),
        "source_sha256": source_sha256,
        "middleware": adapted_spec,
    }


def choose_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def write_configs(
    directory: Path,
    *,
    port: int,
    legacy_rate: dict[str, Any],
    cloudflare_rate: dict[str, Any],
    route_middlewares: list[str],
    allowlist: dict[str, Any],
) -> None:
    static = {
        "entryPoints": {"web": {"address": f":{CONTAINER_PORT}"}},
        "ping": {"manualRouting": True},
        "providers": {
            "file": {
                "filename": f"{TRAEFIK_CONFIG_TARGET}/dynamic.yaml",
                "watch": False,
            }
        },
        "log": {"level": "ERROR"},
        "accessLog": {"format": "json", "filePath": "/dev/stdout"},
    }
    dynamic = {
        "http": {
            "middlewares": {
                "cloudflare-only": allowlist["middleware"],
                "edger-admin-ratelimit": {"rateLimit": legacy_rate},
                "edger-admin-ratelimit-cloudflare": {"rateLimit": cloudflare_rate},
            },
            "routers": {
                "inf34-rate-test": {
                    "rule": f"PathPrefix(`{PROBE_PATH}`)",
                    "entryPoints": ["web"],
                    "middlewares": route_middlewares,
                    "service": "ping@internal",
                }
            },
        }
    }
    yaml = require_yaml()
    (directory / "traefik.yml").write_text(yaml.safe_dump(static, sort_keys=False), encoding="utf-8")
    (directory / "dynamic.yaml").write_text(yaml.safe_dump(dynamic, sort_keys=False), encoding="utf-8")


def docker_inspect(container: str) -> dict[str, Any]:
    result = run_command(["docker", "inspect", container])
    if result.returncode != 0:
        raise HarnessError(f"docker inspect failed for owned container {container}: {result.stderr.strip()}")
    try:
        return json.loads(result.stdout)[0]
    except (ValueError, IndexError) as exc:
        raise HarnessError(f"invalid docker inspect output for {container}") from exc


def check_image() -> dict[str, str]:
    daemon = run_command(["docker", "version", "--format", "{{.Server.Version}}"])
    if daemon.returncode != 0:
        raise HarnessError(f"Docker daemon unavailable: {daemon.stderr.strip()}")
    inspect = run_command(["docker", "image", "inspect", IMAGE])
    if inspect.returncode != 0:
        raise HarnessError(
            f"required image is not cached ({IMAGE}); refusing to pull/install: {inspect.stderr.strip()}"
        )
    try:
        metadata = json.loads(inspect.stdout)[0]
    except (ValueError, IndexError) as exc:
        raise HarnessError("docker image inspect did not return JSON") from exc
    repo_digests = metadata.get("RepoDigests") or []
    if not any(IMAGE_DIGEST in value for value in repo_digests):
        raise HarnessError(f"cached image digest mismatch: {repo_digests!r}")
    version = run_command(["docker", "run", "--rm", "--network", "none", IMAGE, "version"])
    if version.returncode != 0:
        raise HarnessError(f"pinned Traefik version command failed: {version.stderr.strip()}")
    if "Version:      3.7.17" not in version.stdout and "v3.7.17" not in version.stdout:
        raise HarnessError(f"pinned image did not report Traefik v3.7.17: {version.stdout.strip()!r}")
    wget = run_command(
        ["docker", "run", "--rm", "--network", "none", "--entrypoint", "/usr/bin/wget", IMAGE, "--help"]
    )
    if wget.returncode != 0:
        raise HarnessError(
            "the pinned image must provide wget for the direct-peer mutation; "
            f"no dependency may be installed: {wget.stderr.strip()}"
        )
    return {
        "daemon": daemon.stdout.strip(),
        "image": IMAGE,
        "image_id": metadata.get("Id", ""),
        "architecture": metadata.get("Architecture", ""),
        "version_output": " ".join(version.stdout.split()),
        "wget": "available",
    }


def request_status(
    port: int,
    *,
    source_ip: str,
    headers: list[tuple[str, str]] | None = None,
) -> int:
    connection = http.client.HTTPConnection(
        "127.0.0.1", port, timeout=2.0, source_address=(source_ip, 0)
    )
    try:
        connection.putrequest("GET", PROBE_PATH, skip_accept_encoding=True, skip_host=True)
        connection.putheader("Host", "inf34.local")
        for name, value in headers or []:
            connection.putheader(name, value)
        connection.endheaders()
        response = connection.getresponse()
        response.read()
        return int(response.status)
    finally:
        connection.close()


def request_status_from_container(container: str, headers: list[tuple[str, str]] | None = None) -> int:
    args = [
        "docker",
        "exec",
        container,
        "wget",
        "-q",
        "-S",
        "-O",
        "/dev/null",
        "-T",
        "2",
    ]
    for name, value in headers or []:
        args.extend(("--header", f"{name}: {value}"))
    args.append(f"http://127.0.0.1:{CONTAINER_PORT}{PROBE_PATH}")
    result = run_command(args, timeout=5)
    response = result.stdout + result.stderr
    matches = re.findall(r"HTTP/\d(?:\.\d)?\s+(\d{3})", response)
    if not matches:
        raise HarnessError(
            "docker exec wget did not expose an HTTP status; the pinned image "
            f"may not provide wget (exit {result.returncode}): {response[-500:]}"
        )
    return int(matches[-1])


def consume_until_rate_limited(port: int, source_ip: str, header_value: str) -> tuple[int, int, list[int]]:
    statuses: list[int] = []
    deadline = time.monotonic() + 8.0
    while time.monotonic() < deadline and len(statuses) < 180:
        statuses.append(
            request_status(
                port,
                source_ip=source_ip,
                headers=[("CF-Connecting-IP", header_value)],
            )
        )
        if statuses[-1] == 429:
            break
    if 429 not in statuses:
        raise HarnessError(
            f"rate limiter did not return 429 after {len(statuses)} requests within 8s"
        )
    return statuses.index(429) + 1, statuses[-1], statuses


def access_log_peers(container: str) -> list[str]:
    logs = run_command(["docker", "logs", container], timeout=10)
    if logs.returncode != 0:
        return []
    peers: set[str] = set()
    for line in logs.stdout.splitlines():
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        value = entry.get("ClientAddr") or entry.get("clientAddr")
        if value:
            peers.add(str(value))
    return sorted(peers)


class OwnedNetwork:
    """Create one labelled bridge and remove it only after label proof."""

    def __init__(self, run_id: str):
        self.run_id = run_id
        self.name = f"infra-inf34-ratelimit-{run_id[:12]}"
        self.network_id: str | None = None
        self.gateway: str | None = None

    def start(self) -> None:
        result = run_command(
            [
                "docker",
                "network",
                "create",
                "--label",
                f"{OWNER_LABEL}={self.run_id}",
                "--label",
                "infra.inf34.purpose=rate-limit-test",
                self.name,
            ],
            timeout=30,
        )
        if result.returncode != 0:
            raise HarnessError(f"docker network create failed: {result.stderr.strip()}")
        self.network_id = result.stdout.strip()
        metadata = self.inspect()
        labels = metadata.get("Labels") or {}
        if labels.get(OWNER_LABEL) != self.run_id:
            raise HarnessError("owned network label proof failed")
        configs = ((metadata.get("IPAM") or {}).get("Config") or [])
        gateways = [item.get("Gateway") for item in configs if item.get("Gateway")]
        if len(gateways) != 1:
            raise HarnessError(f"owned network has no single IPv4 gateway: {configs!r}")
        self.gateway = str(gateways[0])
        if ":" in self.gateway:
            raise HarnessError(f"owned network chose a non-IPv4 gateway: {self.gateway}")

    def inspect(self) -> dict[str, Any]:
        if not self.network_id:
            raise HarnessError("network was not created")
        result = run_command(["docker", "network", "inspect", self.network_id])
        if result.returncode != 0:
            raise HarnessError(f"docker network inspect failed: {result.stderr.strip()}")
        try:
            return json.loads(result.stdout)[0]
        except (ValueError, IndexError) as exc:
            raise HarnessError("invalid docker network inspect output") from exc

    def cleanup(self) -> None:
        if not self.network_id:
            return
        metadata = self.inspect()
        labels = metadata.get("Labels") or {}
        if labels.get(OWNER_LABEL) != self.run_id:
            raise HarnessError("refusing network cleanup: ownership label does not match")
        removed = run_command(["docker", "network", "rm", self.network_id], timeout=30)
        if removed.returncode != 0:
            raise HarnessError(f"owned network cleanup failed: {removed.stderr.strip()}")


class OwnedContainer:
    """Own exactly one labelled container and remove it only after label proof."""

    def __init__(self, directory: Path, port: int, manifest_sha256: str, network: OwnedNetwork, run_id: str):
        self.directory = directory
        self.port = port
        self.network = network
        self.token = run_id
        self.name = f"infra-inf34-ratelimit-{run_id[:12]}"
        self.container_id: str | None = None
        self.manifest_sha256 = manifest_sha256

    def start(self) -> None:
        result = run_command(
            [
                "docker",
                "create",
                "--name",
                self.name,
                "--label",
                f"{OWNER_LABEL}={self.token}",
                "--label",
                f"{MANIFEST_LABEL}={self.manifest_sha256}",
                "--read-only",
                "--security-opt",
                "no-new-privileges:true",
                "--cap-drop",
                "ALL",
                "--network",
                self.network.name,
                "--publish",
                f"127.0.0.1:{self.port}:{CONTAINER_PORT}/tcp",
                "--mount",
                f"type=bind,source={self.directory},target={TRAEFIK_CONFIG_TARGET},readonly",
                IMAGE,
                "--configFile=/etc/traefik/traefik.yml",
            ],
            timeout=30,
        )
        if result.returncode != 0:
            raise HarnessError(f"docker create failed: {result.stderr.strip()}")
        self.container_id = result.stdout.strip()
        metadata = docker_inspect(self.container_id)
        labels = (metadata.get("Config") or {}).get("Labels") or {}
        if labels.get(OWNER_LABEL) != self.token or labels.get(MANIFEST_LABEL) != self.manifest_sha256:
            raise HarnessError("owned container label proof failed before start")
        started = run_command(["docker", "start", self.container_id], timeout=30)
        if started.returncode != 0:
            raise HarnessError(f"docker start failed: {started.stderr.strip()}")

    def wait_ready(self) -> None:
        if not self.container_id:
            raise HarnessError("container was not created")
        deadline = time.monotonic() + 15.0
        last_status: int | None = None
        while time.monotonic() < deadline:
            metadata = docker_inspect(self.container_id)
            state = metadata.get("State") or {}
            if state.get("Status") != "running":
                logs = run_command(["docker", "logs", self.container_id], timeout=10)
                raise HarnessError(
                    f"Traefik container exited before readiness ({state.get('ExitCode')}): "
                    f"{logs.stdout[-2000:]}{logs.stderr[-2000:]}"
                )
            try:
                last_status = request_status(self.port, source_ip="127.0.0.1", headers=[])
            except (OSError, http.client.HTTPException):
                time.sleep(0.1)
                continue
            if last_status in (200, 403):
                return
            time.sleep(0.1)
        logs = run_command(["docker", "logs", self.container_id], timeout=10)
        raise HarnessError(
            f"Traefik did not become ready within 15s (last status {last_status}); "
            f"logs={logs.stdout[-2000:]}{logs.stderr[-2000:]}"
        )

    def cleanup(self) -> None:
        if not self.container_id:
            return
        try:
            metadata = docker_inspect(self.container_id)
        except HarnessError:
            return
        labels = (metadata.get("Config") or {}).get("Labels") or {}
        if labels.get(OWNER_LABEL) != self.token:
            raise HarnessError("refusing cleanup: container ownership label no longer matches")
        removed = run_command(["docker", "rm", "-f", self.container_id], timeout=30)
        if removed.returncode != 0:
            raise HarnessError(f"owned container cleanup failed: {removed.stderr.strip()}")


def run_scenario(
    *,
    directory: Path,
    manifest_sha256: str,
    legacy_rate: dict[str, Any],
    cloudflare_rate: dict[str, Any],
    route_middlewares: list[str],
    allowlist: dict[str, Any],
    mutation: str,
) -> dict[str, Any]:
    run_id = uuid.uuid4().hex
    network = OwnedNetwork(run_id)
    port = choose_port()
    try:
        network.start()
        if not network.gateway:
            raise HarnessError("owned network did not expose a gateway")
        scenario_allowlist = dict(allowlist)
        scenario_middleware = copy.deepcopy(allowlist["middleware"])
        scenario_middleware["ipAllowList"]["sourceRange"] = [f"{network.gateway}/32"]
        scenario_allowlist["middleware"] = scenario_middleware
        write_configs(
            directory,
            port=port,
            legacy_rate=legacy_rate,
            cloudflare_rate=cloudflare_rate,
            route_middlewares=route_middlewares,
            allowlist=scenario_allowlist,
        )
        container = OwnedContainer(directory, port, manifest_sha256, network, run_id)
        container.start()
        container.wait_ready()
        trusted_status = request_status(port, source_ip="127.0.0.1", headers=[("CF-Connecting-IP", "198.51.100.250")])
        if trusted_status != 200:
            raise HarnessError(
                f"the published host peer did not match the adapted network-gateway allowlist "
                f"{network.gateway}/32 (status {trusted_status})"
            )
        observed_peers = access_log_peers(container.container_id or container.name)
        edge_cases = {}
        for label, case_headers in (
            ("missing", []),
            ("invalid", [("CF-Connecting-IP", "not-an-ip")]),
            (
                "multiple",
                [("CF-Connecting-IP", "198.51.100.12"), ("CF-Connecting-IP", "198.51.100.13")],
            ),
            ("comma", [("CF-Connecting-IP", "198.51.100.14, 198.51.100.15")]),
        ):
            edge_cases[label] = {
                "raw_headers": case_headers,
                "status": request_status(port, source_ip="127.0.0.1", headers=case_headers),
            }
        ipv4 = consume_until_rate_limited(port, "127.0.0.1", "198.51.100.10")
        ipv4_b = request_status(
            port, source_ip="127.0.0.1", headers=[("CF-Connecting-IP", "198.51.100.11")]
        )
        # IPv6 values exercise the same peer and the same request-header
        # extractor; the peer itself remains the explicitly allowlisted local
        # Docker bridge gateway for a portable local experiment.
        ipv6 = consume_until_rate_limited(port, "127.0.0.1", "2001:db8::a")
        ipv6_b = request_status(
            port, source_ip="127.0.0.1", headers=[("CF-Connecting-IP", "2001:db8::b")]
        )
        direct = request_status_from_container(
            container.container_id or container.name,
            headers=[
                ("CF-Connecting-IP", "198.51.100.99"),
                ("X-Forwarded-For", "198.51.100.99"),
            ],
        )
        return {
            "mutation": mutation or "none",
            "port": port,
            "peer": network.gateway,
            "untrusted_peer": UNTRUSTED_PEER_IP,
            "edge_cases": edge_cases,
            "ipv4": {"requests_to_429": ipv4[0], "a_last_status": ipv4[1], "b_status": ipv4_b},
            "ipv6": {"requests_to_429": ipv6[0], "a_last_status": ipv6[1], "b_status": ipv6_b},
            "direct_spoof_status": direct,
            "access_log_peers": observed_peers,
            "peer_observation": f"Docker network IPAM gateway {network.gateway}; trusted host status {trusted_status}",
            "network_id": network.network_id,
            "container_id": container.container_id,
            "cleanup": "owned container and network removed after label proof",
        }
    finally:
        try:
            if "container" in locals():
                container.cleanup()
        finally:
            network.cleanup()


def assert_corrected(result: dict[str, Any]) -> None:
    if result["ipv4"]["a_last_status"] != 429 or result["ipv4"]["b_status"] != 200:
        raise HarnessError(f"corrected IPv4 bucket isolation failed: {result['ipv4']}")
    if result["ipv6"]["a_last_status"] != 429 or result["ipv6"]["b_status"] != 200:
        raise HarnessError(f"corrected IPv6 bucket isolation failed: {result['ipv6']}")
    if result["direct_spoof_status"] != 403:
        raise HarnessError(
            f"direct peer with forged headers was not rejected before the backend: {result['direct_spoof_status']}"
        )


def print_result(
    *,
    image: dict[str, str],
    source: dict[str, str],
    allowlist: dict[str, Any],
    result: dict[str, Any],
    mutation: str,
    status: str,
) -> None:
    print(f"INF-34 Traefik integration: {status}")
    print(f"image={image['image']} daemon={image['daemon']} architecture={image['architecture']}")
    print(f"version={image['version_output']}")
    print(f"source_head={source.get('head', '<unavailable>')} source_sha256={source['sha256']}")
    print(f"peer={result['peer']} allowlist_source={allowlist['source_name']} adapted_range={result['peer']}/32")
    print(f"allowlist_original_range_count={allowlist['original_range_count']} allowlist_ranges_sha256={allowlist['original_ranges_sha256']} allowlist_source_sha256={allowlist['source_sha256']}")
    print(f"peer_observation={result.get('peer_observation', '')}")
    print(f"resource_ids=container:{result.get('container_id')} network:{result.get('network_id')} cleanup={result.get('cleanup')}")
    print(f"mutation={mutation or 'none'} container_access_log_peers={result.get('access_log_peers', [])}")
    print(f"edge_cases={json.dumps(result['edge_cases'], sort_keys=True)}")
    print(f"ipv4={json.dumps(result['ipv4'], sort_keys=True)}")
    print(f"ipv6={json.dumps(result['ipv6'], sort_keys=True)}")
    print(f"direct_spoof_status={result['direct_spoof_status']}")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument(
        "--allowlist",
        type=Path,
        help="Optional cloudflare-only allowlist manifest; only its sourceRange is replaced by the test peer.",
    )
    parser.add_argument(
        "--allowlist-sha256",
        help="Required SHA-256 fingerprint when --allowlist is supplied.",
    )
    parser.add_argument(
        "--mutation",
        choices=("", "remove-source-criterion", "remove-cloudflare-only"),
        default="",
        help="Run an explicit copy-only negative mutation and report the expected failure.",
    )
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    yaml = require_yaml()
    manifest_path = args.manifest.resolve()
    allowlist_path = args.allowlist.resolve() if args.allowlist else None
    if not manifest_path.is_file() or (allowlist_path is not None and not allowlist_path.is_file()):
        raise HarnessError(f"manifest paths must be files: {manifest_path}, {allowlist_path}")
    # Validate the real proposed source first, then, for a negative case, use
    # a physically materialized temporary copy.  The consumer config therefore
    # reflects the mutated YAML rather than a test-only boolean shortcut.
    validate_proposed_manifest(manifest_path, yaml, "")
    with tempfile.TemporaryDirectory(prefix="infra-inf34-rate-manifest-") as mutation_temp:
        selected_manifest = manifest_path
        if args.mutation:
            selected_manifest = materialize_mutation(
                manifest_path, yaml, args.mutation, Path(mutation_temp)
            )
        legacy_rate, cloudflare_rate, route_middlewares, _route = validate_proposed_manifest(
            selected_manifest, yaml, args.mutation
        )
        allowlist = load_allowlist(allowlist_path, args.allowlist_sha256, yaml)
        image = check_image()
        source = source_fingerprint(manifest_path)
        manifest_sha256 = source["sha256"]

        # Each scenario gets its own config directory.  The manifest itself is
        # never copied into a repository or mutated; only the generated Docker
        # provider files are written below a private temporary directory.
        with tempfile.TemporaryDirectory(prefix="infra-inf34-rate-") as temp:
            result = run_scenario(
                directory=Path(temp),
                manifest_sha256=manifest_sha256,
                legacy_rate=legacy_rate,
                cloudflare_rate=cloudflare_rate,
                route_middlewares=route_middlewares,
                allowlist=allowlist,
                mutation=args.mutation,
            )
    try:
        assert_corrected(result)
    except HarnessError as exc:
        print_result(
            image=image,
            source=source,
            allowlist=allowlist,
            result=result,
            mutation=args.mutation,
            status=f"FAIL [contract] {exc}",
        )
        return 1
    print_result(
        image=image,
        source=source,
        allowlist=allowlist,
        result=result,
        mutation=args.mutation,
        status="PASS",
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main(sys.argv[1:]))
    except (HarnessError, OSError, subprocess.SubprocessError) as exc:
        print(f"INF-34 Traefik integration: FAIL [env/contract] {exc}", file=sys.stderr)
        raise SystemExit(1)

