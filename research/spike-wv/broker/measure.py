"""SPIKE-WV gate measurements (ADR-0031 item 4).

Five gate items, measured against the real pinned engine through the broker:

  a) a real HTTPS page loads (the fetch happens in host Chromium: the guest has
     no network; the network module comes later),
  b) JS bridge round trip both ways,
  c) a tap on a web button located by its text via the accessibility tree,
  d) frame time < 50 ms,
  e) extra memory per additional session (WebView) <= 60 MiB, 1 vs 3 sessions
     sharing one Chromium, one browser context each.

Every number is written to `evidence/`, and the summary is regenerated from the
JSON files (no hand-edited results).
"""

from __future__ import annotations

import base64
import json
import os
import statistics
import sys
import time
from typing import Any

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wv_broker import Browser, DEFAULT_SHELL, HERE  # noqa: E402

PAGE_HTML = """<!doctype html><html><head><meta charset=utf-8>
<meta name=viewport content="width=device-width,initial-scale=1">
<style>body{font:28px/1.4 sans-serif;margin:24px}button{font:32px sans-serif;
padding:18px 28px;display:block;margin:24px 0;width:320px;height:110px}</style>
</head><body>
<h1 id=title>ACOS WebView probe</h1>
<button id=btn>Tap me</button>
<div id=out>not tapped</div>
<script>
  window.acosBridgePing = function (s) { return "pong:" + s; };
  document.getElementById('btn').addEventListener('click', function () {
    document.getElementById('out').textContent = 'tapped';
    if (window.acosBridge) window.acosBridge('tapped');
  });
</script></body></html>"""


def write(out: str, name: str, obj: Any) -> str:
    os.makedirs(out, exist_ok=True)
    path = os.path.join(out, name)
    with open(path, "w") as fh:
        json.dump(obj, fh, indent=2, sort_keys=True)
        fh.write("\n")
    return path


def percentiles(values: list[float]) -> dict[str, float]:
    s = sorted(values)
    return {
        "n": len(s),
        "min_ms": round(s[0], 2),
        "median_ms": round(statistics.median(s), 2),
        "p95_ms": round(s[max(0, int(round(len(s) * 0.95)) - 1)], 2),
        "max_ms": round(s[-1], 2),
        "mean_ms": round(statistics.fmean(s), 2),
    }


def gate_a_https(b: Browser, out: str) -> dict[str, Any]:
    page = b.new_page(width=1080, height=2400, dpr=2.625)
    results = []
    for url in ("https://example.com/", "https://www.iana.org/help/example-domains"):
        try:
            nav = page.navigate(url, timeout=45)
            frame = page.frame(timeout=10)
            png = os.path.join(out, "gate-a-%s.png" % url.split("/")[2].replace(".", "-"))
            with open(png, "wb") as fh:
                fh.write(frame["png"])
            results.append({"url": url, "ok": True, **nav,
                            "frame_png_bytes": len(frame["png"]),
                            "frame_ms": round(frame["begin_frame_ms"], 2),
                            "screenshot": os.path.relpath(png, HERE),
                            "tls": page.eval("performance.getEntriesByType('navigation')"
                                             "[0] ? location.protocol : location.protocol")})
        except Exception as exc:  # noqa: BLE001 - evidence, not control flow
            results.append({"url": url, "ok": False, "error": f"{type(exc).__name__}: {exc}"})
    page.close()
    return {"gate": "a", "item": "real HTTPS page loads", "results": results,
            "passed": all(r["ok"] and r.get("nodes", 0) > 5 for r in results)}


def gate_b_jsbridge(b: Browser, out: str) -> dict[str, Any]:
    page = b.new_page()
    page.navigate("data:text/html," + PAGE_HTML.replace("#", "%23").replace("\n", "%20")
                  if False else "https://example.com/")
    # Deterministic page for the bridge itself: served as a data: URL so the
    # result cannot depend on a remote page's DOM.
    page.navigate("data:text/html;base64," + base64.b64encode(PAGE_HTML.encode()).decode())
    detail: dict[str, Any] = {}

    # guest -> page (evaluateJavascript with a ValueCallback)
    detail["host_to_page"] = page.eval("document.getElementById('title').textContent")

    # page -> host (addJavascriptInterface shim: a runtime binding)
    page.add_binding("acosBridge")
    page.eval("document.getElementById('btn').click()")
    detail["page_to_host_click"] = page.wait_binding("acosBridge", timeout=10)
    detail["page_state_after_click"] = page.eval("document.getElementById('out').textContent")

    # host -> page function call with a return value (async promise path)
    detail["host_calls_page_async"] = page.eval("window.acosBridgePing('x')")
    detail["await_promise"] = page.eval("new Promise(r => setTimeout(() => r('delayed-ok'), 30))")
    page.close()
    ok = (detail["host_to_page"] == "ACOS WebView probe"
          and detail["page_to_host_click"] == ["tapped"]
          and detail["page_state_after_click"] == "tapped"
          and detail["host_calls_page_async"] == "pong:x"
          and detail["await_promise"] == "delayed-ok")
    return {"gate": "b", "item": "JS bridge round trip both ways", "detail": detail,
            "passed": ok}


