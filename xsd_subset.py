#!/usr/bin/env python3
"""
xsd_subset.py - Extract a subset of UCI XSD elements and their transitive type dependencies.

Parses the UCI message XSD (and any xs:include'd files), resolves the full transitive
closure of types needed for the requested elements, and emits a self-contained subset XSD
with exact source text preserved for each definition.

Usage:
    python3 xsd_subset.py SystemStatus SystemStatusRequest -o subset.xsd
    python3 xsd_subset.py SystemStatus --schema rcal/schema/UCI_MessageDefinitions_v2_5_0.xsd
"""

import argparse
import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

# ── constants ──────────────────────────────────────────────────────────────────

XS_NS = "http://www.w3.org/2001/XMLSchema"
UCI_NS = "https://www.vdl.afrl.af.mil/programs/oam"
DEFAULT_SCHEMA = Path(__file__).parent / "rcal/schema/UCI_MessageDefinitions_v2_5_0.xsd"

# Tags that can appear at top level with a name= attribute
TOP_LEVEL_TAGS = {"element", "complexType", "simpleType", "group", "attributeGroup", "attribute"}

# Regex for a top-level definition line (one leading tab, then <xs:TAG name="...")
_TOP_DEF_RE = re.compile(
    r"^\t<xs:(" + "|".join(TOP_LEVEL_TAGS) + r")\b"
)
_NAME_RE = re.compile(r'\bname="([^"]+)"')
_TAG_RE = re.compile(r"^\t<(xs:\w+)")


# ── block extractor ────────────────────────────────────────────────────────────

def extract_blocks(path: Path) -> dict[str, str]:
    """
    Return {name: exact_xml_text} for every top-level named definition.
    Relies on the UCI XSD convention: top-level items start at exactly one tab of indent.
    Handles CRLF line endings.
    """
    raw = path.read_bytes().decode("utf-8-sig")  # strip BOM if present
    # Normalise to LF so our regex anchors work; we'll keep original line endings in output
    # by working on the original lines split on \n (CRLF becomes "line\r")
    lines = raw.splitlines(keepends=True)

    blocks: dict[str, str] = {}
    current_name: str | None = None
    current_start: int = 0

    for i, line in enumerate(lines):
        # Strip CRLF for matching but keep original in output
        stripped = line.rstrip("\r\n")
        if _TOP_DEF_RE.match(stripped):
            if current_name is not None:
                blocks[current_name] = "".join(lines[current_start:i])
            m = _NAME_RE.search(stripped)
            if m:
                current_name = m.group(1)
                current_start = i
            else:
                current_name = None
        elif current_name and "</xs:schema>" in stripped:
            blocks[current_name] = "".join(lines[current_start:i])
            current_name = None

    if current_name is not None:
        blocks[current_name] = "".join(lines[current_start:])

    return blocks


def extract_schema_header(path: Path) -> str:
    """
    Return the opening <xs:schema ...> tag lines (up to and including the first top-level def).
    Strips xs:include lines and top-level xs:annotation/comment blocks so we can re-emit clean.
    """
    raw = path.read_bytes().decode("utf-8-sig")
    lines = raw.splitlines(keepends=True)
    header_lines: list[str] = []
    for line in lines:
        stripped = line.rstrip("\r\n")
        if _TOP_DEF_RE.match(stripped):
            break
        # Drop includes — we flatten everything into one file
        if re.match(r"^\t<xs:include\b", stripped):
            continue
        header_lines.append(line)
    return "".join(header_lines)


# ── dependency resolver ────────────────────────────────────────────────────────

def _refs_in_elem(elem: ET.Element) -> set[str]:
    """
    Walk the ElementTree node and collect every referenced UCI type name
    (strips the namespace prefix; ignores xs: built-ins).
    """
    refs: set[str] = set()
    for node in elem.iter():
        for attr in ("type", "base", "ref", "itemType"):
            val = node.get(attr, "")
            if val and not val.startswith("xs:") and ":" in val:
                refs.add(val.split(":", 1)[1])
            elif val and ":" not in val and val:
                # unqualified reference (rare but possible)
                refs.add(val)
        mt = node.get("memberTypes", "")
        for token in mt.split():
            if token and not token.startswith("xs:") and ":" in token:
                refs.add(token.split(":", 1)[1])
    return refs


