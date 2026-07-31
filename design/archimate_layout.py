#!/usr/bin/env python3
"""
archimate_layout.py
===================
Force-directed layout tool for Archi (.archimate) XML files.

MODES
-----
visualize
    Parse the file, extract one view (or all views), run a Fruchterman–Reingold
    spring layout, and write:
        <prefix>[_<ViewName>].svg   – lightweight circle-and-line SVG preview
        <prefix>.json               – layout data reusable by the "apply" mode

apply
    Read the layout JSON produced by "visualize" and patch the <bounds> elements
    in the .archimate file, writing a new *_layouted.archimate alongside the
    original (the original file is never modified).

USAGE
-----
    python archimate_layout.py visualize  model.archimate --view "Application View"
    python archimate_layout.py visualize  model.archimate --all-views --output out/layout
    python archimate_layout.py visualize  model.archimate --view "App View" --tightness 0.6
    python archimate_layout.py apply      model.archimate layout.json
    python archimate_layout.py apply      model.archimate layout.json --output patched.archimate

TIGHTNESS
---------
    --tightness  (float, default 1.0)

    Scales the ideal spring length used by the Fruchterman–Reingold algorithm.
    The default value of 1.0 already produces a compact layout — nodes are close
    but never overlapping, with a minimum gap of ~10 % of each node's own width.

        < 1.0  →  tighter / more compressed  (e.g. 0.5 for very dense)
        > 1.0  →  looser  / more spread out  (e.g. 2.0 for generous spacing)

    After the FR pass a post-layout overlap-resolution step enforces the minimum
    separation regardless of the tightness value chosen.

DEPENDENCIES
------------
    Required : Python 3.8+
    Optional : networkx  (pip install networkx)
               When present, uses networkx.spring_layout (Fruchterman–Reingold
               with the standard scipy solver).  Falls back to a built-in
               pure-Python FR implementation automatically.
"""

from __future__ import annotations

import argparse
import json
import math
import random
import sys
import xml.etree.ElementTree as ET
from pathlib import Path
from typing import Dict, List, Optional, Tuple

# ---------------------------------------------------------------------------
# Namespace constants
# ---------------------------------------------------------------------------
NS_XSI  = "http://www.w3.org/2001/XMLSchema-instance"
NS_ARCH = "http://www.archimatetool.com/archimate"

ET.register_namespace("xsi",       NS_XSI)
ET.register_namespace("archimate", NS_ARCH)

XSI_TYPE = f"{{{NS_XSI}}}type"

NodeMap  = Dict[str, dict]
EdgeList = List[dict]

CANVAS_W = 1400
CANVAS_H = 1000

# Minimum gap between nodes expressed as a fraction of the node's own width.
# 0.10 = 10 % — enforced by the post-layout overlap-resolution pass.
MIN_GAP_FRACTION = 0.10


# ===========================================================================
# PARSING
# ===========================================================================

def parse_file(path: str) -> Tuple[ET.ElementTree, ET.Element, dict]:
    """
    Parse an .archimate XML file.

    Returns
    -------
    tree        : ElementTree (used for patching and re-serialising)
    root        : root Element
    element_map : id -> {'name': str, 'type': str}
    """
    tree = ET.parse(path)
    root = tree.getroot()

    element_map: dict = {}
    for elem in root.iter():
        eid = elem.get("id")
        if eid:
            element_map[eid] = {
                "name": elem.get("name", ""),
                "type": elem.get(XSI_TYPE, ""),
            }

    return tree, root, element_map


def extract_views(root: ET.Element,
                  element_map: dict,
                  view_name: Optional[str] = None) -> List[dict]:
    """Return a list of parsed view dicts (filtered by name when given)."""
    views: List[dict] = []
    for elem in root.iter():
        etype = elem.get(XSI_TYPE, "")
        if "DiagramModel" in etype or "SketchModel" in etype:
            name = elem.get("name", "")
            if view_name is None or name == view_name:
                views.append(_build_view(elem, element_map))
    return views


