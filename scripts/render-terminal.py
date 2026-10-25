#!/usr/bin/env python3
"""Render actual pyte terminal cells from tui-test.py as a small SVG preview."""
import html
import json
from pathlib import Path
import sys

screen=json.loads(Path(sys.argv[1]).read_text())
columns,rows=screen['columns'],screen['rows']
cell_width,cell_height,padding=10,22,24
width,height=columns*cell_width+padding*2,rows*cell_height+padding*2
cells={(c['x'],c['y']):c for c in screen['cells']}
background='#'+screen.get('background','11141d')
parts=[f'<svg xmlns="http://www.w3.org/2000/svg" width="1120" height="{round(height*1120/width)}" viewBox="0 0 {width} {height}" role="img" aria-label="AXIT terminal storage browser"><title>AXIT terminal capture — temporary test fixtures</title><rect width="100%" height="100%" rx="16" fill="{background}"/>']
for y in range(rows):
    x=0
    while x<columns:
        color=cells.get((x,y),{}).get('bg','default')
        end=x+1
        while end<columns and cells.get((end,y),{}).get('bg','default')==color:end+=1
        if color!='default':parts.append(f'<rect x="{padding+x*cell_width}" y="{padding+y*cell_height}" width="{(end-x)*cell_width}" height="{cell_height}" fill="#{color}"/>')
        x=end
for y in range(rows):
    x=0
    while x<columns:
        c=cells.get((x,y),{})
        if c.get('text')=='▀':
            color=c.get('fg','default')
            if color!='default':parts.append(f'<rect x="{padding+x*cell_width}" y="{padding+y*cell_height}" width="{cell_width}" height="{cell_height/2}" fill="#{color}"/>')
            x+=1
            continue
        color,bold=c.get('fg','default'),c.get('bold',False)
        end=x+1
        while end<columns and cells.get((end,y),{}).get('text')!='▀' and (cells.get((end,y),{}).get('fg','default'),cells.get((end,y),{}).get('bold',False))==(color,bold):end+=1
        text=''.join(cells.get((i,y),{}).get('text',' ') for i in range(x,end)).rstrip()
        if text.strip():
            fill='#e0e4f0' if color=='default' else '#'+color
            parts.append(f'<text xml:space="preserve" x="{padding+x*cell_width}" y="{padding+y*cell_height+16}" font-family="ui-monospace,Menlo,Consolas,monospace" font-size="16.6" font-weight="{700 if bold else 400}" fill="{fill}">{html.escape(text)}</text>')
        x=end
parts.append('</svg>')
Path(sys.argv[2]).write_text('\n'.join(parts)+'\n')
