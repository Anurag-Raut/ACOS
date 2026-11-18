#!/usr/bin/env python3
"""parity_nodes.py -- the UI-tree model, matcher and comparator for PARITY1.

One node model for both sides of a parity run:

  ACOS ART   `tree/<frame_seq>.json` (what `acos verify` records per step)
  phone      `uiautomator dump` XML (live) or an agent-device snapshot JSON
             (`fixtures/reference-device/<app>/snapshot-*.json`, recorded)

Everything here is pure: no adb, no host, no network. `diff_trees.py` builds a
report from these primitives and `tools/tests/test-parity-harness.py` tests
them against the checked-in captures.

Why dp.  The phone is 1080x2400 @ 400 dpi (Android 15), the ACOS reference
session is 1080x2400 @ 420 dpi (Android 14 image). The same `dp` layout is then
a *different* number of pixels on each side, so comparing raw pixels would
report every node as a difference. Bounds are converted to dp with each side's
own dpi before they are compared; the tolerance is in dp too. That makes a
difference a layout difference, not a density one.

Bounds that still differ are reported with their dp delta. A difference that
is exactly the window-frame shift (status/navigation bar insets) is marked
`explained_by: decor_shift` and can be allowlisted as one documented platform
difference instead of one entry per node.
"""

from __future__ import annotations

import re

# ---------------------------------------------------------------------------
# roles
# ---------------------------------------------------------------------------

# A role is the *behaviour* a class exposes, which is what an agent's selector
# semantics actually depend on. `class_changed` is only reported when the role
# changes; a different concrete class with the same role is informational
# (`class_name_differs`), e.g. `android.widget.FrameLayout` vs
# `android.view.ViewGroup` for the same container.
_ROLE_RULES = (
    ("button", ("button",)),
    ("edit", ("edittext", "textinputedittext", "autocompletetextview")),
    ("check", ("checkbox", "switch", "switchcompat", "togglebutton", "radiobutton")),
    ("seek", ("seekbar", "progressbar", "ratingbar")),
    ("image", ("imageview", "imagebutton", "appcompatimageview")),
    ("list", ("recyclerview", "listview", "gridview", "scrollview",
              "horizontalScrollview".lower(), "viewpager", "viewpager2", "webview")),
    ("text", ("textview", "textclock", "digitalclock", "title")),
    ("container", ("viewgroup", "framelayout", "linearlayout", "relativelayout",
                   "constraintlayout", "coordinatorlayout", "view", "surfaceview",
                   "actionbaroverlaylayout", "toolbar", "decorview", "layout")),
)


def derive_role(class_name: str) -> str:
    """Class -> coarse role. Unknown classes keep their own name as the role."""
    name = (class_name or "").strip()
    if not name:
        return "unknown"
    low = name.rsplit(".", 1)[-1].lower()
    # A `...Layout` is a container before it is anything else: `ButtonBarLayout`
    # is the dialog's button container, not a button.
    if low.endswith("layout") or low.endswith("layoutmanager"):
        return "container"
    for role, needles in _ROLE_RULES:
        for needle in needles:
            if needle in low:
                return role
    return "other:" + low


def role_of(node) -> str:
    return node.get("role") or derive_role(node.get("class", ""))


# ---------------------------------------------------------------------------
# node model
# ---------------------------------------------------------------------------

def _int_bounds(value):
    """uiautomator '[x1,y1][x2,y2]', a 4-list, or a rect dict -> [x1,y1,x2,y2]."""
    if value is None:
        return None
    if isinstance(value, (list, tuple)) and len(value) == 4:
        return [int(v) for v in value]
    if isinstance(value, dict):
        return [int(value.get("x", 0)), int(value.get("y", 0)),
                int(value.get("x", 0)) + int(value.get("width", 0)),
                int(value.get("y", 0)) + int(value.get("height", 0))]
    nums = [int(x) for x in re.findall(r"-?\d+", str(value))]
    return nums[:4] if len(nums) >= 4 else None


def _norm_text(value) -> str:
    """Trim and collapse runs of whitespace (incl. NBSP/narrow NBSP).

    A TextView's accessible name is the same string on both platforms; the
    spaces inside it are not always the same Unicode code point (the phone's
    clock uses U+202F, a framework string can use U+00A0). Collapsing those to
    a plain space compares the text an agent reads, not the byte encoding.
    """
    if value is None:
        return ""
    text = str(value).replace("\u00a0", " ").replace("\u202f", " ")
    return " ".join(text.split())