def _build_view(view_elem: ET.Element, element_map: dict) -> dict:
    view: dict = {
        "id":    view_elem.get("id", ""),
        "name":  view_elem.get("name", "Unnamed"),
        "nodes": {},
        "edges": [],
    }
    _recurse_children(view_elem, element_map, view,
                      off_x=0, off_y=0, parent_id=None)
    return view


def _recurse_children(parent: ET.Element,
                       element_map: dict,
                       view: dict,
                       off_x: int,
                       off_y: int,
                       parent_id: Optional[str]) -> None:
    """
    Walk <child> elements recursively, accumulating absolute coordinates.
    """
    for child in parent:
        local = child.tag.split("}")[-1] if "}" in child.tag else child.tag
        if local != "child":
            continue

        cid = child.get("id", "")
        if not cid:
            continue

        bounds = child.find("bounds")
        if bounds is None:
            continue

        rel_x = int(float(bounds.get("x", 0)))
        rel_y = int(float(bounds.get("y", 0)))
        bw    = int(float(bounds.get("width",  120)))
        bh    = int(float(bounds.get("height",  55)))
        abs_x = rel_x + off_x
        abs_y = rel_y + off_y

        arch_ref = child.get("archimateElement", "")
        if arch_ref and arch_ref in element_map:
            label = element_map[arch_ref]["name"]
            etype = element_map[arch_ref]["type"]
        else:
            label = child.get("name", "")
            etype = child.get(XSI_TYPE, "")

        view["nodes"][cid] = {
            "id":        cid,
            "label":     label,
            "abs_x":     abs_x,
            "abs_y":     abs_y,
            "w":         bw,
            "h":         bh,
            "type":      etype,
            "parent_id": parent_id,
        }

        for conn in child:
            cloc = conn.tag.split("}")[-1] if "}" in conn.tag else conn.tag
            if cloc == "sourceConnection":
                src = conn.get("source", cid)
                tgt = conn.get("target", "")
                eid = conn.get("id", "")
                if tgt:
                    view["edges"].append({"id": eid, "source": src, "target": tgt})

        _recurse_children(child, element_map, view,
                          off_x=abs_x, off_y=abs_y, parent_id=cid)


# ===========================================================================
# FORCE-DIRECTED LAYOUT
# ===========================================================================

def _fr_layout_builtin(nodes: NodeMap,
                        edges: EdgeList,
                        width: int,
                        height: int,
                        tightness: float = 1.0,
                        iterations: int = 350,
                        seed: int = 42) -> Dict[str, Tuple[float, float]]:
    """
    Pure-Python Fruchterman–Reingold spring layout.

    The *tightness* parameter scales the ideal spring length *k*:
        tightness < 1  →  shorter springs  →  nodes pulled closer together
        tightness > 1  →  longer  springs  →  nodes pushed further apart

    The base multiplier (0.35) is intentionally small so that tightness=1.0
    already produces a compact, Archi-friendly layout.

    Returns a dict of node_id -> (center_x, center_y).
    """
    if not nodes:
        return {}

    random.seed(seed)
    ids  = list(nodes)
    n    = len(ids)
    pad  = 80
    W, H = max(width  - 2 * pad, 100), max(height - 2 * pad, 100)

    pos = {nid: [random.random() * W + pad,
                 random.random() * H + pad]
           for nid in ids}

    # Base spring length — 0.35 gives a compact default; scaled by tightness.
    k  = math.sqrt(W * H / max(n, 1)) * 0.35 * tightness
    t  = 0.12 * max(W, H)          # initial temperature
    dt = t / (iterations + 1)

    edge_pairs = {
        (e["source"], e["target"])
        for e in edges
        if e["source"] in pos and e["target"] in pos
    }

    for _ in range(iterations):
        disp = {nid: [0.0, 0.0] for nid in ids}

        # ── Repulsive forces (all pairs) ─────────────────────────────────
        for i in range(n):
            for j in range(i + 1, n):
                a, b = ids[i], ids[j]
                dx = pos[a][0] - pos[b][0]
                dy = pos[a][1] - pos[b][1]
                dist = math.hypot(dx, dy) or 0.01
                f = (k * k) / dist
                ux, uy = dx / dist, dy / dist
                disp[a][0] += ux * f
                disp[a][1] += uy * f
                disp[b][0] -= ux * f
                disp[b][1] -= uy * f

        # ── Attractive forces (edges) ────────────────────────────────────
        for src, tgt in edge_pairs:
            dx = pos[src][0] - pos[tgt][0]
            dy = pos[src][1] - pos[tgt][1]
            dist = math.hypot(dx, dy) or 0.01
            f = (dist * dist) / k
            ux, uy = dx / dist, dy / dist
            disp[src][0] -= ux * f
            disp[src][1] -= uy * f
            disp[tgt][0] += ux * f
            disp[tgt][1] += uy * f

        # ── Apply displacement (capped by temperature) ───────────────────
        for nid in ids:
            dm = math.hypot(*disp[nid]) or 0.01
            scale = min(dm, t) / dm
            pos[nid][0] += disp[nid][0] * scale
            pos[nid][1] += disp[nid][1] * scale
            pos[nid][0] = max(pad, min(width  - pad, pos[nid][0]))
            pos[nid][1] = max(pad, min(height - pad, pos[nid][1]))

        t = max(t - dt, 1.0)

    return {nid: (pos[nid][0], pos[nid][1]) for nid in ids}