def gate_c_tap(b: Browser, out: str) -> dict[str, Any]:
    page = b.new_page(width=1080, height=2400, dpr=2.625)
    page.navigate("data:text/html;base64," + base64.b64encode(PAGE_HTML.encode()).decode())
    before = page.frame(timeout=10)
    node = page.find_by_text("Tap me")
    tap = page.tap_text("Tap me")
    after_text = page.eval("document.getElementById('out').textContent")
    after = page.frame(timeout=10)
    for name, blob in (("gate-c-before.png", before["png"]), ("gate-c-after.png", after["png"])):
        with open(os.path.join(out, name), "wb") as fh:
            fh.write(blob)
    # Second half: locate a link by its text on a *real* HTTPS page and verify
    # the tap navigates, which is what an agent does on web content.
    real: dict[str, Any] = {}
    try:
        page.navigate("https://www.wikipedia.org/", timeout=45)
        node2 = page.find_by_text("English")
        if node2:
            box = page.box(node2["backend_dom_node_id"])
            page.tap(box["x"] + box["w"] / 2, box["y"] + box["h"] / 2)
            deadline = time.monotonic() + 30
            url = page.eval("location.href")
            while time.monotonic() < deadline and "wikipedia.org" in url and "www.wikipedia.org" in url:
                time.sleep(0.25)
                url = page.eval("location.href")
            real = {"found": True, "role": node2["role"], "bounds": box, "url_after": url,
                    "navigated": "www.wikipedia.org" not in url}
        else:
            real = {"found": False}
    except Exception as exc:  # noqa: BLE001
        real = {"found": False, "error": f"{type(exc).__name__}: {exc}"}
    page.close()
    return {"gate": "c", "item": "tap on web button by text (a11y tree)",
            "ax_node": node, "tap": tap,
            "state": {"before": "not tapped", "after": after_text},
            "real_page_link_tap": real,
            "passed": bool(node) and after_text == "tapped" and tap["found"]
            and real.get("navigated", False)}


def gate_d_frames(b: Browser, out: str, sizes=(("1080x800@2.625", 1080, 800, 2.625),
                                             ("1080x2400@2.625", 1080, 2400, 2.625))):
    page = b.new_page()
    page.navigate("data:text/html;base64," + base64.b64encode(PAGE_HTML.encode()).decode())
    items = []
    for label, w, h, dpr in sizes:
        page.set_view(w, h, dpr)
        page.frame()
        times = []
        bytes_ = []
        for _ in range(30):
            f = page.frame()
            times.append(f["begin_frame_ms"])
            bytes_.append(len(f["png"]))
        # animated content: paint must still be produced per frame
        page.eval("document.getElementById('out').textContent = 't' + Date.now()")
        anim = []
        for _ in range(10):
            page.eval("document.getElementById('out').textContent = 'f' + Math.random()")
            anim.append(page.frame()["begin_frame_ms"])
        items.append({"view": label, "begin_frame": percentiles(times),
                      "with_dom_change": percentiles(anim),
                      "png_bytes_median": int(statistics.median(bytes_)),
                      "passed": percentiles(times)["p95_ms"] < 50.0
                      and percentiles(anim)["p95_ms"] < 50.0})
    page.close()
    return {"gate": "d", "item": "frame time < 50 ms (HeadlessExperimental.beginFrame, PNG)",
            "items": items, "passed": all(i["passed"] for i in items)}