def make_node(*, resource_id="", class_name="", text="", desc="", hint="",
              enabled=None, checked=None, focused=None, selected=None,
              clickable=None, editable=None, visible=True, bounds=None,
              depth=0, package="", index=0, role=None):
    node = {
        "id": (resource_id or "").strip(),
        "class": (class_name or "").strip(),
        "text": _norm_text(text),
        "desc": _norm_text(desc),
        "hint": _norm_text(hint),
        "enabled": enabled,
        "checked": checked,
        "focused": focused,
        "selected": selected,
        "clickable": clickable,
        "editable": editable,
        "visible": bool(visible),
        "bounds_px": _int_bounds(bounds),
        "depth": depth,
        "package": package or "",
        "source_index": index,
    }
    node["role"] = role or derive_role(node["class"])
    return node


def with_dp(node, dpi):
    """Attach `bounds_dp` (rounded to 0.01 dp) using this side's dpi."""
    b = node.get("bounds_px")
    if not b or not dpi:
        node["bounds_dp"] = None
        return node
    scale = 160.0 / float(dpi)
    node["bounds_dp"] = [round(v * scale, 2) for v in b]
    node["dpi"] = dpi
    return node


# --- ACOS -----------------------------------------------------------------

def acos_nodes(tree_doc):
    """`acos verify` `tree/<seq>.json` -> flat node list in pre-order."""
    out = []

    def walk(children, depth):
        for raw in children or []:
            # Only the Android resource-id: ACOS's `id` field is its internal
            # node handle (`n4_dev.acos.fixtures.counter:id/count`), not a
            # resource id. Using it as a fallback invents ids the phone has
            # never seen.
            node = make_node(
                resource_id=raw.get("resource_id") or "",
                class_name=raw.get("class") or "",
                text=raw.get("text") or "",
                desc=raw.get("content_desc") or "",
                hint=raw.get("hint") or "",
                enabled=raw.get("enabled"),
                checked=raw.get("checked"),
                focused=raw.get("focused"),
                selected=raw.get("selected"),
                clickable=raw.get("clickable"),
                editable=raw.get("editable"),
                visible=raw.get("visible", True),
                # `bounds` is the node's rect in display coordinates, the same
                # quantity uiautomator reports. `visible_bounds` (a separate
                # ACOS field) is checked for consistency by the harness and
                # must never replace `bounds`: after a rotation it can be
                # clipped to a stale display rect.
                bounds=raw.get("bounds"),
                depth=depth,
                package=raw.get("package") or "",
                index=len(out),
            )
            if raw.get("visible_bounds") is not None:
                node["visible_bounds_px"] = _int_bounds(raw.get("visible_bounds"))
            out.append(node)
            walk(raw.get("children"), depth + 1)

    walk(tree_doc.get("nodes"), 0)
    return out


def tree_display(tree_doc):
    """The display a tree was captured on: its application window's bounds.

    `tree/<seq>.json` carries `windows[]`; the main application window reports
    the display it is on, so this is the authoritative display size for that
    frame (including after a rotation), never a value carried over from the
    scenario.
    """
    for window in tree_doc.get("windows") or []:
        bounds = _int_bounds(window.get("bounds"))
        if bounds and window.get("kind") == "application":
            return bounds
    nodes = tree_doc.get("nodes") or []
    if nodes:
        return _int_bounds(nodes[0].get("bounds"))
    return None


def clip(bounds, display):
    if not bounds or not display:
        return bounds
    return [max(bounds[0], display[0]), max(bounds[1], display[1]),
            min(bounds[2], display[2]), min(bounds[3], display[3])]


def tree_inconsistencies(nodes, display):
    """ACOS's own `visible_bounds` must be `bounds` clipped to the display.

    A node whose visible rect disagrees with that is an evidence defect in the
    tree an agent reads (the rotation bug PARITY1 found), independent of the
    phone. Reported per node, never silently dropped.
    """
    out = []
    for node in nodes:
        vb = node.get("visible_bounds_px")
        if vb is None or not display or not node.get("bounds_px"):
            continue
        expected = clip(node["bounds_px"], display)
        if vb != expected:
            out.append({
                "kind": "acos_tree_inconsistent",
                "key": node_key(node),
                "field": "visible_bounds",
                "acos": {"bounds_px": node["bounds_px"],
                         "visible_bounds_px": vb,
                         "display_px": display,
                         "expected_visible_bounds_px": expected},
                "reason": ("ACOS's visible_bounds is not the node's bounds "
                           "clipped to the session display"),
            })
    return out


