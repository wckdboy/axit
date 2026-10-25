#!/usr/bin/env python3
"""Render the shared terminal pixel axe as crisp SVG assets."""
from pathlib import Path
import sys
import re

root = Path(__file__).resolve().parent.parent
theme = (root / "src/tui/theme.rs").read_text()
rgb = re.search(r'const ACCENT: Color = Color::Rgb\((\d+), (\d+), (\d+)\)', theme).groups()
accent = '#' + ''.join(f'{int(channel):02x}' for channel in rgb)
rows = (root / "assets/axe.txt").read_text().splitlines()
assert len(rows) == 8 and all(len(row) == 8 and set(row) <= {".", "A"} for row in rows)
rects = []
for y, row in enumerate(rows):
    for x, cell in enumerate(row):
        if cell != ".":
            rects.append(f'<rect x="{2+x*2}" y="{2+y*2}" width="2" height="2" fill="{accent}"/>')
svg = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" shape-rendering="crispEdges" role="img"><title>Axit pixel axe</title>' + ''.join(rects) + '</svg>\n'
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
