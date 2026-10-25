#!/usr/bin/env python3
"""Render AXIT's brand sheet from the runtime theme; no duplicate color definitions."""
import html
import re
from pathlib import Path

source=Path('src/tui/theme.rs').read_text()
rgb={name:tuple(map(int,(r,g,b))) for name,r,g,b in re.findall(r'const\s+(\w+):\s*Color\s*=\s*Color::Rgb\(\s*(\d+),\s*(\d+),\s*(\d+)\s*\)',source)}
colors={key:'#'+''.join(f'{c:02X}' for c in value) for key,value in rgb.items()}
parts=['<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="800" viewBox="0 0 1200 800" role="img" aria-label="AXIT Copper color palette"><title>AXIT / Copper color palette</title>']
def rect(x,y,w,h,color):
    stroke=f' stroke="{colors["BORDER"]}"' if color=='BG' and x>0 else ''
    parts.append(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="0" fill="{colors.get(color,color)}"{stroke}/>')
def text(x,y,value,color='FG',size=18,weight=400):parts.append(f'<text x="{x}" y="{y}" font-family="ui-monospace,Menlo,Consolas,monospace" font-size="{size}" font-weight="{weight}" fill="{colors.get(color,color)}">{html.escape(value)}</text>')
rect(0,0,1200,800,'BG')
text(40,56,'AXIT / WCKD.ai','ACCENT',26,700)
text(40,90,'COPPER — warm focus on charcoal.','DIM',17)
rect(40,126,530,216,'ACCENT')
text(65,168,'COPPER','BG',20,700)
text(65,220,colors['ACCENT'],'BG',38,700)
text(65,304,'Identity · focus · navigation','BG',17)
rect(594,126,566,216,'PANEL')
text(618,163,'AXIT','ACCENT',20,700)
text(1015,163,'42 GiB free','INFO',16)
text(618,205,'[✓]  invoices.pdf','FG',17)
text(1038,205,'24 MiB','FG',17)
rect(610,220,532,34,'SELECTED')
text(618,243,'[ ]  projects/','FG',17)
text(1038,243,'18 GiB','FG',17)
text(618,298,'Find   Mark   Undo','ACCENT',17)
text(905,298,'Trash   Clean','DANGER',17)
text(40,391,'FOUNDATION','DIM',16,700)
for i,(key,name) in enumerate([('BG','Charcoal'),('PANEL','Graphite'),('FG','Chalk'),('DIM','Silver')]):
 x=40+i*286
 rect(x,413,262,82,key)
 text(x,526,name,'FG',18,700);text(x,553,colors[key],'DIM',16)
text(40,602,'SEMANTICS','DIM',16,700)
for i,(key,name) in enumerate([('INFO','Sky / information'),('MINT','Sage / success'),('AMBER','Sand / warning'),('DANGER','Rose / destructive')]):
 x=40+i*286
 rect(x,624,262,56,key)
 text(x,711,name,'FG',16,700);text(x,738,colors[key],'DIM',16)
text(40,778,'WCKD.ai · AXIT / Copper · RGB palette','DIM',13)
parts.append('</svg>')
Path('assets/palette.svg').write_text('\n'.join(parts)+'\n')