# --- phone: uiautomator XML ----------------------------------------------

def phone_nodes_from_uiautomator(xml):
    """`uiautomator dump` XML -> flat node list (pre-order, depth preserved)."""
    import xml.etree.ElementTree as ET

    start = xml.find("<hierarchy")
    end = xml.rfind("</hierarchy>")
    if start < 0 or end < 0:
        return []
    root = ET.fromstring(xml[start:end + len("</hierarchy>")])
    out = []

    def walk(el, depth):
        for child in list(el):
            if child.tag != "node":
                continue
            a = child.attrib
            bounds = _int_bounds(a.get("bounds"))
            visible_attr = a.get("visible-to-user")
            if visible_attr is not None:
                visible = visible_attr == "true"
            else:
                # Android 15's dump has no visible-to-user; a zero-size rect is
                # the only thing the platform marks as not on screen.
                visible = bool(bounds) and bounds[2] > bounds[0] and bounds[3] > bounds[1]
            node = make_node(
                resource_id=a.get("resource-id") or "",
                class_name=a.get("class") or "",
                text=a.get("text") or "",
                desc=a.get("content-desc") or "",
                enabled=a.get("enabled") == "true" if a.get("enabled") is not None else None,
                checked=a.get("checked") == "true" if a.get("checked") is not None else None,
                focused=a.get("focused") == "true" if a.get("focused") is not None else None,
                selected=a.get("selected") == "true" if a.get("selected") is not None else None,
                clickable=a.get("clickable") == "true" if a.get("clickable") is not None else None,
                editable=(a.get("class") or "").endswith("EditText"),
                visible=visible,
                bounds=bounds,
                depth=depth,
                package=a.get("package") or "",
                index=len(out),
            )
            out.append(node)
            walk(child, depth + 1)

    walk(root, 0)
    return out


# --- phone: agent-device snapshot / live snapshot JSON -------------------

def phone_nodes_from_snapshot(doc):
    """agent-device `snapshot` JSON (`{data:{nodes:[...]}}`) -> flat node list.

    `label` is agent-device's *computed accessible name*; `text` (when the
    platform snapshot carries it) wins, exactly as in
    `tools/phone/replay_scenario.py` (TREE1: a computed label must never be
    presented as a node field when the real one is available).
    """
    data = doc.get("data") if isinstance(doc, dict) else None
    nodes = (data or {}).get("nodes") or doc.get("nodes") or []
    out = []
    for raw in nodes:
        out.append(make_node(
            resource_id=raw.get("identifier") or raw.get("resource_id") or "",
            class_name=raw.get("type") or raw.get("class") or "",
            text=raw.get("text") or raw.get("label") or "",
            desc=raw.get("contentDescription") or raw.get("content_desc") or "",
            hint=raw.get("placeholder") or "",
            enabled=raw.get("enabled"),
            checked=raw.get("checked"),
            focused=raw.get("focused"),
            selected=raw.get("selected"),
            clickable=raw.get("clickable"),
            editable=raw.get("editable"),
            visible=raw.get("visibleToUser", raw.get("visible", True)),
            bounds=raw.get("rect") or raw.get("bounds"),
            depth=raw.get("depth", 0),
            package=raw.get("bundleId") or raw.get("bundle")
            or raw.get("package") or "",
            index=len(out),
        ))
    return out


def split_windows(nodes):
    """Split a flat pre-order node list into per-window subtrees."""
    groups = []
    current = None
    for node in nodes:
        if node.get("depth") == 0:
            current = []
            groups.append(current)
        if current is not None:
            current.append(node)
    return groups


def select_window(nodes, windows_meta=None):
    """The window an agent is looking at, plus the others.

    ACOS's tree carries every window (`windows[]` with `z`); the topmost
    application window is the one whose UI an agent reads (a dialog covers the
    activity behind it). Without metadata the last root wins, which is the
    order both `uiautomator dump` and the agent-device snapshot append windows
    in (bottom to top).
    """
    groups = split_windows(nodes)
    if not groups:
        return [], []
    if windows_meta and len(windows_meta) == len(groups):
        index = max(range(len(groups)),
                    key=lambda i: (windows_meta[i].get("z", 0), i))
    else:
        index = len(groups) - 1
    others = [g for i, g in enumerate(groups) if i != index]
    return groups[index], others


