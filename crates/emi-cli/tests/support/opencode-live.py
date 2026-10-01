"""Installed OpenCode V2 events + real EMI/pytest, with a local scripted provider.

Run after preparing a configured scratch Project fixture:
    python3 opencode-live.py REPO EMI_BINARY STATE_ROOT
No model, account credential, or remote provider is used. This exercises
native harness admission/events; it is not a usefulness experiment.
"""

import json
import os
from pathlib import Path
import re
import secrets
import selectors
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Proposer(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.server.calls.append(request)
        number = len(self.server.calls)
        if number > 3:
            self.send_error(500, "unexpected extra proposer turn")
            return
        repo = self.server.repo
        if number == 1:
            (repo / "app.py").write_text("def add(a, b):\n    return a - b\n")
        elif number == 2:
            (repo / "app.py").write_text(
                "def add(a, b):\n    if a is None:\n        return b\n    return a + b\n"
            )
            (repo / "tests/test_new.py").write_text(
                "from app import add\n\ndef test_none():\n    assert add(None, 2) == 2\n"
                "\ndef test_again():\n    assert add(1, 2) == 3\n"
            )
        else:
            (repo / "tests/test_new.py").write_text(
                "from app import add\n\ndef test_none():\n    assert add(None, 2) == 2\n"
            )
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        for delta, finish in [({"role": "assistant", "content": "Done."}, None), ({}, "stop")]:
            chunk = {
                "id": f"fixture-{number}", "object": "chat.completion.chunk",
                "created": 1, "model": "scripted",
                "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
            }
            self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode())
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()


def run(repo, env, args):
    output = subprocess.run(args, cwd=repo, env=env, capture_output=True, text=True, timeout=45)
    if output.returncode:
        raise RuntimeError(f"{args}: exit {output.returncode}\n{output.stdout}\n{output.stderr}")
    return output.stdout


def endpoint(server):
    with selectors.DefaultSelector() as selector:
        selector.register(server.stdout, selectors.EVENT_READ)
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if not selector.select(timeout=1):
                continue
            line = server.stdout.readline()
            match = re.search(r"http://127\.0\.0\.1:\d+", line)
            if match:
                return match.group()
            if server.poll() is not None:
                raise RuntimeError(f"OpenCode server exited before readiness: {line}")
    raise RuntimeError("OpenCode server did not report readiness")


def check(repo, env, url, provider, events_path):
    def api(method, path, body=None):
        args = ["opencode", "api", "--server", url, method, path]
        if body is not None:
            args += ["--data", json.dumps(body)]
        output = run(repo, env, args)
        return json.loads(output) if output.strip() else None

    api("get", "/api/config")
    api("get", "/api/agent")
    readiness = time.monotonic() + 10
    plugins = []
    while not plugins and time.monotonic() < readiness:
        plugins = api("post", "/api/plugin/check", {})["data"]
        if not plugins:
            time.sleep(0.25)
    emi = [p for p in plugins if p["id"] == "emi-hook"]
    assert len(emi) == 1 and emi[0]["state"]["status"] == "active", {
        "plugins": plugins, "config": api("get", "/api/config"),
    }
    session = api("post", "/api/session", {"title": "EMI native loop fixture"})["data"]
    session_id = session["id"]
    api("post", f"/api/session/{session_id}/model", {"model": {"providerID": "fixture", "id": "scripted"}})
    api("post", f"/api/session/{session_id}/prompt", {"text": "Handle None in add, then finish."})
    deadline = time.monotonic() + 75
    while time.monotonic() < deadline:
        # Waiting for a native transcript, not a shell background job.
        context = api("get", f"/api/session/{session_id}/context")["data"]
        pending = api("get", f"/api/session/{session_id}/inbox")["data"]
        text = json.dumps([context, pending])
        if "Change evidence: proven" in text:
            assert len(provider.calls) == 3, provider.calls
            assert "repair round 1 of 3" in text and "repair round 2 of 3" in text, text
            assert len(pending) == 1 and pending[0]["delivery"] == "queue", pending
            print("Native OpenCode: regression repair, redundancy repair, proven receipt; 3 scripted turns")
            return
        if "INCOMPLETE" in text or "NOT VERIFIED" in text:
            raise AssertionError(text)
        time.sleep(0.25)
    raise AssertionError({
        "proposer_calls": len(provider.calls), "context": context, "pending": pending,
        "session": api("get", f"/api/session/{session_id}"),
        "events": events_path.read_text() if events_path.exists() else "no fixture events",
    })


def main():
    repo, binary, state = map(lambda p: Path(p).resolve(), sys.argv[1:])
    with tempfile.TemporaryDirectory(prefix="emi-live-", dir=state) as scratch:
        env = {k: v for k, v in os.environ.items() if not k.startswith(("OPENCODE_", "EMI_"))}
        env["HOME"] = scratch
        env["OPENCODE_PASSWORD"] = secrets.token_hex(24)
        for kind in ["CONFIG", "DATA", "STATE", "CACHE"]:
            directory = Path(scratch) / kind.lower()
            directory.mkdir()
            env[f"XDG_{kind}_HOME"] = str(directory)
        run(repo, env, [str(binary), "hook", "install", "--harness", "opencode"])
        provider = ThreadingHTTPServer(("127.0.0.1", 0), Proposer)
        provider.calls, provider.repo = [], repo
        threading.Thread(target=provider.serve_forever, daemon=True).start()
        port = provider.server_address[1]
        config = {
            "$schema": "https://opencode.ai/config.json", "model": "fixture/scripted",
            "plugins": ["./plugins/emi", "./plugins/fixture-title"],
            "providers": {"fixture": {
                "package": "@opencode/ai/providers/openai-compatible",
                "settings": {"baseURL": f"http://127.0.0.1:{port}/v1", "apiKey": "local-fixture"},
                "models": {"scripted": {"limit": {"context": 100000, "output": 1000}}},
            }},
        }
        (repo / ".opencode/opencode.json").write_text(json.dumps(config))
        title = repo / ".opencode/plugins/fixture-title"
        title.mkdir()
        events_path = Path(scratch) / "events.jsonl"
        (title / "index.js").write_text(
            'import {appendFile} from "node:fs/promises";\n'
            'export default {id:"fixture-title", async setup(ctx) {'
            'await ctx.session.hook("title", e => { e.result = "Native loop fixture" });'
            'const c = new AbortController(); void (async () => {'
            'for await (const e of ctx.event.subscribe({signal:c.signal})) {'
            f'await appendFile({json.dumps(str(events_path))}, JSON.stringify(e)+"\\n");'
            '}})().catch(()=>{}); return () => c.abort(); }}\n'
        )
        server = subprocess.Popen(
            ["opencode", "serve", "--hostname", "127.0.0.1", "--port", "0"],
            cwd=repo, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
        )
        try:
            check(repo, env, endpoint(server), provider, events_path)
        finally:
            server.terminate()
            try:
                server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()
            provider.shutdown()
            provider.server_close()


if __name__ == "__main__":
    main()