def _nx_layout(nodes: NodeMap,
               edges: EdgeList,
               width: int,
               height: int,
               tightness: float = 1.0,
               seed: int = 42) -> Dict[str, Tuple[float, float]]:
    """networkx spring_layout wrapper (used when networkx is available)."""
    import networkx as nx  # type: ignore

    G = nx.DiGraph()
    G.add_nodes_from(nodes.keys())
    for e in edges:
        if e["source"] in nodes and e["target"] in nodes:
            G.add_edge(e["source"], e["target"])

    pad = 80
    W, H = max(width - 2 * pad, 100), max(height - 2 * pad, 100)
    n    = max(len(nodes), 1)

    # Mirror the same base-multiplier logic used by the built-in FR.
    k_val = math.sqrt(W * H / n) * 0.35 * tightness / max(W, H)

    raw = nx.spring_layout(G, k=k_val, iterations=350, seed=seed)

    # networkx returns values in [-1, 1]; rescale to canvas pixels.
    result: Dict[str, Tuple[float, float]] = {}
    for nid, (nx_x, nx_y) in raw.items():
        cx = (nx_x + 1) / 2 * W + pad
        cy = (nx_y + 1) / 2 * H + pad
        result[nid] = (cx, cy)
    return result


# ---------------------------------------------------------------------------
# Overlap resolution
# ---------------------------------------------------------------------------

def _resolve_overlaps(pos: Dict[str, Tuple[float, float]],
                       nodes: NodeMap,
                       min_gap_fraction: float = MIN_GAP_FRACTION,
                       iterations: int = 120) -> Dict[str, Tuple[float, float]]:
    """
    Post-layout AABB overlap-resolution pass.

    Pushes overlapping node pairs apart so that every node has at least
    *min_gap_fraction × own_width* of clear space on each side.  The push is
    split equally between the two nodes and applied along the axis of minimum
    penetration (separating axis theorem, 2-D AABB variant).

    Runs for up to *iterations* sweeps; exits early once no overlaps remain.
    """
    # Work with a mutable copy
    mpos: Dict[str, List[float]] = {k: list(v) for k, v in pos.items()}
    ids  = [nid for nid in mpos if nid in nodes]

    for _ in range(iterations):
        moved = False

        for i in range(len(ids)):
            for j in range(i + 1, len(ids)):
                id_a, id_b = ids[i], ids[j]

                na, nb = nodes[id_a], nodes[id_b]
                wa, ha = na["w"], na["h"]
                wb, hb = nb["w"], nb["h"]

                # Minimum allowed gap between edges = 10 % of each node's width
                gap_a = wa * min_gap_fraction
                gap_b = wb * min_gap_fraction
                # Use the average so the constraint is symmetric
                gap = (gap_a + gap_b) / 2

                cx_a, cy_a = mpos[id_a]
                cx_b, cy_b = mpos[id_b]

                half_w = (wa + wb) / 2 + gap
                half_h = (ha + hb) / 2 + gap

                dx = cx_b - cx_a
                dy = cy_b - cy_a

                pen_x = half_w - abs(dx)
                pen_y = half_h - abs(dy)

                if pen_x <= 0 or pen_y <= 0:
                    continue  # no overlap

                moved = True

                # Separate along the axis with the smaller penetration
                if pen_x <= pen_y:
                    push = pen_x / 2 + 0.5   # tiny extra to avoid exact-edge touching
                    if dx >= 0:
                        mpos[id_a][0] -= push
                        mpos[id_b][0] += push
                    else:
                        mpos[id_a][0] += push
                        mpos[id_b][0] -= push
                else:
                    push = pen_y / 2 + 0.5
                    if dy >= 0:
                        mpos[id_a][1] -= push
                        mpos[id_b][1] += push
                    else:
                        mpos[id_a][1] += push
                        mpos[id_b][1] -= push

        if not moved:
            break

    return {k: (v[0], v[1]) for k, v in mpos.items()}