def filter_package(nodes, package):
    """Keep the app's own window plus the framework decor nodes it hosts.

    A phone accessibility dump also contains other windows (the shade, the
    keyguard, SystemUI chrome). They are not the app under test, so they are
    dropped -- and the count is returned so a report can say how many.
    """
    keep = []
    dropped = 0
    for node in nodes:
        pkg = node.get("package") or ""
        if pkg == package or pkg == "android" or pkg == "":
            keep.append(node)
        else:
            dropped += 1
    return keep, dropped


# ---------------------------------------------------------------------------
# keys + matching
# ---------------------------------------------------------------------------

def node_key(node):
    """The stable identity used to pair a node with its counterpart.

    Order follows what makes an Android node addressable: its resource-id
    first, then the text an agent reads, then its content description. A node
    with none of those is a layout container, and its identity is its position
    in the tree: `class:<class>@<nearest ancestor with a resource-id>#<ordinal
    among same-class siblings>`. That keeps the third `LinearLayout` under
    `android:id/content` distinct from one under a dialog panel, which a bare
    `class:` key cannot.
    """
    if node.get("id"):
        return "id:" + node["id"]
    if node.get("text"):
        return "text:" + node["text"]
    if node.get("desc"):
        return "desc:" + node["desc"]
    parent = node.get("parent_path")
    if parent is None:
        return "class:" + (node.get("class") or role_of(node))
    return "class:%s@%s#%d" % (role_of(node), parent or "root",
                               node.get("ordinal", 0))


def annotate_tree(nodes):
    """Attach `parent_path`/`ordinal` to a pre-order single-window node list."""
    stack = []
    counters = {}
    for node in nodes:
        depth = node.get("depth", 0)
        while stack and stack[-1][0] >= depth:
            stack.pop()
        parent_path = stack[-1][1] if stack else ""
        strong = node.get("id") or node.get("text") or node.get("desc")
        own = node_key(node) if strong else parent_path
        node["parent_path"] = parent_path
        counter_key = (role_of(node), parent_path)
        node["ordinal"] = counters.get(counter_key, 0)
        counters[counter_key] = node["ordinal"] + 1
        stack.append((depth, own))
    return nodes


def _center(bounds):
    if not bounds:
        return (0.0, 0.0)
    return ((bounds[0] + bounds[2]) / 2.0, (bounds[1] + bounds[3]) / 2.0)


def _dist(a, b):
    ax, ay = _center(a.get("bounds_dp"))
    bx, by = _center(b.get("bounds_dp"))
    return abs(ax - bx) + abs(ay - by)


def _pair_by_proximity(left, right):
    """Greedy nearest-centre pairing for nodes that share a key."""
    pairs = []
    remaining = list(right)
    for a in left:
        if not remaining:
            break
        best = min(remaining, key=lambda p: _dist(a, p))
        pairs.append((a, best))
        remaining.remove(best)
    for b in remaining:
        pairs.append((None, b))
    return pairs


def key_is_positional(key, a, p):
    return a.get("parent_path") is not None and \
        a.get("parent_path") == p.get("parent_path") and \
        a.get("ordinal") == p.get("ordinal")


def _shift_node(node, shift):
    """Copy of a node whose dp bounds are translated by `shift`."""
    if not shift or not node.get("bounds_dp"):
        return node
    moved = dict(node)
    moved["bounds_dp"] = [round(node["bounds_dp"][0] + shift[0], 2),
                          round(node["bounds_dp"][1] + shift[1], 2),
                          round(node["bounds_dp"][2] + shift[0], 2),
                          round(node["bounds_dp"][3] + shift[1], 2)]
    return moved