def build_named_map(schema_paths: list[Path]) -> dict[str, ET.Element]:
    """
    Parse all schema files and return {name: ET.Element} for every top-level named definition.
    Later files win on name collision (shouldn't happen in practice).
    """
    ET.register_namespace("xs", XS_NS)
    ET.register_namespace("uci", UCI_NS)
    named: dict[str, ET.Element] = {}
    for path in schema_paths:
        tree = ET.parse(path)
        root = tree.getroot()
        for child in root:
            name = child.get("name")
            if name:
                named[name] = child
    return named


def resolve_deps(element_names: list[str], named: dict[str, ET.Element]) -> set[str]:
    """
    Return the full transitive closure of type names needed for element_names.
    Missing names (e.g. built-ins) are silently skipped.
    """
    needed: set[str] = set()
    queue = list(element_names)
    while queue:
        name = queue.pop()
        if name in needed or name not in named:
            continue
        needed.add(name)
        for ref in _refs_in_elem(named[name]):
            if ref not in needed:
                queue.append(ref)
    return needed


# ── main ───────────────────────────────────────────────────────────────────────

def main() -> None:
    parser = argparse.ArgumentParser(
        description="Extract a subset XSD containing only the specified elements and their transitive type deps."
    )
    parser.add_argument(
        "elements",
        nargs="+",
        metavar="ELEMENT",
        help="Top-level xs:element names to keep (message names)",
    )
    parser.add_argument(
        "--schema",
        type=Path,
        default=DEFAULT_SCHEMA,
        help=f"Path to the main XSD file (default: {DEFAULT_SCHEMA})",
    )
    parser.add_argument(
        "-o", "--output",
        type=Path,
        default=None,
        help="Output file (default: stdout)",
    )
    args = parser.parse_args()

    main_schema: Path = args.schema
    if not main_schema.exists():
        sys.exit(f"Schema not found: {main_schema}")

    # Discover xs:include'd files relative to the main schema
    include_paths: list[Path] = []
    raw_header = main_schema.read_bytes().decode("utf-8-sig")
    for m in re.finditer(r'<xs:include\s+schemaLocation="([^"]+)"', raw_header):
        inc = main_schema.parent / m.group(1)
        if inc.exists():
            include_paths.append(inc)
        else:
            print(f"Warning: included schema not found: {inc}", file=sys.stderr)

    all_schema_paths = include_paths + [main_schema]

    # Build a single merged map of all named definitions
    named = build_named_map(all_schema_paths)

    # Validate requested elements exist
    missing = [e for e in args.elements if e not in named]
    if missing:
        sys.exit(f"Error: elements not found in schema: {', '.join(missing)}")

    # Resolve the full transitive dependency set
    needed_names = resolve_deps(args.elements, named)

    # Separate requested top-level elements from supporting types
    requested_elements = set(args.elements)
    support_types = needed_names - requested_elements

    # Extract raw text blocks from source files
    all_blocks: dict[str, str] = {}
    for path in all_schema_paths:
        all_blocks.update(extract_blocks(path))

    # Build output
    parts: list[str] = []
    parts.append(extract_schema_header(main_schema))

    # Elements first (the "messages")
    parts.append("\t<!--== START MESSAGES ==-->\n")
    for name in sorted(requested_elements):
        if name in all_blocks:
            parts.append(all_blocks[name])
        else:
            print(f"Warning: no raw block found for element '{name}'", file=sys.stderr)

    # Supporting types
    if support_types:
        parts.append("\t<!--== SUPPORTING TYPES ==-->\n")
        for name in sorted(support_types):
            if name in all_blocks:
                parts.append(all_blocks[name])
            # else it's a built-in or unknown — silently skip

    parts.append("</xs:schema>\n")

    output = "".join(parts)

    if args.output:
        args.output.write_text(output, encoding="utf-8")
        # Summary to stderr so stdout remains clean when piping
        print(
            f"Wrote {args.output}: {len(requested_elements)} element(s), "
            f"{len(support_types)} supporting type(s)",
            file=sys.stderr,
        )
    else:
        sys.stdout.write(output)


if __name__ == "__main__":
    main()
