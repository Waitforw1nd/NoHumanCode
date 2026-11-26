"""Opt-in real CLI/Host + Notepad acceptance. See notepad_save_as.md."""
import argparse
import ctypes
import json
import os
import socket
import subprocess as sp
import tempfile
import threading
import time
import uuid
from contextlib import ExitStack
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


def check(condition, message):
    if not condition:
        raise RuntimeError(message)


def eventually(probe, label, seconds=45):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        result = probe()
        if result:
            return result
        time.sleep(.2)
    raise TimeoutError(label)


def acceptance(binary, stack, host_only):
    root = Path(tempfile.mkdtemp(prefix="coda-notepad-"))
    print("Retained fixture:", root, flush=True)
    ws = root / "workspace"
    ws.mkdir()
    text = "Coda-Notepad-" + uuid.uuid4().hex
    source, dest = ws / "original.txt", ws / "new-name.txt"
    source.write_bytes(text.encode())
    original_mtime = source.stat().st_mtime_ns
    # No operator keys, proxy, MCP config, or previously selected route.
    env = {k: os.environ[k] for k in ("SystemRoot", "WINDIR", "PATH", "TEMP", "TMP",
                                      "USERPROFILE", "LOCALAPPDATA", "APPDATA") if k in os.environ}
    env["NO_PROXY"] = "*"
    hidden = sp.CREATE_NO_WINDOW
    system = Path(env["SystemRoot"]) / "System32"
    def stop(p):
        if p.poll() is None:
            p.terminate()
            p.wait(timeout=10)

    if not host_only:
        existing = sp.check_output([system / "tasklist.exe", "/FI", "IMAGENAME eq notepad.exe",
                                    "/FO", "CSV", "/NH"], creationflags=hidden)
        check(b'"notepad.exe"' not in existing.lower(), "Existing Notepad: abort; use a clean VM")
        notepad = sp.Popen([system / "notepad.exe", source], env=env)
        stack.callback(stop, notepad)
        env["CODA_COMPUTER_TARGET"] = json.dumps({
            "pid": notepad.pid, "workspace": str(ws), "allow_save_as": True})
    user = ctypes.windll.user32
    user.GetForegroundWindow.restype = ctypes.c_void_p
    user.GetWindowThreadProcessId.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong)]

    def focused():
        check(notepad.poll() is None, "Launched Notepad exited/handed off; will not adopt another PID")
        pid = ctypes.c_ulong()
        user.GetWindowThreadProcessId(user.GetForegroundWindow(), ctypes.byref(pid))
        return pid.value == notepad.pid

    state = {"phase": None, "refs": [], "results": {}, "fault": None}

    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            try:
                check(self.path == "/v1/chat/completions", "unexpected mock endpoint")
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                phase = state["phase"]
                last = body["messages"][-1]
                call_id = last.get("tool_call_id")
                if last["role"] == "user":
                    name, args = "computer_observe", {}
                elif call_id == phase + "-computer_observe":
                    obs = json.loads(last["content"])
                    ref = obs["observation_ref"]
                    check(obs.get("save_as_available") is True and isinstance(ref, str) and ref
                          and ref not in state["refs"], "No fresh Save As observation")
                    state["refs"].append(ref)
                    print(f"{phase}: save_as_available=true observation_ref={ref}", flush=True)
                    name, args = "computer_save_as", {"observation_ref": ref,
                                   "path": dest.name, "expected_text": text}
                elif call_id == phase + "-computer_save_as":
                    state["results"][phase] = last["content"]
                    name, args = ("read_file", {"path": dest.name}) if phase == "fresh" else (None, None)
                elif call_id == "fresh-read_file":
                    check(json.loads(last["content"])["content"] == text, "read_file content mismatch")
                    state["results"]["read_file"] = text
                    name, args = None, None
                else:
                    raise ValueError("Unexpected tool result/order: " + str(call_id))
                if name:
                    check(name in [t["function"]["name"] for t in body["tools"]], "Tool absent: " + name)
                    delta = {"tool_calls": [{"index": 0, "id": phase + "-" + name, "type": "function",
                             "function": {"name": name, "arguments": json.dumps(args)}}]}
                else:
                    delta = {"content": "done"}
                event = {"choices": [{"delta": delta, "finish_reason": "tool_calls" if name else "stop"}]}
                data = ("data: " + json.dumps(event) + "\n\ndata: [DONE]\n\n").encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
            except Exception as exc:
                state["fault"] = str(exc)
                self.send_error(500)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    stack.callback(server.server_close)
    stack.callback(server.shutdown)
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = str(sock.getsockname()[1])
    prefix = [binary, "--port", port]
    log = stack.enter_context((root / "host.log").open("wb"))
    host = sp.Popen(prefix + ["--data-dir", root / "data", "--workspace", ws], cwd=root, env=env,
                    stdin=sp.DEVNULL, stdout=log, stderr=log, creationflags=hidden)
    stack.callback(stop, host)

    def cli(*args, stdin=None):
        check(host.poll() is None, "Fixture Host exited")
        check(not state["fault"], state["fault"])
        out = sp.run(prefix + ["--json", *map(str, args)], cwd=root, env=env, input=stdin or "",
                     capture_output=True, encoding="utf-8", timeout=20, creationflags=hidden)
        check(out.returncode == 0, f"{args[:2]} failed: {out.stderr} {out.stdout}")
        return json.loads(out.stdout)

    def ready():
        check(host.poll() is None, "Host startup failed: see fixture host.log")
        try:
            with socket.create_connection(("127.0.0.1", int(port)), timeout=.3):
                return True
        except OSError:
            return False

    eventually(ready, "Host did not listen")
    check(cli("host", "config", "get") == {"revision": 0, "routes": []}, "Expected a fresh Host")
    base = f"http://127.0.0.1:{server.server_port}/v1"
    cli("host", "route", "set", "local", "--expected-revision", 0, "--name", "Mock",
        "--base-url", base, "--model", "mock", "--max-tokens", 128, "--parallel-limit", 1,
        "--key", "setup-route", "--credential-stdin", stdin="synthetic-notepad-only\n")
    project = cli("project", "create", "--name", "Notepad", "--root", ws, "--key", "project")["id"]
    config = root / "config.json"
    config.write_text(json.dumps({"allowed_route_ids": ["local"], "default_route_id": "local",
                                  "tools": True, "write_scopes": [dest.name], "allow_commands": False}))
    rev = cli("project", "config", "get", project)["revision"]
    cli("project", "config", "set", project, "--expected-revision", rev, "--file", config)
    cli("project", "select", project, "--expected-revision", cli("project", "current")["revision"])
    cli("project", "budget", "config", "set", "--id", project, "--expected-revision", 0,
        "--limit-tokens", 20000, "--per-call-reservation-tokens", 128, "--key", "budget")
    preview = cli("network", "preview", "--route", "local", "--project", project)
    cli("network", "authorize", "--route", "local", "--project", project, "--purpose", "completion",
        "--config-revision", preview["config_revision"], "--credential-revision",
        preview["credential_revision"], "--binding", preview["binding_digest"], "--key", "network")
    if host_only:
        print("Host setup OK (no UI, tool call, approval or save exercised).")
        return

    for phase in ["stale", "fresh"]:
        state["phase"] = phase
        print(f"\n{phase}: target PID {notepad.pid}, destination {dest}, expected text {text}.")
        print("After typing APPROVE, manually focus this Notepad's already-open Save As dialog.")
        print("Open File > Save As BEFORE typing APPROVE here. Do NOT click Save.")
        check(input("> ") == "APPROVE", "Operator aborted")
        eventually(focused, "Notepad not foreground", 90)
        run = cli("run", "start", "--route", "local", "--title", phase, "--message", phase,
                  "--key", phase, "--tools", "--scope", dest.name)
        task = run["tasks"][0]["id"]

        def pending():
            return next((c for c in cli("task", "approvals", task)["approvals"]
                         if c["status"] == "pending"), None)

        card = eventually(pending, "No pending Save As approval")
        check(card["tool_call_id"] == phase + "-computer_save_as" and
              card["tool_name"] == "computer_save_as" and card["task_id"] == task and
              dest.name in card["preview"] and text in card["preview"], "Unexpected approval")
        print("Pending:", card["preview"], flush=True)
        if phase == "stale":
            print("NOW click THIS console to deliberately defocus Notepad; leave console focused.", flush=True)
            eventually(lambda: not focused(), "Operator did not defocus target", 90)
        else:
            check(focused(), "Focus lost before fresh approval")
        check(not dest.exists() and source.read_bytes() == text.encode(), "Changed before approval")
        cli("approval", "approve", card["id"])
        def terminal():
            item = cli("run", "status", run["run_id"])["tasks"][0]
            return item if item["status"] in ("completed", "failed", "interrupted", "cancelled") else None
        done = eventually(terminal, "Task did not terminate")
        receipt = cli("approval", "get", card["id"])
        print("Approval", receipt["id"], receipt["status"], receipt["execution_state"])
        check(receipt["status"] == "approved", "Approval was not recorded")
        result = state["results"].get(phase) or done.get("error") or ""
        print("Tool outcome:", result, flush=True)
        if phase == "stale":
            check(not dest.exists() and any(word in str(result).lower() for word in
                  ("foreground changed", "no longer foreground", "reobserve", "re-observe", "重新观察")),
                  "Missing explicit stale/foreground rejection")
        else:
            check(done["status"] == "completed" and receipt["execution_state"] == "finished"
                  and state["results"].get("read_file") == text
                  and dest.read_bytes() == text.encode(), "Missing verified saved bytes/read_file result")
        check(source.read_bytes() == text.encode() and source.stat().st_mtime_ns == original_mtime,
              "Source overwritten")
    print("PASS: stale approval refused; fresh approved save and independent read_file verified.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--coda", type=Path, required=True)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--disposable-session", action="store_true")
    mode.add_argument("--check-host-only", action="store_true")
    options = parser.parse_args()
    check(os.name == "nt" and options.coda.is_file(), "Requires Windows and built coda.exe")
    with ExitStack() as resources:
        acceptance(options.coda.resolve(), resources, options.check_host_only)
