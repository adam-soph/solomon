#!/usr/bin/env python3
"""Split a stdlib module lib/<m>.hc into lib/<m>.hh (interface) + slimmed lib/<m>.hc (impl).

Routing of each top-level construct (with its leading comment block):
  - `#define X`            -> .hh if public (not `_`-prefixed), else .hc
  - `typedef ...` / `RET (*Name)(...)` keyword-less fn-ptr typedef -> .hh
  - `class`/`union` block  -> .hh if `public` or generic (`<type`), else .hc (private helper)
  - generic function       -> .hh (FULL BODY — a generic's body IS its template, which the
                              parser needs in scope at the call site)
  - non-generic public fn  -> prototype to .hh, body to .hc
  - bodyless public proto  -> .hh only
  - everything else (private fns, globals) -> .hc
`#include <dep.hc>` lines are DROPPED (the caller re-adds `<dep.hh>` where needed).
Run, then review + hand-fix cross-module wiring (includes, cross-module-internal protos)."""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

GENERIC = re.compile(r"<(type|comparable|int)\s")
PROTO = re.compile(r"^(public\s+([^(]*?)\(([^)]*)\))")

def is_generic_sig(line):
    head = line.split("(", 1)[0]
    return bool(GENERIC.search(head))

def block_end(lines, start):
    """Index (inclusive) where a brace-block starting at `start` closes; tracks depth.
    For a line with no `{` (e.g. bodyless proto or single-line global), returns start."""
    depth = 0
    seen = False
    j = start
    while j < len(lines):
        depth += lines[j].count("{") - lines[j].count("}")
        if "{" in lines[j]:
            seen = True
        if seen and depth <= 0:
            return j
        # A single-line construct (bodyless proto / global / typedef) ends at its `;`.
        # Strip a trailing `// comment` first, or `...); // note` wouldn't be recognised
        # and we'd over-consume the following lines (dropping them).
        code = lines[j].split("//", 1)[0].rstrip()
        if not seen and code.endswith(";"):
            return j
        j += 1
    return len(lines) - 1

def split(mod):
    src = (ROOT / "lib" / f"{mod}.hc").read_text()
    lines = src.splitlines()
    guard_hh, guard_hc = f"_{mod.upper()}_HH", f"_{mod.upper()}_HC"

    # leading doc comment (after the guard) -> reused in .hh
    i = 0
    while i < len(lines) and (lines[i].startswith("#ifndef") or lines[i].startswith("#define _")):
        i += 1
    doc = []
    while i < len(lines) and (lines[i].startswith("//") or lines[i].strip() == ""):
        doc.append(lines[i])
        i += 1

    hh = [f"#ifndef {guard_hh}", f"#define {guard_hh}"] + doc
    hc = [f"#ifndef {guard_hc}", f"#define {guard_hc}",
          f"// {mod}.hc — implementation (interface in {mod}.hh).", ""]

    pend = []                 # buffered comment/blank lines awaiting their construct
    def flush(dst):
        dst.extend(pend)
        pend.clear()

    j = i
    while j < len(lines):
        line = lines[j]
        s = line.strip()
        if s == "" or s.startswith("//"):
            pend.append(line)
            j += 1
            continue
        if s.startswith("#include "):     # drop original cross-module includes
            pend.clear()
            j += 1
            continue
        if s == "#endif":                  # original trailing guard; we re-emit ours
            pend.clear()
            j += 1
            continue
        if s.startswith("#define "):
            (flush(hh), hh.append(line)) if not re.match(r"#define\s+_", s) else (flush(hc), hc.append(line))
            j += 1
            continue
        # a construct: capture its full span (block or to `;`)
        end = block_end(lines, j)
        block = lines[j:end + 1]
        generic = is_generic_sig(s)
        is_class = bool(re.match(r"^(public\s+)?(class|union)\b", s))
        is_typedef = s.startswith("typedef") or re.match(r"^[A-Za-z_][\w ]*\(\*\w+\)\(", s)
        is_public_fn = s.startswith("public ") and "(" in s and not is_class
        if is_typedef:
            flush(hh); hh.extend(block)
        elif is_class:
            if s.startswith("public") or generic:
                flush(hh); hh.extend(block)
            else:
                flush(hc); hc.extend(block)
        elif generic:                      # generic fn: full template body -> .hh
            flush(hh); hh.extend(block)
        elif is_public_fn:
            m = PROTO.match(s)
            code = s.split("//", 1)[0].rstrip()
            if m:
                flush(hh); hh.append(m.group(1) + ";")
            if code.endswith(");"):        # bodyless primitive proto: header only
                pass
            else:
                # keep the body in the impl (its own comment already flushed to .hh)
                hc.extend(block)
        else:                              # private fn / global -> impl
            flush(hc); hc.extend(block)
        j = end + 1

    hh += ["", "#endif"]
    hc += ["", "#endif"]
    (ROOT / "lib" / f"{mod}.hh").write_text("\n".join(hh) + "\n")
    (ROOT / "lib" / f"{mod}.hc").write_text("\n".join(hc) + "\n")
    print(f"{mod}: hh={len(hh)}L hc={len(hc)}L")

for m in sys.argv[1:]:
    split(m)
