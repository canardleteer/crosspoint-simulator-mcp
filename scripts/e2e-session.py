#!/usr/bin/env python3
"""Linux Session e2e against a live csm-proxy and firmware simulator.

Environment
  SIMULATOR_BIN   Required. Firmware `program` built with -DCROSSPOINT_SIM_GRPC
  CSM_PROXY_BIN   Default: <repo>/target/debug/crosspoint-simulator-mcp-proxy
  DISPLAY         Inherited. Automated mode uses SDL dummy video unless
                  --show-windows (avoids stealing the caller's display)
  LD_LIBRARY_PATH Caller may add a local grpc++ lib dir if rpath is not enough
  CSM_LISTEN      Default 127.0.0.1:50051
  CSM_MCP_HTTP    Default 127.0.0.1:8765

Modes
  (default)       Two instances, then missing-simulator and missing-server
  --human         Prompt for real key/click in the SDL window, then observe HUMAN
  --headless      One instance with --sim-headless; heartbeat, inject, snapshot
  --spawn         Proxy starts the simulator via start_instance (needs --simulator)
  --show-windows  Open real SDL windows during automated mode
  --keep          Leave proxy/simulator processes running on exit
"""

from __future__ import annotations

import argparse
import json
import os
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_PROXY = REPO_ROOT / "target" / "debug" / "crosspoint-simulator-mcp-proxy"
WINDOW_TITLE = "Simulator - XTEINK X4 (SSD1677)"


class Fail(Exception):
    pass


class Mcp:
    def __init__(self, url: str) -> None:
        self.url = url
        self.n = 0
        self.session: str | None = None
        self._init()

    def _post(self, body: dict[str, Any], timeout: float = 10.0) -> list[dict[str, Any]]:
        data = json.dumps(body).encode()
        headers = {
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
        }
        if self.session:
            headers["Mcp-Session-Id"] = self.session
        req = urllib.request.Request(self.url, data=data, headers=headers, method="POST")
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            if not self.session:
                self.session = resp.headers.get("Mcp-Session-Id")
            raw = resp.read().decode()
        payloads: list[dict[str, Any]] = []
        for line in raw.splitlines():
            if line.startswith("data: ") and line[6:].startswith("{"):
                payloads.append(json.loads(line[6:]))
        return payloads

    def _init(self) -> None:
        last: Exception | None = None
        for _ in range(50):
            try:
                self._post(
                    {
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "initialize",
                        "params": {
                            "protocolVersion": "2025-03-26",
                            "capabilities": {},
                            "clientInfo": {"name": "e2e-session", "version": "0"},
                        },
                    }
                )
                self._post({"jsonrpc": "2.0", "method": "notifications/initialized"})
                return
            except (urllib.error.URLError, TimeoutError, OSError) as err:
                last = err
                time.sleep(0.1)
        raise Fail(f"MCP initialize failed: {last}")

    def call(self, name: str, args: dict[str, Any] | None = None, timeout: float = 10.0) -> dict[str, Any]:
        self.n += 1
        payloads = self._post(
            {
                "jsonrpc": "2.0",
                "id": self.n + 10,
                "method": "tools/call",
                "params": {"name": name, "arguments": args or {}},
            },
            timeout=timeout,
        )
        for payload in payloads:
            if "result" in payload or "error" in payload:
                return payload
        raise Fail(f"{name}: no JSON-RPC result ({payloads!r})")

    def read_resource(self, uri: str, timeout: float = 10.0) -> dict[str, Any]:
        self.n += 1
        payloads = self._post(
            {
                "jsonrpc": "2.0",
                "id": self.n + 10,
                "method": "resources/read",
                "params": {"uri": uri},
            },
            timeout=timeout,
        )
        for payload in payloads:
            if "result" in payload or "error" in payload:
                return payload
        raise Fail(f"resources/read {uri}: no JSON-RPC result ({payloads!r})")


def tool_error(resp: dict[str, Any]) -> bool:
    if "error" in resp:
        return True
    result = resp.get("result") or {}
    return bool(result.get("isError"))


def tool_text(resp: dict[str, Any]) -> str:
    if "error" in resp:
        return json.dumps(resp["error"])
    content = (resp.get("result") or {}).get("content") or []
    for block in content:
        if block.get("type") == "text":
            return str(block.get("text") or "")
    return ""


def tool_json(resp: dict[str, Any]) -> Any:
    text = tool_text(resp)
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return text


def require(cond: bool, message: str) -> None:
    if not cond:
        raise Fail(message)