# ---------------------------------------------------------------------------
# Public layout entry-point
# ---------------------------------------------------------------------------

def compute_layout(view: dict,
                   width: int = CANVAS_W,
                   height: int = CANVAS_H,
                   tightness: float = 1.0) -> Dict[str, Tuple[float, float]]:
    """
    Run force-directed layout for *view*, then resolve overlaps.

    Returns a dict of node_id -> (center_x, center_y) in canvas pixels.
    """
    nodes: NodeMap = view["nodes"]
    edges: EdgeList = view["edges"]

    try:
        import networkx  # noqa: F401
        raw = _nx_layout(nodes, edges, width, height, tightness)
    except ImportError:
        raw = _fr_layout_builtin(nodes, edges, width, height, tightness)

    # Enforce minimum separation after FR has converged
    resolved = _resolve_overlaps(raw, nodes)
    return resolved


# ===========================================================================
# SVG RENDERING
# ===========================================================================

# ArchiMate layer → fill colour (dark theme)
_LAYER_COLOUR = {
    "Application": "#2a6db5",
    "Business":    "#c0392b",
    "Technology":  "#27ae60",
    "Motivation":  "#d35400",
    "Implementation": "#8e44ad",
    "Migration":   "#8e44ad",
}
_DEFAULT_COLOUR = "#555e6e"


def _node_colour(node_type: str) -> str:
    for layer, colour in _LAYER_COLOUR.items():
        if layer.lower() in node_type.lower():
            return colour
    return _DEFAULT_COLOUR


def _truncate(text: str, max_chars: int = 18) -> str:
    return text if len(text) <= max_chars else text[:max_chars - 1] + "…"