def match_nodes(acos, phone, class_pair_dp=8.0, rescue_dp=6.0):
    """Pair ACOS and phone nodes.

    Returns `(pairs, missing_in_acos, extra_in_acos, stats)` where each pair is
    `(acos_node, phone_node, matched_by)` and `matched_by` is one of
    `root`, `id`, `text`, `desc`, `class`, or `rescue:<why>`.

    `missing_in_acos` holds nodes only the phone has; `extra_in_acos` holds
    nodes only ACOS has.

    Nodes with a resource-id, text or content-desc are paired by that key (the
    same identity an Android selector uses). Nodes that only have a class --
    layout containers -- are paired by position *inside the measured window
    frame*, so a dialog whose frame is centred 32 dp lower on one platform
    still pairs its containers instead of reporting every one as moved. Two
    containers further apart than `class_pair_dp` are left unpaired: a
    mispair invents a bounds difference and hides the real structural one.

    The rescue passes pair what is obviously the same node when one platform
    does not expose its id or label: identical text, identical content-desc,
    or same class and role within `rescue_dp`.
    """
    stats = {"acos_nodes": len(acos), "phone_nodes": len(phone)}
    pairs = []
    acos = annotate_tree(list(acos))
    phone = annotate_tree(list(phone))

    # The window root is the same node on both sides even when the concrete
    # class differs (ACOS exposes the framework DecorView, a phone snapshot
    # often starts at the decor's FrameLayout). Pair those first so the whole
    # tree is not shifted by one node.
    if acos and phone and acos[0].get("depth") == 0 and phone[0].get("depth") == 0:
        pairs.append((acos.pop(0), phone.pop(0), "root"))

    strong, weak = {}, {}
    for node in acos:
        bucket = strong if node_key(node).split(":", 1)[0] in ("id", "text", "desc") else weak
        bucket.setdefault(node_key(node), ([], []))[0].append(node)
    for node in phone:
        bucket = strong if node_key(node).split(":", 1)[0] in ("id", "text", "desc") else weak
        bucket.setdefault(node_key(node), ([], []))[1].append(node)

    for key in sorted(strong):
        left, right = strong[key]
        for a, p in _pair_by_proximity(list(left), list(right)):
            if a is not None and p is not None:
                pairs.append((a, p, "key"))
            elif a is not None:
                pairs.append((a, None, "unmatched"))
            else:
                pairs.append((None, p, "unmatched"))

    # Containers: nearest-first pairing in the measured window frame.
    shift = decor_shift([(a, p, why) for a, p, why in pairs
                         if a is not None and p is not None])
    weak_keys = sorted(set(weak) |
                       {k for k in weak if k in weak})
    class_candidates = []
    for key in weak_keys:
        left, right = weak.get(key, ([], []))
        for a in left:
            moved = _shift_node(a, shift)
            for p in right:
                class_candidates.append((_dist(moved, p), key, a, p))
    class_candidates.sort(key=lambda t: t[0])
    used_a, used_p = set(), set()
    for dist, key, a, p in class_candidates:
        # Same tree position (same class under the same identified ancestor,
        # same ordinal) *is* the same node; the distance only breaks ties.
        if dist > class_pair_dp and not key_is_positional(key, a, p):
            break
        if id(a) in used_a or id(p) in used_p:
            continue
        used_a.add(id(a))
        used_p.add(id(p))
        pairs.append((a, p, "class"))
    for key in weak_keys:
        left, right = weak.get(key, ([], []))
        for a in left:
            if id(a) not in used_a:
                pairs.append((a, None, "unmatched"))
        for p in right:
            if id(p) not in used_p:
                pairs.append((None, p, "unmatched"))

    def take(a, p, why):
        pairs.remove((a, None, "unmatched"))
        pairs.remove((None, p, "unmatched"))
        pairs.append((a, p, why))

    def unmatched_a():
        return [a for a, p, _ in pairs if p is None]

    def unmatched_p():
        return [p for a, p, _ in pairs if a is None]

    # rescue 1: identical non-empty text (id differs / one side has no id)
    for a in list(unmatched_a()):
        if not a.get("text"):
            continue
        moved = _shift_node(a, shift)
        candidates = [p for p in unmatched_p() if p.get("text") == a["text"]]
        if candidates:
            take(a, min(candidates, key=lambda p: _dist(moved, p)), "rescue:text")

    # rescue 2: identical non-empty content description
    for a in list(unmatched_a()):
        if not a.get("desc"):
            continue
        moved = _shift_node(a, shift)
        candidates = [p for p in unmatched_p() if p.get("desc") == a["desc"]]
        if candidates:
            take(a, min(candidates, key=lambda p: _dist(moved, p)), "rescue:desc")

    # rescue 3: same class+role, close on screen, both without text/desc
    for a in list(unmatched_a()):
        if a.get("text") or a.get("desc"):
            continue
        moved = _shift_node(a, shift)
        candidates = [p for p in unmatched_p()
                      if p.get("class") == a.get("class") and not p.get("text")
                      and not p.get("desc") and _dist(moved, p) <= rescue_dp]
        if candidates:
            take(a, min(candidates, key=lambda p: _dist(moved, p)),
                 "rescue:class+bounds")

    matched = [(a, p, why) for a, p, why in pairs if a is not None and p is not None]
    missing = [a for a, p, _ in pairs if p is None]
    extra = [p for a, p, _ in pairs if a is None]
    stats["pairs"] = len(matched)
    return matched, missing, extra, stats