class Proc:
    def __init__(self, name: str, popen: subprocess.Popen[str], log_path: Path) -> None:
        self.name = name
        self.popen = popen
        self.log_path = log_path

    @property
    def pid(self) -> int:
        return self.popen.pid

    def alive(self) -> bool:
        return self.popen.poll() is None

    def log_text(self) -> str:
        try:
            return self.log_path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            return ""

    def terminate(self) -> None:
        if not self.alive():
            return
        self.popen.send_signal(signal.SIGTERM)
        try:
            self.popen.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.popen.kill()
            self.popen.wait(timeout=3)


class Harness:
    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        self.keep = args.keep
        self.listen = os.environ.get("CSM_LISTEN", "127.0.0.1:50051")
        self.mcp_http = os.environ.get("CSM_MCP_HTTP", "127.0.0.1:8765")
        self.mcp_url = f"http://{self.mcp_http}/mcp"
        self.proxy_bin = Path(os.environ.get("CSM_PROXY_BIN", str(DEFAULT_PROXY)))
        sim = os.environ.get("SIMULATOR_BIN")
        if not sim:
            raise Fail("SIMULATOR_BIN is required")
        self.sim_bin = Path(sim)
        if not self.proxy_bin.is_file():
            raise Fail(f"CSM_PROXY_BIN not found: {self.proxy_bin}")
        if not self.sim_bin.is_file():
            raise Fail(f"SIMULATOR_BIN not found: {self.sim_bin}")
        self.log_dir = Path(args.log_dir) if args.log_dir else Path(os.environ.get("TMPDIR", "/tmp"))
        self.procs: list[Proc] = []
        self.proxy: Proc | None = None
        self.mcp: Mcp | None = None

    def spawn(self, name: str, argv: list[str], extra_env: dict[str, str] | None = None) -> Proc:
        log_path = self.log_dir / f"e2e-session-{name}-{os.getpid()}.log"
        log_file = log_path.open("w", encoding="utf-8")
        env = os.environ.copy()
        if extra_env:
            env.update(extra_env)
        popen = subprocess.Popen(
            argv,
            stdout=log_file,
            stderr=subprocess.STDOUT,
            env=env,
            start_new_session=True,
        )
        log_file.close()
        proc = Proc(name, popen, log_path)
        self.procs.append(proc)
        return proc

    def start_proxy(self) -> Proc:
        extra_env: dict[str, str] = {}
        if self.args.spawn and not self.args.show_windows:
            extra_env["SDL_VIDEODRIVER"] = "dummy"
        argv = [
            str(self.proxy_bin),
            "--listen",
            self.listen,
            "--mcp-http",
            self.mcp_http,
        ]
        if self.args.spawn:
            argv.extend(["--simulator", str(self.sim_bin)])
        proc = self.spawn("proxy", argv, extra_env=extra_env or None)
        self.proxy = proc
        self.mcp = Mcp(self.mcp_url)
        return proc

    def start_sim(self, instance_id: str, *, headless: bool = False) -> Proc:
        extra_env: dict[str, str] = {}
        if not self.args.show_windows and not self.args.human:
            extra_env["SDL_VIDEODRIVER"] = "dummy"
        argv = [
            str(self.sim_bin),
            "--sim-grpc",
            "--sim-grpc-addr",
            self.listen,
            "--sim-instance-id",
            instance_id,
        ]
        if headless:
            argv.append("--sim-headless")
        return self.spawn(instance_id, argv, extra_env=extra_env or None)

    def reconnect_mcp(self) -> Mcp:
        self.mcp = Mcp(self.mcp_url)
        return self.mcp

    def call(self, name: str, args: dict[str, Any] | None = None, timeout: float = 10.0) -> dict[str, Any]:
        if self.mcp is None:
            raise Fail("MCP client is not connected")
        return self.mcp.call(name, args, timeout=timeout)

    def read_resource(self, uri: str) -> Any:
        if self.mcp is None:
            raise Fail("MCP client is not connected")
        resp = self.mcp.read_resource(uri)
        if "error" in resp:
            raise Fail(f"resources/read {uri}: {json.dumps(resp['error'])}")
        contents = (resp.get("result") or {}).get("contents") or []
        for block in contents:
            text = block.get("text")
            if text:
                try:
                    return json.loads(text)
                except json.JSONDecodeError:
                    return text
        raise Fail(f"resources/read {uri}: no text ({resp!r})")

    def wait_instances(self, expected: set[str], timeout: float = 15.0) -> list[dict[str, Any]]:
        deadline = time.time() + timeout
        last: list[dict[str, Any]] = []
        while time.time() < deadline:
            resp = self.call("list_instances")
            require(not tool_error(resp), f"list_instances failed: {tool_text(resp)}")
            body = tool_json(resp)
            last = list(body.get("instances") or [])
            ids = {item.get("instanceId") for item in last}
            if expected <= ids:
                return last
            time.sleep(0.2)
        raise Fail(f"timed out waiting for {sorted(expected)}; have {[i.get('instanceId') for i in last]}")

    def wait_absent(self, instance_id: str, timeout: float = 8.0) -> None:
        deadline = time.time() + timeout
        while time.time() < deadline:
            resp = self.call("list_instances")
            require(not tool_error(resp), f"list_instances failed: {tool_text(resp)}")
            ids = {item.get("instanceId") for item in (tool_json(resp).get("instances") or [])}
            if instance_id not in ids:
                return
            time.sleep(0.2)
        raise Fail(f"{instance_id} still listed after disconnect")

    def wait_log(self, proc: Proc, needle: str, timeout: float = 10.0) -> None:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if needle in proc.log_text():
                return
            if not proc.alive():
                raise Fail(f"{proc.name} exited before log contained {needle!r}")
            time.sleep(0.1)
        raise Fail(f"{proc.name} log did not contain {needle!r}")

    def observe_events(self, instance_id: str) -> list[dict[str, Any]]:
        resp = self.call("observe", {"instance_id": instance_id})
        require(not tool_error(resp), f"observe {instance_id} failed: {tool_text(resp)}")
        body = tool_json(resp)
        return list(body.get("events") or [])

    def drain_observe(self, instance_id: str) -> None:
        for _ in range(4):
            events = self.observe_events(instance_id)
            if not events:
                return
            time.sleep(0.05)

    def wait_remote_key(self, instance_id: str, name: str, timeout: float = 3.0) -> list[dict[str, Any]]:
        deadline = time.time() + timeout
        collected: list[dict[str, Any]] = []
        while time.time() < deadline:
            events = self.observe_events(instance_id)
            collected.extend(events)
            if name in observed_keys(collected, "INPUT_SOURCE_REMOTE"):
                return collected
            time.sleep(0.1)
        raise Fail(f"{instance_id} observe missing remote {name}: {collected}")

    def cleanup(self) -> None:
        if self.keep:
            print("keeping processes (--keep):")
            for proc in self.procs:
                print(f"  {proc.name} pid={proc.pid} alive={proc.alive()} log={proc.log_path}")
            return
        for proc in reversed(self.procs):
            proc.terminate()


