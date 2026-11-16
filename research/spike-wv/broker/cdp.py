"""Minimal Chrome DevTools Protocol client over `--remote-debugging-pipe`.

The pipe transport (fds 3/4, NUL-terminated JSON) is deliberate: SPIKE-WV must
show that the *guest* never sees a DevTools TCP socket. The broker owns the
pipe; the guest only ever speaks ACOS's own framed protocol to the broker
(see `wv_broker.py --serve`).

Stdlib only: no websocket, no third-party packages.
"""

from __future__ import annotations

import json
import os
import queue
import subprocess
import threading
import time
from typing import Any


class CDPError(RuntimeError):
    """A protocol error / error object returned by Chromium."""


class CDP:
    def __init__(self, argv: list[str], env: dict[str, str] | None = None, cwd: str | None = None):
        # Chrome insists on having commands on fd 3 and replies on fd 4, and it
        # validates them in the browser process at startup. `preexec_fn` cannot
        # be used: CPython closes inherited fds *after* preexec_fn, so the dup2
        # there is undone. Instead the pipes are moved onto 3/4 in the parent,
        # `pass_fds` keeps exactly those two open across exec, and the parent's
        # originals are restored immediately after the fork.
        self._cmd_r, self._cmd_w = os.pipe()
        self._rep_r, self._rep_w = os.pipe()
        saved = {}
        for fd in (3, 4):
            try:
                saved[fd] = os.dup(fd)
            except OSError:
                saved[fd] = None
        try:
            os.dup2(self._cmd_r, 3)
            os.dup2(self._rep_w, 4)
            self.proc = subprocess.Popen(
                argv,
                env=env,
                cwd=cwd,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                pass_fds=(3, 4),
            )
        finally:
            for fd, keep in saved.items():
                if keep is None:
                    try:
                        os.close(fd)
                    except OSError:
                        pass
                else:
                    os.dup2(keep, fd)
                    os.close(keep)
        os.close(self._cmd_r)
        os.close(self._rep_w)
        self._next_id = 0
        self._pending: dict[int, queue.Queue] = {}
        self._lock = threading.Lock()
        self._subs: list[tuple[str | None, queue.Queue]] = []
        self._closed = False
        self._buf = b""
        self._reader = threading.Thread(target=self._read_loop, daemon=True)
        self._reader.start()

    # --- transport ---------------------------------------------------------
    def _read_loop(self) -> None:
        while True:
            try:
                chunk = os.read(self._rep_r, 1 << 16)
            except OSError:
                break
            if not chunk:
                break
            self._buf += chunk
            while b"\0" in self._buf:
                raw, self._buf = self._buf.split(b"\0", 1)
                if not raw:
                    continue
                try:
                    msg = json.loads(raw)
                except json.JSONDecodeError:
                    continue
                self._dispatch(msg)
        self._closed = True
        for q in list(self._pending.values()):
            q.put({"error": {"message": "browser closed the pipe"}})

    def _dispatch(self, msg: dict[str, Any]) -> None:
        if "id" in msg:
            # Keep the entry until `result()` consumes it: `send_raw` callers
            # (the touch-input pipeline) read the reply much later.
            with self._lock:
                q = self._pending.get(msg["id"])
            if q is not None:
                q.put(msg)
            return
        with self._lock:
            subs = list(self._subs)
        for sid, q in subs:
            if sid is None or sid == msg.get("sessionId"):
                q.put(msg)

    def _write(self, payload: dict[str, Any]) -> None:
        os.write(self._cmd_w, json.dumps(payload).encode() + b"\0")

    # --- calls -------------------------------------------------------------
    def send_raw(self, method: str, params: dict[str, Any] | None = None,
                 session_id: str | None = None) -> int:
        with self._lock:
            self._next_id += 1
            mid = self._next_id
            self._pending[mid] = queue.Queue()
        payload: dict[str, Any] = {"id": mid, "method": method, "params": params or {}}
        if session_id:
            payload["sessionId"] = session_id
        self._write(payload)
        return mid

    def result(self, mid: int, timeout: float = 30.0) -> dict[str, Any]:
        with self._lock:
            q = self._pending.get(mid)
        if q is None:
            raise CDPError(f"no pending call {mid}")
        msg = q.get(timeout=timeout)
        with self._lock:
            self._pending.pop(mid, None)
        if "error" in msg:
            raise CDPError(f"{msg['error'].get('message')} ({msg.get('error')})")
        return msg.get("result", {})

    def call(self, method: str, params: dict[str, Any] | None = None,
             session_id: str | None = None, timeout: float = 30.0) -> dict[str, Any]:
        return self.result(self.send_raw(method, params, session_id), timeout)

    def subscribe(self, session_id: str | None = None) -> queue.Queue:
        q: queue.Queue = queue.Queue()
        with self._lock:
            self._subs.append((session_id, q))
        return q

    def unsubscribe(self, q: queue.Queue) -> None:
        with self._lock:
            self._subs = [s for s in self._subs if s[1] is not q]

    def wait_event(self, q: queue.Queue, method: str, predicate=None,
                   timeout: float = 30.0) -> dict[str, Any]:
        deadline = time.monotonic() + timeout
        while True:
            left = deadline - time.monotonic()
            if left <= 0:
                raise TimeoutError(f"timed out waiting for {method}")
            ev = q.get(timeout=left)
            if ev.get("method") == method and (predicate is None or predicate(ev.get("params", {}))):
                return ev.get("params", {})

    def close(self) -> None:
        try:
            self.proc.terminate()
            self.proc.wait(timeout=10)
        except Exception:
            self.proc.kill()
        for fd in (self._cmd_w, self._rep_r):
            try:
                os.close(fd)
            except OSError:
                pass