# ---------------------------------------------------------------------------
# comparison
# ---------------------------------------------------------------------------

def _bounds_delta(a, p):
    if not a.get("bounds_dp") or not p.get("bounds_dp"):
        return None
    return [round(p["bounds_dp"][i] - a["bounds_dp"][i], 2) for i in range(4)]


def _is_translation(delta, tol):
    """(dx, dy) when both opposite edges move by the same amount."""
    if abs(delta[2] - delta[0]) > tol or abs(delta[3] - delta[1]) > tol:
        return None
    return [round((delta[0] + delta[2]) / 2.0, 2),
            round((delta[1] + delta[3]) / 2.0, 2)]


def diff_pair(a, p, matched_by, tol_dp=1.5, strict_class=False,
              decor_shift=None, display_a=None, display_p=None):
    """Field differences for one matched node pair (list of diff records)."""
    out = []
    base = {
        "key": node_key(a),
        "matched_by": matched_by,
        "acos": _brief(a),
        "phone": _brief(p),
    }

    def add(kind, **fields):
        rec = dict(base)
        rec["kind"] = kind
        rec.update(fields)
        out.append(rec)

    # text / content-desc. A name that moved between text and content-desc is
    # its own kind: both platforms expose the string, a *different* attribute
    # carries it (PHONE2 §5.1's text-field finding).
    if a["text"] != p["text"]:
        if a["text"] and not p["text"] and a["text"] == p["desc"]:
            add("text_vs_desc", acos=a["text"], phone=p["desc"],
                field="text->desc")
        elif p["text"] and not a["text"] and p["text"] == a["desc"]:
            add("text_vs_desc", acos=a["desc"], phone=p["text"],
                field="desc->text")
        else:
            add("text_changed", field="text", acos=a["text"], phone=p["text"])
    if a["desc"] != p["desc"]:
        if not (a["desc"] == p["text"] or p["desc"] == a["text"]):
            add("desc_changed", field="content_desc",
                acos=a["desc"], phone=p["desc"])

    # boolean state: only where both sides report a value
    for field in ("enabled", "checked", "focused", "selected"):
        av, pv = a.get(field), p.get(field)
        if av is None or pv is None:
            continue
        if bool(av) != bool(pv):
            add("state_changed", field=field, acos=bool(av), phone=bool(pv))

    # visibility
    if bool(a.get("visible")) != bool(p.get("visible")):
        add("visibility_changed", field="visible",
            acos=bool(a.get("visible")), phone=bool(p.get("visible")))

    # class: report a role change always; a concrete class change only under
    # --strict-class (the container/VC class name is a framework detail).
    if a.get("class") and p.get("class") and a.get("class") != p.get("class"):
        if role_of(a) != role_of(p):
            add("class_changed", field="class", acos=a.get("class"),
                phone=p.get("class"), acos_role=role_of(a),
                phone_role=role_of(p))
        elif strict_class:
            add("class_name_differs", field="class", acos=a.get("class"),
                phone=p.get("class"))

    # bounds in dp
    delta = _bounds_delta(a, p)
    if delta and max(abs(v) for v in delta) > tol_dp:
        rec = {
            "kind": "bounds_changed",
            "delta_dp": delta,
            "acos_dp": a["bounds_dp"],
            "phone_dp": p["bounds_dp"],
            "tol_dp": tol_dp,
        }
        # A node that fills the whole display on both sides has a different dp
        # extent whenever the two screens are a different size in dp (the
        # phone is 1080x2400 @400dpi = 432x960dp, the ACOS reference session
        # 1080x2400 @420dpi = 411x914dp). That is a density property, not a
        # layout difference, and the allowlist names it once.
        if display_a and display_p and \
                a.get("bounds_px") == list(display_a) and \
                p.get("bounds_px") == list(display_p):
            rec["spans_display"] = True
        shift = _is_translation(delta, tol_dp)
        if shift:
            rec["shift_dp"] = shift
        # A node that kept its size and moved exactly with the app frame is a
        # frame shift, not a layout difference. A node whose size changed
        # (the edge-to-edge window) is reported as a real bounds difference.
        if decor_shift and \
                abs(delta[0] - decor_shift[0]) <= tol_dp and \
                abs(delta[1] - decor_shift[1]) <= tol_dp and \
                abs((delta[2] - delta[0]) - 0.0) <= 2 * tol_dp and \
                abs((delta[3] - delta[1]) - 0.0) <= 2 * tol_dp:
            rec["explained_by"] = "decor_shift"
        # A match_parent / right-aligned node follows the display's dp size: it
        # is 20.6 dp wider on a 432 dp phone than on the 411 dp ACOS session.
        # The node is still where it belongs when the margin to its anchoring
        # display edge is the same in dp on both sides and the width change is
        # exactly the display's dp width change. Reported separately from a
        # real size difference.
        if not rec.get("explained_by") and decor_shift and display_a and \
                display_p and a.get("bounds_px") and p.get("bounds_px") and \
                a.get("dpi") and p.get("dpi"):
            wa = (display_a[2] - display_a[0]) * 160.0 / a["dpi"]
            wp = (display_p[2] - display_p[0]) * 160.0 / p["dpi"]
            ha = (display_a[3] - display_a[1]) * 160.0 / a["dpi"]
            hp = (display_p[3] - display_p[1]) * 160.0 / p["dpi"]
            dw = wp - wa
            dh = hp - ha
            rd = [delta[0] - decor_shift[0], delta[1] - decor_shift[1],
                  delta[2] - decor_shift[0], delta[3] - decor_shift[1]]
            ab, pb = a["bounds_dp"], p["bounds_dp"]
            left_anchor = abs(ab[0] - pb[0]) <= tol_dp
            right_anchor = abs((wa - ab[2]) - (wp - pb[2])) <= tol_dp
            top_anchor = abs(ab[1] - pb[1]) <= tol_dp
            bottom_anchor = abs((ha - ab[3]) - (hp - pb[3])) <= tol_dp
            # The node keeps its dp margin(s) to the anchoring display edge:
            # both margins (match_parent), or the right margin while the left
            # edge moves with the display width (right-aligned), or the mirror
            # case.
            width_follows = (left_anchor and right_anchor) or \
                (right_anchor and abs(rd[0] - dw) <= tol_dp) or \
                (left_anchor and abs(rd[2] - dw) <= tol_dp)
            height_follows = (top_anchor and bottom_anchor) or \
                (bottom_anchor and abs(rd[1] - dh) <= tol_dp) or \
                (top_anchor and abs(rd[3] - dh) <= tol_dp)
            if width_follows and abs(rd[1]) <= tol_dp and \
                    abs(rd[3] - rd[1]) <= 2 * tol_dp:
                rec["explained_by"] = "display_width"
            elif height_follows and abs(rd[0]) <= tol_dp and \
                    abs(rd[2] - rd[0]) <= 2 * tol_dp:
                rec["explained_by"] = "display_height"
        # Android 15 draws the app window edge-to-edge (its bottom is the
        # display bottom); the API-34 image ends the window above the
        # navigation bar. A full-width node that reaches the phone's display
        # bottom but not ACOS's is that platform change, not a layout bug.
        if not rec.get("spans_display") and display_a and display_p and \
                a.get("bounds_px") and p.get("bounds_px") and decor_shift:
            ab, pb = a["bounds_px"], p["bounds_px"]
            # The node's origin must be where it belongs: either translated
            # with the window frame or at the display origin on both sides
            # (the decor container starts at y=0 and spans the status bar).
            origin_ok = (abs(delta[0] - decor_shift[0]) <= tol_dp and
                         abs(delta[1] - decor_shift[1]) <= tol_dp) or \
                        (abs(delta[0]) <= tol_dp and abs(delta[1]) <= tol_dp)
            if origin_ok and pb[3] == display_p[3] and ab[3] != display_a[3]:
                rec["explained_by"] = "edge_to_edge"
        out.append({**base, **rec})

    return out