def observed_keys(events: list[dict[str, Any]], source: str) -> list[str]:
    names: list[str] = []
    for event in events:
        observed = event.get("inputObserved") or {}
        if observed.get("source") != source:
            continue
        key = observed.get("key") or {}
        name = key.get("name")
        if name:
            names.append(str(name))
    return names


def run_automated(harness: Harness) -> None:
    harness.start_proxy()
    sim_a = harness.start_sim("e2e-a")
    sim_b = harness.start_sim("e2e-b")
    instances = harness.wait_instances({"e2e-a", "e2e-b"})
    by_id = {item["instanceId"]: item for item in instances}
    pid_a = by_id["e2e-a"]["register"]["pid"]
    pid_b = by_id["e2e-b"]["register"]["pid"]
    require(pid_a != pid_b, f"instance pids are not distinct: {pid_a}")
    print(f"ok two instances listed (pids {pid_a}, {pid_b})")

    missing_id = harness.call("list_instances")
    require(not tool_error(missing_id), f"list_instances failed: {tool_text(missing_id)}")
    omitted = harness.call("get_instance", {})
    require(tool_error(omitted), "get_instance without instance_id should fail")
    omitted_text = tool_text(omitted).lower()
    require(
        "instance_id" in omitted_text or "default-instance" in omitted_text,
        f"unexpected omit-id error: {tool_text(omitted)}",
    )
    print("ok omitted instance_id is rejected")

    harness.drain_observe("e2e-a")
    harness.drain_observe("e2e-b")
    injected = harness.call("inject_key", {"instance_id": "e2e-a", "name": "ENTER", "hold_ms": 80})
    require(not tool_error(injected), f"inject_key e2e-a failed: {tool_text(injected)}")
    body = tool_json(injected)
    require(body.get("accepted") is True, f"inject_key e2e-a not accepted: {body}")
    events_a = harness.wait_remote_key("e2e-a", "ENTER")
    events_b = harness.observe_events("e2e-b")
    require(
        "ENTER" not in observed_keys(events_b, "INPUT_SOURCE_REMOTE"),
        f"e2e-b observe saw e2e-a ENTER: {events_b}",
    )
    print("ok inject on e2e-a is not visible on e2e-b")

    snap_a = harness.call("request_snapshot", {"instance_id": "e2e-a"})
    snap_b = harness.call("request_snapshot", {"instance_id": "e2e-b"})
    require(not tool_error(snap_a), f"snapshot e2e-a failed: {tool_text(snap_a)}")
    require(not tool_error(snap_b), f"snapshot e2e-b failed: {tool_text(snap_b)}")
    require(tool_json(snap_a).get("instanceId") == "e2e-a", f"snapshot a id: {tool_json(snap_a)}")
    require(tool_json(snap_b).get("instanceId") == "e2e-b", f"snapshot b id: {tool_json(snap_b)}")
    print("ok snapshots are instance-scoped")

    sim_b.terminate()
    harness.wait_absent("e2e-b")
    listed = harness.call("list_instances")
    require(not tool_error(listed), f"list_instances after kill failed: {tool_text(listed)}")
    require(harness.proxy is not None and harness.proxy.alive(), "proxy died after simulator exit")
    dead = harness.call("get_instance", {"instance_id": "e2e-b"})
    require(tool_error(dead), "get_instance on dead id should error")
    dead_inject = harness.call("inject_key", {"instance_id": "e2e-b", "name": "ENTER"})
    require(tool_error(dead_inject), "inject_key on dead id should error")
    still = harness.call("list_instances")
    require(not tool_error(still), f"proxy unusable after missing simulator: {tool_text(still)}")
    print("ok missing simulator does not take down MCP")

    require(sim_a.alive(), "e2e-a exited before missing-server check")
    assert harness.proxy is not None
    harness.proxy.terminate()
    time.sleep(0.4)
    require(sim_a.alive(), "simulator window quit when the server went away")
    harness.wait_log(sim_a, "control plane: retrying")
    print("ok missing server does not quit the simulator")

    harness.start_proxy()
    harness.wait_instances({"e2e-a"})
    require(sim_a.alive(), "simulator quit while reconnecting")
    print("ok simulator redials after the server returns")


