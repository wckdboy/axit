#!/usr/bin/env python3
"""Render the shared terminal pixel axe as crisp SVG assets."""
from pathlib import Path
import sys

root = Path(__file__).resolve().parent.parent
palette = {"E": "#e9f1de", "S": "#70c7e8", "C": "#99ada5", "B": "#d6f45b", "H": "#d6f45b"}
rows = (root / "assets/axe.txt").read_text().splitlines()
assert len(rows) == 6 and all(len(row) == 6 for row in rows)
rects = ['<rect width="16" height="16" fill="#0e1718"/>']
for y, row in enumerate(rows):
    for x, cell in enumerate(row):
        if cell != ".":
            rects.append(f'<rect x="{2+x*2}" y="{2+y*2}" width="2" height="2" fill="{palette[cell]}"/>')
svg = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" shape-rendering="crispEdges" role="img"><title>Axit pixel axe</title>' + ''.join(rects) + '</svg>\n'
paths = [root / "assets/axe.svg", root / "site/public/favicon.svg"]
if sys.argv[1:] == ["--check"]:
    for path in paths:
        if path.read_text() != svg:
            raise SystemExit(f"Stale pixel axe: {path.relative_to(root)}; run python3 scripts/render-logo.py")
    print("Shared terminal and website pixel axe checked.")
elif sys.argv[1:]:
    raise SystemExit("Usage: render-logo.py [--check]")
else:
    for path in paths:
        path.write_text(svg)
    print('Rendered the shared Axit pixel axe.')