def _brief(node):
    return {
        "key": node_key(node),
        "id": node.get("id", ""),
        "class": node.get("class", ""),
        "role": role_of(node),
        "text": node.get("text", ""),
        "desc": node.get("desc", ""),
        "visible": node.get("visible"),
        "bounds_px": node.get("bounds_px"),
        "bounds_dp": node.get("bounds_dp"),
        "depth": node.get("depth"),
    }


def missing_record(node, side):
    """A `missing_in_acos` / `extra_in_acos` difference record."""
    kind = "missing_in_acos" if side == "phone" else "extra_in_acos"
    return {
        "kind": kind,
        "key": node_key(node),
        "node": _brief(node),
        "reason": ("the phone shows this node and ACOS does not"
                   if side == "phone" else
                   "ACOS shows this node and the phone does not"),
    }


# ---------------------------------------------------------------------------
# window-frame shift
# ---------------------------------------------------------------------------

FRAME_IDS = (
    "android:id/content",
    "android:id/decor_content_parent",
    "android:id/statusBarBackground",
    "android:id/navigationBarBackground",
)


def decor_shift(matched):
    """Measured window-frame origin shift (phone - ACOS, dp) from the decor.

    The status/navigation bar insets are the platform's, not the app's. When
    the two sides put the app frame at a different origin, every node in it
    moves by the same (dx, dy). The *content* node's top-left edge is the app
    frame's origin, and it is used even when the frame's size also differs
    (edge-to-edge phone window vs the API-34 image's inset window): the origin
    shift still applies to every child.
    """
    for want in FRAME_IDS:
        for a, p, _ in matched:
            if a.get("id") != want or p.get("id") != want:
                continue
            delta = _bounds_delta(a, p)
            if not delta:
                continue
            return [round(delta[0], 2), round(delta[1], 2)]
    return None


