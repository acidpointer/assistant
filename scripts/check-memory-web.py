#!/usr/bin/env python3
"""Required-artifact integration smoke against two real daemon processes.

No downloads or implicit builds. Credentials stay in process memory; logs contain
only sanitized progress. Temporary stores contain authored synthetic facts only.
This checks native/source recall and HTTP recovery, not held-out graph quality.
"""

import argparse
import contextlib
import json
import os
from pathlib import Path
import queue
import signal
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request


INPUT = "Remember this fact: my bicycle is blue. Reply in one short sentence."


class Daemon:
    """Own one foreground daemon and join it on every success/failure path."""

    def __init__(self, args, work):
        env = dict(os.environ)
        env.pop("XDG_RUNTIME_DIR", None)
        self.process = subprocess.Popen(
            [str(args.binary), "--model", str(args.model),
             "--memory-dir", str(work / "data"), "--embedding-model", str(args.embedding),
             "--cpu", "--ctx-size", "4096",
             "--max-tokens", "128", "--run-max-tokens", "128", "--threads", "4",
             "--no-tools", "--port", "0"],
            env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        self.lines = queue.Queue(maxsize=128)

        def read_ready():
            for line in self.process.stderr:
                if line.startswith("WebUI ready at "):
                    self.lines.put(line.removeprefix("WebUI ready at ").strip())
                # Keep draining for the process lifetime. Native diagnostics can
                # otherwise fill the pipe and block the owner during inference.

        self.reader = threading.Thread(target=read_ready, daemon=True)
        self.reader.start()
        try:
            self.url = self.lines.get(timeout=90)
        except queue.Empty as error:
            self.close()
            raise AssertionError("daemon did not become ready; inspect startup independently") from error
        self.work = work

    def close(self):
        if self.process.poll() is None:
            self.process.send_signal(signal.SIGINT)
        try:
            self.process.wait(timeout=150)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
            raise AssertionError("daemon failed to join native owners during shutdown")
        if self.process.returncode != 0:
            raise AssertionError("daemon exited with a failure; private stderr was not printed")

    def call(self, path, method="GET", data=None, expected=200, origin=None):
        headers = {"Content-Type": "application/json", "X-Assistant-Client": "webui",
                   "Origin": origin or self.url}
        request = urllib.request.Request(
            self.url + path, method=method, headers=headers,
            data=None if data is None else json.dumps(data).encode(),
        )
        try:
            reply = urllib.request.urlopen(request, timeout=15)
        except urllib.error.HTTPError as error:
            reply = error
        with reply:
            assert reply.status == expected, f"{method} {path}: expected {expected}, got {reply.status}"
            raw = reply.read()
            return json.loads(raw) if raw else None

    def submit(self, key, text):
        return self.call("/api/v2/runs", "POST", {
            "client_id": "memory-web-smoke", "client_request_id": key,
            "input": {"type": "text", "text": text},
        }, expected=202)

    def terminal(self, operation):
        end = time.monotonic() + 300
        next_progress = time.monotonic() + 20
        while time.monotonic() < end:
            result = self.call("/api/v2/operations/" + operation)
            if result["status"] not in ("accepted", "running", "stopping"):
                if result["status"] != "completed":
                    snapshot = self.call("/api/v2/snapshot")
                    partial = [message["text"] for message in snapshot["messages"] if message["run_id"] == operation and message["role"] == "assistant"]
                    print("Synthetic smoke terminal output:", repr("\n".join(partial)[:1024]), flush=True)
                assert result["status"] == "completed", f"native smoke must complete: status={result['status']} limit={result.get('limit_reason')} code={result.get('error')}"
                return result
            if time.monotonic() >= next_progress:
                print("Native progress:", json.dumps({"status": result["status"], "turns": [{"turn": turn["turn"], "generated_tokens": turn["generated_tokens"], "timing": turn["timing"]} for turn in result.get("performance", [])]}), flush=True)
                next_progress += 20
            time.sleep(0.1)
        snapshot = self.call("/api/v2/snapshot")
        partial = [message["text"] for message in snapshot["messages"] if message["run_id"] == operation and message["role"] == "assistant"]
        print("Synthetic smoke partial output:", repr("\n".join(partial)[:512]), flush=True)
        raise AssertionError("native smoke exceeded its completion allowance")


def run(args):
    for path in (args.binary, args.model, args.embedding):
        if not path.is_file():
            raise AssertionError("required local binary/model artifact is absent")
    context = tempfile.TemporaryDirectory(prefix="assistant-memory-web-") if args.work_directory is None else contextlib.nullcontext(str(args.work_directory))
    with context as temporary:
        work = Path(temporary)
        work.mkdir(mode=0o700, parents=True, exist_ok=True)
        marker = work / ".assistant-web-smoke"
        if any(work.iterdir()) and (not marker.is_file() or marker.read_text() != "synthetic-smoke-v1"):
            raise AssertionError("refusing to use an existing non-smoke directory")
        marker.write_text("synthetic-smoke-v1")
        daemon = Daemon(args, work)
        try:
            assert daemon.call("/api/discovery") == {"api_version": 2, "authentication": "none"}
            daemon.call("/api/v2/snapshot")
            daemon.call("/api/v1/bootstrap", expected=404)
            daemon.call("/api/v2/memory/notes", "POST", {}, expected=403,
                        origin="http://other.invalid")
            profile = daemon.call("/api/v2/assistant")
            saved_profile = daemon.call("/api/v2/assistant", "PUT", {
                "revision": profile["revision"], "settings": {
                    "instructions": "Be a friendly project companion. Keep answers brief and remember the owner's preferences.",
                    "initiative_interval_seconds": None,
                },
            })
            daemon.call("/api/v2/assistant", "PUT", {
                "revision": profile["revision"], "settings": saved_profile["settings"],
            }, expected=409)
            daemon.call("/api/v2/memory/notes", "POST", {"request_id": "bench-finish-note", "text": "For my garden bench, I want a satin finish."})
            run = daemon.submit("bicycle-1", INPUT)
            daemon.terminal(run["id"])
            snapshot = daemon.call("/api/v2/snapshot")
            chat_id = snapshot["chat_id"]
            instance = snapshot["instance_id"]
            history = daemon.call("/api/v2/history?limit=16")
            seed = next(exchange for exchange in history["exchanges"] if exchange["input"]["source"]["text"] == INPUT)
            source_id = seed["input"]["source"]["id"]
            graph = daemon.call("/api/v2/memory/connections", "POST", {"source_id": source_id})
            assert any(link["relation"] == "responds_to" for link in graph["links"]), "chat becomes a navigable source graph"
            daemon.call("/api/v2/runs", "POST", {
                "client_id": "memory-web-smoke", "client_request_id": "bicycle-1",
                "input": {"type": "text", "text": "different input"},
            }, expected=409)
            recalled = daemon.call("/api/v2/memory/search", "POST", {"query": "bicycle color", "limit": 8})
            assert recalled["semantic_available"], "real E5 semantic channel must run"
            assert any("bicycle is blue" in hit["record"]["source"]["text"] for hit in recalled["sources"])
            if args.browser:
                frontend = Path(__file__).resolve().parent.parent / "crates/assistant-web/frontend"
                subprocess.run(["node", "scripts/check-memory-daemon.mjs"], cwd=frontend,
                               input=json.dumps({"url": daemon.url,
                                                 "expectedInput": INPUT,
                                                 "screenshot": str(args.screenshot) if args.screenshot else None}),
                               text=True, check=True, timeout=60)
            daemon.call("/api/v2/session", "POST", {}, expected=404)
            daemon.call("/api/v2/snapshot")
            print("PASS: direct local access, committed chat, history, semantic search and conflict", flush=True)
        finally:
            daemon.close()

        daemon = Daemon(args, work)
        try:
            daemon.call("/api/v2/snapshot")
            snapshot = daemon.call("/api/v2/snapshot")
            assert snapshot["chat_id"] == chat_id and snapshot["instance_id"] != instance
            assert daemon.call("/api/v2/assistant")["settings"] == saved_profile["settings"], "personality survives a fresh process"
            assert any(message["text"] == INPUT for message in snapshot["messages"])
            lookup = daemon.call("/api/v2/operations/lookup", "POST", {
                "client_id": "memory-web-smoke", "client_request_id": "bicycle-1",
            })
            assert lookup["id"] == run["id"] and lookup["status"] == "completed"
            retry = daemon.submit("bicycle-1", INPUT)
            assert retry["id"] == run["id"] and retry["status"] == "completed"
            question = daemon.submit("bench-finish-1", "What finish do I want for my garden bench? Answer briefly using saved memory.")
            daemon.terminal(question["id"])
            history = daemon.call("/api/v2/history?limit=1")
            answer = history["exchanges"][0]["assistant"]["source"]["text"]
            assert "satin" in answer.casefold(), "fresh-process generation must retrieve an explicit note that never appeared in chat history"
            print("PASS: fresh-process direct access, history, key reconciliation and native E5-backed answer", flush=True)
            if args.initiative:
                profile = daemon.call("/api/v2/assistant")
                settings = {"instructions": "You are my friendly project companion. I explicitly want you to start a NEW conversation by asking how my garden bench is doing at the next initiative check. Keep this check-in to one short question about my bench project.", "initiative_interval_seconds": 60}
                previous_notices = {notice["id"] for notice in daemon.call("/api/v2/assistant/notices")}
                daemon.call("/api/v2/assistant", "PUT", {"revision": profile["revision"], "settings": settings})
                before = daemon.call("/api/v2/history?limit=16")
                end = time.monotonic() + 190
                print("Checking one real-model initiative decision after its durable 60-second timer…", flush=True)
                while time.monotonic() < end:
                    notices = [notice for notice in daemon.call("/api/v2/assistant/notices") if notice["id"] not in previous_notices and not notice["dismissed"]]
                    if notices:
                        break
                    time.sleep(0.5)
                else:
                    status = daemon.call("/api/v2/assistant")["last_check"]
                    raise AssertionError(f"requested native initiative did not deliver; loop status={status}")
                assert "bench" in notices[0]["text"].casefold(), "initiative should honor the newly requested topic"
                assert daemon.call("/api/v2/history?limit=16") == before, "initiative does not fabricate user exchanges"
                daemon.call("/api/v2/assistant/notices/" + notices[0]["id"] + "/dismiss", "POST", {}, expected=204)
                assert daemon.call("/api/v2/assistant/notices")[0]["dismissed"]
                profile = daemon.call("/api/v2/assistant")
                settings["initiative_interval_seconds"] = None
                daemon.call("/api/v2/assistant", "PUT", {"revision": profile["revision"], "settings": settings})
                print("PASS: real-model assistant-initiated message, durable mailbox, no fake user turn, dismissal and disable", flush=True)

        finally:
            daemon.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=lambda value: Path(value).resolve(), required=True)
    parser.add_argument("--model", type=lambda value: Path(value).resolve(), required=True)
    parser.add_argument("--embedding", type=lambda value: Path(value).resolve(), required=True)
    parser.add_argument("--work-directory", type=lambda value: Path(value).resolve(), help="retain only synthetic smoke data and reuse durable keys on retries; refuses non-smoke directories")
    parser.add_argument("--initiative", action="store_true", help="also wait for one real-model assistant-initiated message; adds at least 60 seconds")
    parser.add_argument("--browser", action="store_true", help="also use the installed frontend Playwright/Chromium against the real daemon")
    parser.add_argument("--screenshot", type=lambda value: Path(value).resolve(), help="optional synthetic browser capture; requires --browser")
    run(parser.parse_args())