def render_svg(view: dict,
               pos: Dict[str, Tuple[float, float]],
               width: int = CANVAS_W,
               height: int = CANVAS_H) -> str:
    """
    Produce a lightweight SVG string: circles for nodes, lines for edges,
    brief text labels.  Dark background, colour-coded by ArchiMate layer.
    """
    nodes: NodeMap  = view["nodes"]
    edges: EdgeList = view["edges"]

    # Compute actual bounding box of laid-out nodes so we can crop the canvas
    if pos:
        xs = [p[0] for p in pos.values()]
        ys = [p[1] for p in pos.values()]
        margin = 60
        vx = max(0, min(xs) - margin)
        vy = max(0, min(ys) - margin)
        vw = min(width,  max(xs) - min(xs) + 2 * margin)
        vh = min(height, max(ys) - min(ys) + 2 * margin)
    else:
        vx, vy, vw, vh = 0, 0, width, height

    lines: List[str] = [
        f'<svg xmlns="http://www.w3.org/2000/svg" '
        f'viewBox="{vx:.0f} {vy:.0f} {vw:.0f} {vh:.0f}" '
        f'width="{vw:.0f}" height="{vh:.0f}">',
        f'<rect x="{vx:.0f}" y="{vy:.0f}" width="{vw:.0f}" height="{vh:.0f}" fill="#1e2330"/>',
        # Arrow-head marker
        '<defs>'
        '<marker id="arr" markerWidth="8" markerHeight="8" refX="8" refY="3" orient="auto">'
        '<path d="M0,0 L0,6 L8,3 z" fill="#7a8394"/>'
        '</marker>'
        '</defs>',
    ]

    # ── Edges ────────────────────────────────────────────────────────────────
    R = 22   # node circle radius (used for arrowhead offset)
    for edge in edges:
        src, tgt = edge["source"], edge["target"]
        if src not in pos or tgt not in pos:
            continue
        x1, y1 = pos[src]
        x2, y2 = pos[tgt]
        # Offset line end by circle radius so arrow lands on the circumference
        dist = math.hypot(x2 - x1, y2 - y1) or 1
        ox = (x2 - x1) / dist * R
        oy = (y2 - y1) / dist * R
        lines.append(
            f'<line x1="{x1:.1f}" y1="{y1:.1f}" '
            f'x2="{x2 - ox:.1f}" y2="{y2 - oy:.1f}" '
            f'stroke="#7a8394" stroke-width="1.5" marker-end="url(#arr)"/>'
        )

    # ── Nodes ────────────────────────────────────────────────────────────────
    for nid, (cx, cy) in pos.items():
        if nid not in nodes:
            continue
        node   = nodes[nid]
        colour = _node_colour(node["type"])
        label  = _truncate(node["label"] or nid)
        lines.append(
            f'<circle cx="{cx:.1f}" cy="{cy:.1f}" r="{R}" '
            f'fill="{colour}" stroke="#e0e6f0" stroke-width="1.5"/>'
        )
        lines.append(
            f'<text x="{cx:.1f}" y="{cy + R + 12:.1f}" '
            f'text-anchor="middle" font-family="sans-serif" font-size="10" '
            f'fill="#c8d0e0">{label}</text>'
        )

    lines.append("</svg>")
    return "\n".join(lines)


# ===========================================================================
# APPLY MODE – patch <bounds> in the .archimate XML
# ===========================================================================

def apply_layout(archimate_path: str,
                 layout_json_path: str,
                 output_path: Optional[str] = None) -> str:
    """
    Read layout JSON and patch the matching <bounds> elements in the
    .archimate XML.  Writes a new *_layouted.archimate file (never overwrites
    the original).

    Returns the path of the written file.
    """
    with open(layout_json_path) as f:
        layout_data = json.load(f)

    tree, root, element_map = parse_file(archimate_path)

    # Build a lookup: child_id -> new (abs_x, abs_y, w, h)
    patch: Dict[str, dict] = {}
    for view_entry in layout_data.get("views", []):
        for nid, info in view_entry.get("positions", {}).items():
            patch[nid] = info

    # Walk all <child> elements and update <bounds>
    for child in root.iter():
        local = child.tag.split("}")[-1] if "}" in child.tag else child.tag
        if local != "child":
            continue
        cid = child.get("id", "")
        if cid not in patch:
            continue
        info   = patch[cid]
        bounds = child.find("bounds")
        if bounds is None:
            bounds = ET.SubElement(child, "bounds")
        bounds.set("x",      str(int(info["new_x"])))
        bounds.set("y",      str(int(info["new_y"])))
        bounds.set("width",  str(int(info["w"])))
        bounds.set("height", str(int(info["h"])))

    # Determine output path
    if output_path is None:
        p = Path(archimate_path)
        output_path = str(p.with_name(p.stem + "_layouted" + p.suffix))

    tree.write(output_path, encoding="unicode", xml_declaration=True)
    return output_path


# ===========================================================================
# VISUALIZE MODE
# ===========================================================================