# ---------------------------------------------------------------------------
# allowlist
# ---------------------------------------------------------------------------

def _glob_match(pattern, value):
    if pattern is None:
        return True
    if isinstance(pattern, (list, tuple)):
        return any(_glob_match(p, value) for p in pattern)
    import fnmatch
    return fnmatch.fnmatchcase(value or "", str(pattern))


def diff_field_value(diff, entry_match):
    """The value an allowlist `node`/`field` match is tested against."""
    node = diff.get("node") or diff.get("acos") or {}
    key = diff.get("key") or node.get("key") or ""
    return key


def allowlist_match(entry, diff, scenario_name, phone_source=None):
    """Does one allowlist entry cover this difference record?"""
    m = entry.get("match") or {}
    if not _glob_match(m.get("scenario"), scenario_name):
        return False
    if m.get("phone_source") is not None and \
            not _glob_match(m.get("phone_source"), phone_source):
        return False
    if not _glob_match(m.get("kind"), diff.get("kind")):
        return False
    if not _glob_match(m.get("node"), diff.get("key") or ""):
        return False
    if m.get("field") is not None and not _glob_match(m.get("field"),
                                                      diff.get("field")):
        return False
    if m.get("explained_by") is not None and m.get("explained_by") != \
            diff.get("explained_by"):
        return False
    limit = m.get("max_delta_dp")
    if limit is not None:
        delta = diff.get("delta_dp")
        if not delta or max(abs(v) for v in delta) > float(limit):
            return False
    # `visible: false` restricts an entry to nodes the ACOS side itself marks
    # as not on screen (a zero-width overflow menu, a collapsed context bar).
    if m.get("acos_visible") is not None:
        node = diff.get("node") or diff.get("acos") or {}
        if bool(node.get("visible")) != bool(m.get("acos_visible")):
            return False
    if m.get("spans_display") is not None:
        if bool(diff.get("spans_display")) != bool(m.get("spans_display")):
            return False
    return True


def apply_allowlist(diffs, allowlist, scenario_name, phone_source=None):
    """Mark each difference `allowed`/`allowed_by`, or leave it unexpected."""
    entries = (allowlist or {}).get("entries") or []
    for diff in diffs:
        diff["allowed"] = False
        diff["allowed_by"] = None
        diff["allow_reason"] = None
        for entry in entries:
            if allowlist_match(entry, diff, scenario_name, phone_source):
                diff["allowed"] = True
                diff["allowed_by"] = entry.get("id")
                diff["allow_reason"] = entry.get("reason")
                break
    return diffs