def run_spawn(harness: Harness) -> None:
    harness.start_proxy()
    caps = harness.read_resource("csm://capabilities")
    require(caps.get("spawn", {}).get("configured") is True, f"spawn not configured: {caps}")
    require(caps.get("spawn", {}).get("tool") == "start_instance", f"spawn tool: {caps}")
    print("ok csm://capabilities reports spawn configured")

    started = harness.call(
        "start_instance",
        {"instance_id": "e2e-spawn", "headless": True},
        timeout=20.0,
    )
    require(not tool_error(started), f"start_instance failed: {tool_text(started)}")
    body = tool_json(started)
    require(body.get("instanceId") == "e2e-spawn", f"start id: {body}")
    require(int(body.get("pid") or 0) > 0, f"start pid: {body}")
    print("ok start_instance registered e2e-spawn")

    harness.drain_observe("e2e-spawn")
    injected = harness.call(
        "inject_key",
        {"instance_id": "e2e-spawn", "name": "ENTER", "hold_ms": 80},
    )
    require(not tool_error(injected), f"inject_key failed: {tool_text(injected)}")
    require(tool_json(injected).get("accepted") is True, f"inject not accepted: {tool_json(injected)}")
    events = harness.wait_remote_key("e2e-spawn", "ENTER")
    require(
        "INPUT_SOURCE_HUMAN" not in [
            (event.get("inputObserved") or {}).get("source")
            for event in events
            if event.get("inputObserved")
        ],
        f"spawn observe saw HUMAN: {events}",
    )
    print("ok inject ENTER is remote")

    snap = harness.call("request_snapshot", {"instance_id": "e2e-spawn"})
    require(not tool_error(snap), f"snapshot failed: {tool_text(snap)}")
    snap_body = tool_json(snap)
    require(snap_body.get("instanceId") == "e2e-spawn", f"snapshot id: {snap_body}")
    require(snap_body.get("mimeType") == "image/png", f"snapshot mime: {snap_body}")
    require(int(snap_body.get("width") or 0) > 0, f"snapshot width: {snap_body}")
    print("ok snapshot PNG")