def visualize(args: argparse.Namespace) -> None:
    src_path = args.archimate_file
    output   = args.output or Path(src_path).stem

    tree, root, element_map = parse_file(src_path)

    if args.all_views:
        views = extract_views(root, element_map)
    else:
        vname = args.view
        views = extract_views(root, element_map, view_name=vname)
        if not views:
            print(f"ERROR: view '{vname}' not found.", file=sys.stderr)
            _list_views(root)
            sys.exit(1)

    if not views:
        print("ERROR: no views found in file.", file=sys.stderr)
        sys.exit(1)

    all_positions: List[dict] = []

    for view in views:
        safe_name = view["name"].replace(" ", "_").replace("/", "-")
        svg_path  = f"{output}_{safe_name}.svg" if len(views) > 1 else f"{output}.svg"

        print(f"  Laying out view '{view['name']}' "
              f"({len(view['nodes'])} nodes, {len(view['edges'])} edges) "
              f"tightness={args.tightness:.2f} …")

        pos = compute_layout(view, tightness=args.tightness)

        svg = render_svg(view, pos)
        Path(svg_path).write_text(svg, encoding="utf-8")
        print(f"    SVG  → {svg_path}")

        # Build position records for JSON
        nodes = view["nodes"]
        view_positions: dict = {}
        for nid, (cx, cy) in pos.items():
            if nid not in nodes:
                continue
            n     = nodes[nid]
            new_x = cx - n["w"] / 2
            new_y = cy - n["h"] / 2
            view_positions[nid] = {
                "new_x": round(new_x, 1),
                "new_y": round(new_y, 1),
                "w":     n["w"],
                "h":     n["h"],
                "label": n["label"],
            }
        all_positions.append({"view_id": view["id"],
                               "view_name": view["name"],
                               "positions": view_positions})

    json_path = f"{output}.json"
    with open(json_path, "w") as f:
        json.dump({"tightness": args.tightness, "views": all_positions},
                  f, indent=2)
    print(f"  JSON → {json_path}")


def _list_views(root: ET.Element) -> None:
    print("Available views:", file=sys.stderr)
    for elem in root.iter():
        etype = elem.get(XSI_TYPE, "")
        if "DiagramModel" in etype or "SketchModel" in etype:
            print(f"  • {elem.get('name', '(unnamed)')}", file=sys.stderr)


# ===========================================================================
# CLI
# ===========================================================================

def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="archimate_layout.py",
        description="Force-directed layout for Archi .archimate files.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    sub = p.add_subparsers(dest="mode", required=True)

    # ── visualize ────────────────────────────────────────────────────────────
    vis = sub.add_parser("visualize",
                         help="Run layout and generate SVG + JSON")
    vis.add_argument("archimate_file", help="Path to the .archimate file")
    vis.add_argument("--view",      metavar="NAME",
                     help="Name of the view to lay out (required unless --all-views)")
    vis.add_argument("--all-views", action="store_true",
                     help="Lay out every view in the file")
    vis.add_argument("--output",    metavar="PREFIX",
                     help="Output path prefix (default: <filename stem>)")
    vis.add_argument(
        "--tightness",
        type=float,
        default=1.0,
        metavar="T",
        help=(
            "Controls node spacing (default: 1.0 = compact but non-overlapping). "
            "Values < 1.0 pack nodes tighter; values > 1.0 spread them further apart. "
            "A post-layout pass always enforces a minimum gap of 10%% of each node's "
            "width, regardless of this setting."
        ),
    )

    # ── apply ────────────────────────────────────────────────────────────────
    app = sub.add_parser("apply",
                         help="Patch .archimate bounds from a layout JSON")
    app.add_argument("archimate_file",  help="Path to the .archimate file")
    app.add_argument("layout_json",     help="Path to layout JSON from visualize")
    app.add_argument("--output",        metavar="PATH",
                     help="Output .archimate path (default: <stem>_layouted.archimate)")

    return p


def main() -> None:
    parser = build_parser()
    args   = parser.parse_args()

    if args.mode == "visualize":
        if not args.view and not args.all_views:
            parser.error("Specify --view <name> or --all-views.")
        if args.tightness <= 0:
            parser.error("--tightness must be a positive number.")
        visualize(args)

    elif args.mode == "apply":
        out = apply_layout(args.archimate_file, args.layout_json,
                           getattr(args, "output", None))
        print(f"Patched file written to: {out}")


if __name__ == "__main__":
    main()
