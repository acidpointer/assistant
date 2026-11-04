#!/usr/bin/env python3
"""Exercise the real daemon, HTTP/SSE contract and SIGTERM with local model weights.

No model is downloaded. Absent default weights skip; --require-model makes that
an error. Server diagnostics stay in a bounded in-memory tail and are redacted
on failure. All spawned processes and subscriptions are closed in finally blocks.
"""
from __future__ import annotations

import argparse
import json
import gzip
import queue
import re
import signal
import subprocess
import threading
import time
import urllib.error
import urllib.request
from collections import deque
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def run(binary: Path, model: Path, stop_signal: signal.Signals, contract_output: Path | None) -> None:
    process = subprocess.Popen(
        [str(binary), "--model", str(model), "--port", "0", "--cpu", "--no-tools",
         "--ctx-size", "1024", "--batch-size", "32", "--max-tokens", "64"],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
        text=True,
    )
    lines: queue.Queue[str | None] = queue.Queue(maxsize=512)
    tail: deque[str] = deque(maxlen=32)

    def collect() -> None:
        assert process.stderr is not None
        for line in process.stderr:
            tail.append(line.rstrip())
            try:
                lines.put_nowait(line)
            except queue.Full:
                pass
        try:
            lines.put_nowait(None)
        except queue.Full:
            pass

    reader = threading.Thread(target=collect, daemon=True)
    reader.start()
    stream = None
    try:
        deadline = time.monotonic() + 30
        base = None
        while time.monotonic() < deadline:
            line = lines.get(timeout=max(0.1, deadline - time.monotonic()))
            if line is None:
                raise RuntimeError("daemon exited before readiness")
            match = re.search(r"WebUI ready at (http://\S+)", line)
            if match:
                base = match[1]
                break
        if base is None:
            raise RuntimeError("daemon did not become ready")

        def request(path: str, body=None, *, headers=None, expected=200):
            actual_headers = {} if headers is None else dict(headers)
            data = None
            if body is not None:
                data = json.dumps(body).encode()
                actual_headers.setdefault("Content-Type", "application/json")
                actual_headers.setdefault("X-Assistant-Client", "webui")
            req = urllib.request.Request(base + path, data=data, headers=actual_headers)
            try:
                response = urllib.request.urlopen(req, timeout=10)
            except urllib.error.HTTPError as error:
                response = error
            with response:
                assert response.status == expected, (path, response.status, expected)
                payload = response.read(8 * 1024 * 1024)
                return json.loads(payload) if payload else None

        with urllib.request.urlopen(base + "/", timeout=10) as response:
            html = response.read().decode()
            assert "worker-src 'self'" in response.headers["Content-Security-Policy"]
            module = re.search(r'src="(/assets/[^" ]+\.js)"', html)
            assert module is not None, "missing embedded module"
        with urllib.request.urlopen(urllib.request.Request(base + module[1], headers={"Accept-Encoding": "gzip"}), timeout=10) as response:
            assert response.headers["Content-Encoding"] == "gzip"
            assert "immutable" in response.headers["Cache-Control"]
            assert gzip.decompress(response.read()), "empty embedded module"
        bootstrap = request("/api/v1/bootstrap")
        chat = bootstrap["primary_chat_id"]
        path = f"/api/v1/chats/{chat}/runs"
        body = {"client_id": "smoke", "client_request_id": "first",
                "input": {"type": "text", "text": "Tell a short story about a tree."}}

        request("/api/v1/bootstrap", headers={"Host": "evil.invalid"}, expected=403)
        request(path, body, headers={"Origin": "http://evil.invalid"}, expected=403)
        request(path, body, headers={"Content-Type": "text/plain"}, expected=415)
        oversized = dict(body, input={"type": "text", "text": "x" * 140_000})
        request(path, oversized, expected=413)

        stream = urllib.request.urlopen(base + f"/api/v1/chats/{chat}/events", timeout=15)
        updates: queue.Queue[dict] = queue.Queue(maxsize=8192)
        stream_closed = threading.Event()

        def consume() -> None:
            kind, data = "", []
            try:
                for raw in stream:
                    line = raw.decode().rstrip("\r\n")
                    if line.startswith("event:"):
                        kind = line[6:].strip()
                    elif line.startswith("data:"):
                        data.append(line[5:].lstrip())
                    elif not line and data:
                        updates.put_nowait({"event": kind, "data": json.loads("\n".join(data))})
                        kind, data = "", []
            finally:
                stream_closed.set()

        subscriber = threading.Thread(target=consume, daemon=True)
        subscriber.start()
        initial = updates.get(timeout=5)
        assert initial["event"] == "snapshot"
        accepted = request(path, body, expected=202)
        duplicate = request(path, body, expected=202)
        assert duplicate["id"] == accepted["id"]

        terminal = None
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            update = updates.get(timeout=max(0.1, deadline - time.monotonic()))["data"]
            if update.get("type") == "run" and update["run"]["id"] == accepted["id"]:
                if update["run"]["status"] in {"completed", "truncated", "cancelled", "failed", "deadline"}:
                    terminal = update["run"]
                    break
        assert terminal is not None, "missing terminal SSE event"
        assert terminal["status"] in {"completed", "truncated"}, terminal
        snapshot = request(f"/api/v1/chats/{chat}")
        assert any(item["role"] == "assistant" and item["text"] for item in snapshot["messages"])
        assert terminal["performance"], "native measurements missing"
        assert sum(turn["generated_tokens"] for turn in terminal["performance"]) == terminal["generated_tokens"]
        assert all(turn["measurement_complete"] and turn["timing"]["generation_ns"] > 0 for turn in terminal["performance"])
        if terminal["status"] == "truncated":
            assert terminal["limit_reason"] in {"output_tokens", "context_capacity", "run_tokens"}
        if contract_output is not None:
            contract_output.write_text(json.dumps(snapshot), encoding="utf-8")

        # Start another interaction and stop the process with an open subscriber.
        body["client_request_id"] = "shutdown"
        request(path, body, expected=202)
        process.send_signal(stop_signal)
        assert process.wait(timeout=15) == 0, "shutdown signal did not cleanly stop the daemon"
        assert stream_closed.wait(5), "SSE prevented shutdown"
        subscriber.join(timeout=1)
        print(f"PASS: real generation, deduplication, HTTP policy, SSE and {stop_signal.name}")
    except Exception:
        for line in tail:
            print(line.replace(str(model), "<model>"))
        raise
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        if stream is not None:
            stream.close()
        reader.join(timeout=1)
        if process.stderr is not None:
            process.stderr.close()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/assistant")
    parser.add_argument("--model", type=Path, default=ROOT / "target/test-models/stories260K.gguf")
    parser.add_argument("--require-model", action="store_true")
    parser.add_argument("--contract-output", type=Path, help="Write the observed JSON snapshot for the TypeScript contract test")
    parser.add_argument("--signal", choices=("SIGTERM", "SIGINT"), default="SIGTERM")
    args = parser.parse_args()
    if not args.model.is_file():
        if args.require_model:
            parser.error("a local model is required")
        print("SKIP: no local smoke-test model")
        return
    run(args.binary.resolve(), args.model.resolve(), getattr(signal, args.signal), args.contract_output)


if __name__ == "__main__":
    main()