def run_headless(harness: Harness) -> None:
    harness.start_proxy()
    sim = harness.start_sim("e2e-headless", headless=True)
    harness.wait_instances({"e2e-headless"})
    deadline = time.time() + 5.0
    heartbeat: dict[str, Any] = {}
    while time.time() < deadline:
        inst = tool_json(harness.call("get_instance", {"instance_id": "e2e-headless"}))
        heartbeat = inst.get("lastHeartbeat") or {}
        if heartbeat.get("headless") is True:
            break
        time.sleep(0.2)
    else:
        raise Fail(f"lastHeartbeat.headless was not true: {heartbeat}")
    require(sim.alive(), "headless simulator exited before heartbeat")
    print("ok heartbeat reports headless")

    harness.drain_observe("e2e-headless")
    injected = harness.call(
        "inject_key",
        {"instance_id": "e2e-headless", "name": "ENTER", "hold_ms": 80},
    )
    require(not tool_error(injected), f"inject_key failed: {tool_text(injected)}")
    body = tool_json(injected)
    require(body.get("accepted") is True, f"inject_key not accepted: {body}")
    events = harness.wait_remote_key("e2e-headless", "ENTER")
    require(
        "INPUT_SOURCE_HUMAN" not in [
            (event.get("inputObserved") or {}).get("source")
            for event in events
            if event.get("inputObserved")
        ],
        f"headless observe saw HUMAN: {events}",
    )
    print("ok inject ENTER is remote and not human")

    snap = harness.call("request_snapshot", {"instance_id": "e2e-headless"})
    require(not tool_error(snap), f"snapshot failed: {tool_text(snap)}")
    snap_body = tool_json(snap)
    require(snap_body.get("instanceId") == "e2e-headless", f"snapshot id: {snap_body}")
    require(snap_body.get("mimeType") == "image/png", f"snapshot mime: {snap_body}")
    require(int(snap_body.get("width") or 0) > 0, f"snapshot width: {snap_body}")
    require(int(snap_body.get("height") or 0) > 0, f"snapshot height: {snap_body}")
    print("ok snapshot PNG")


def run_human(harness: Harness) -> None:
    harness.start_proxy()
    sim = harness.start_sim("e2e-human")
    harness.wait_instances({"e2e-human"})
    harness.drain_observe("e2e-human")
    print()
    print(f"Simulator window title: {WINDOW_TITLE}")
    print("In that window, press Escape or Return, or click the panel.")
    print("Then return here and press Enter.")
    try:
        input()
    except EOFError as err:
        raise Fail("stdin closed before human input") from err
    require(sim.alive(), "simulator exited before observe")
    events = harness.observe_events("e2e-human")
    sources = [
        (event.get("inputObserved") or {}).get("source")
        for event in events
        if event.get("inputObserved")
    ]
    require(
        "INPUT_SOURCE_HUMAN" in sources,
        f"observe had no INPUT_SOURCE_HUMAN: {events}",
    )
    print("ok observe saw INPUT_SOURCE_HUMAN")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Session e2e: two instances, missing peer, optional human observe",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    parser.add_argument(
        "--human",
        action="store_true",
        help="prompt for a real key/click, then require INPUT_SOURCE_HUMAN",
    )
    parser.add_argument(
        "--headless",
        action="store_true",
        help="one instance with --sim-headless; require heartbeat, inject, snapshot",
    )
    parser.add_argument(
        "--spawn",
        action="store_true",
        help="proxy start_instance of SIMULATOR_BIN; require inject and snapshot",
    )
    parser.add_argument(
        "--keep",
        action="store_true",
        help="leave proxy and simulator processes running",
    )
    parser.add_argument(
        "--show-windows",
        action="store_true",
        help="open real SDL windows (default automated mode uses SDL dummy video)",
    )
    parser.add_argument(
        "--log-dir",
        help="directory for process logs (default: $TMPDIR or /tmp)",
    )
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        harness = Harness(args)
    except Fail as err:
        print(f"FAIL: {err}", file=sys.stderr)
        return 2
    try:
        if sum(bool(flag) for flag in (args.human, args.headless, args.spawn)) > 1:
            raise Fail("--human, --headless, and --spawn cannot be combined")
        if args.human:
            run_human(harness)
        elif args.headless:
            run_headless(harness)
        elif args.spawn:
            run_spawn(harness)
        else:
            run_automated(harness)
        print("PASS")
        return 0
    except Fail as err:
        print(f"FAIL: {err}", file=sys.stderr)
        return 1
    finally:
        harness.cleanup()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