def gate_e_memory(b: Browser, out: str) -> dict[str, Any]:
    """1 vs 3 sessions sharing one Chromium, one browser context each."""
    url = "data:text/html;base64," + base64.b64encode(PAGE_HTML.encode()).decode()

    def settle(seconds: float = 2.0) -> None:
        time.sleep(seconds)

    settle(1.0)
    baseline = b.process_tree()
    first = b.new_page(width=1080, height=2400, dpr=2.625)
    first.navigate(url)
    first.frame()
    settle()
    one = b.process_tree()
    extra = [b.new_page(width=1080, height=2400, dpr=2.625) for _ in range(2)]
    for p in extra:
        p.navigate(url)
        p.frame()
    settle()
    three = b.process_tree()
    for p in extra:
        p.close()
    settle(3.0)
    after_close = b.process_tree()
    first.close()
    settle(3.0)
    zero = b.process_tree()
    d_pss = (three["total_pss_mib"] - one["total_pss_mib"]) / 2.0
    d_rss = (three["total_rss_mib"] - one["total_rss_mib"]) / 2.0
    return {
        "gate": "e", "item": "extra memory per additional session (WebView) <= 60 MiB",
        "policy": "one Chromium per host, one browser context per session",
        "snapshots": [
            {"label": "0 sessions (idle)", **{k: baseline[k] for k in
                                             ("total_rss_mib", "total_pss_mib")},
             "procs": len(baseline["processes"])},
            {"label": "1 session", **{k: one[k] for k in
                                      ("total_rss_mib", "total_pss_mib")},
             "procs": len(one["processes"])},
            {"label": "3 sessions", **{k: three[k] for k in
                                       ("total_rss_mib", "total_pss_mib")},
             "procs": len(three["processes"])},
            {"label": "3 -> 1 after closing 2", **{k: after_close[k] for k in
                                                   ("total_rss_mib", "total_pss_mib")},
             "procs": len(after_close["processes"])},
            {"label": "0 sessions after closing all", **{k: zero[k] for k in
                                                         ("total_rss_mib", "total_pss_mib")},
             "procs": len(zero["processes"])},
        ],
        "per_extra_session_pss_mib": round(d_pss, 1),
        "per_extra_session_rss_mib": round(d_rss, 1),
        "passed": d_pss <= 60.0 and d_rss <= 60.0,
    }


def run(out: str, shell: str = DEFAULT_SHELL) -> int:
    os.makedirs(out, exist_ok=True)
    b = Browser(shell=shell)
    engine = b.version()
    b.cdp.call("Browser.getVersion")
    meta = {
        "engine_product": engine.get("product"),
        "revision": engine.get("revision"),
        "protocol": engine.get("protocolVersion"),
        "user_agent": engine.get("userAgent"),
        "argv": b.argv,
        "shell": shell,
        "host": os.uname().machine,
        "python": sys.version.split()[0],
        "started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }
    write(out, "engine.json", meta)
    gates = []
    for fn in (gate_a_https, gate_b_jsbridge, gate_c_tap):
        gates.append(fn(b, out))
        write(out, "gate-%s.json" % gates[-1]["gate"], gates[-1])
    gates.append(gate_d_frames(b, out))
    write(out, "gate-d.json", gates[-1])
    gates.append(gate_e_memory(b, out))
    write(out, "gate-e.json", gates[-1])
    b.close()

    lines = ["# SPIKE-WV gate summary (generated)", "",
             "Engine: %s (%s)" % (meta["engine_product"], meta["revision"]), "",
             "| gate | item | verdict | numbers |", "|---|---|---|---|"]
    def fmt(g: dict[str, Any]) -> str:
        if g["gate"] == "a":
            return "; ".join("%s -> %s nodes=%s load=%sms" % (
                r["url"], r.get("ready"), r.get("nodes"), round(r.get("load_ms", 0), 1))
                if r["ok"] else "%s -> %s" % (r["url"], r["error"]) for r in g["results"])
        if g["gate"] == "b":
            d = g["detail"]
            return "host->page=%r; page->host=%r; async=%r" % (
                d["host_to_page"], d["page_to_host_click"], d["await_promise"])
        if g["gate"] == "c":
            return "ax role=%s bounds=%s state=%s real-page-nav=%s" % (
                (g["ax_node"] or {}).get("role"),
                {k: round(v, 1) for k, v in (g["tap"].get("bounds") or {}).items()},
                g["state"]["after"], g["real_page_link_tap"].get("navigated"))
        if g["gate"] == "d":
            return "; ".join("%s median=%sms p95=%sms max=%sms" % (
                i["view"], i["begin_frame"]["median_ms"], i["begin_frame"]["p95_ms"],
                i["begin_frame"]["max_ms"]) for i in g["items"])
        if g["gate"] == "e":
            return "PSS %s MiB/session, RSS %s MiB/session" % (
                g["per_extra_session_pss_mib"], g["per_extra_session_rss_mib"])
        return ""
    for g in gates:
        lines.append("| %s | %s | **%s** | %s |" % (
            g["gate"], g["item"], "GO" if g["passed"] else "NO-GO", fmt(g)))
    lines += ["", "Verdicts: %d/%d GO" % (sum(1 for g in gates if g["passed"]), len(gates)), ""]
    summary = "\n".join(lines)
    with open(os.path.join(out, "SUMMARY.md"), "w") as fh:
        fh.write(summary)
    sys.stdout.write(summary)
    write(out, "gates.json", {"engine": meta, "gates": gates,
                              "go": sum(1 for g in gates if g["passed"])})
    return 0 if all(g["passed"] for g in gates) else 1
