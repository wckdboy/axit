#!/usr/bin/env python3
"""Real PTY keyboard/mouse integration tests. Optional dependency: pyte==0.8.2."""
import codecs
import fcntl
import json
import os
from pathlib import Path
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

try:
    import pyte
except ImportError:
    raise SystemExit('Install the test dependency: python3 -m pip install pyte==0.8.2')

binary = str(Path(sys.argv[1]).resolve())
screens = Path(sys.argv[2]).resolve() if len(sys.argv)>2 else None
if screens:
    screens.mkdir(parents=True,exist_ok=True)

class Session:
    def __init__(self,root,state,recovery,base):
        self.master,self.slave=os.openpty()
        self.resize(140,38,notify=False)
        self.before=subprocess.run(['stty','-g'],stdin=self.slave,capture_output=True,text=True,check=True).stdout.strip()
        self.restore_file=base/'restored-terminal.txt'
        env=dict(os.environ,TERM='xterm-256color',COLORTERM='truecolor',AXIT_TEST_RESTORE=str(self.restore_file),AXIT_TRASH_DIR=str(recovery))
        env.pop('NO_COLOR',None)
        def terminal_session():
            os.setsid()
            fcntl.ioctl(self.slave,termios.TIOCSCTTY,0)
        wrapper='"$@"\naxit_test_status=$?\nstty -g > "$AXIT_TEST_RESTORE"\nexit "$axit_test_status"\n'
        args=[binary,'--root',str(root),'--state-dir',str(state)]
        self.process=subprocess.Popen(['/bin/sh','-c',wrapper,'axit-ui-test',*args],stdin=self.slave,stdout=self.slave,stderr=self.slave,preexec_fn=terminal_session,close_fds=True,env=env)
        self.screen=pyte.Screen(140,38)
        self.stream=pyte.Stream(self.screen)
        self.decoder=codecs.getincrementaldecoder('utf-8')(errors='replace')
        self.raw=bytearray()
        self.pump(.8)
    def resize(self,columns,rows,notify=True):
        fcntl.ioctl(self.slave,termios.TIOCSWINSZ,struct.pack('HHHH',rows,columns,0,0))
        if notify:
            self.screen.resize(lines=rows,columns=columns)
            self.process.send_signal(signal.SIGWINCH)
            self.pump(.2)
    def pump(self,seconds=.15):
        deadline=time.monotonic()+seconds
        while time.monotonic()<deadline:
            ready,_,_=select.select([self.master],[],[],min(.03,max(0,deadline-time.monotonic())))
            if ready:
                try: data=os.read(self.master,65536)
                except OSError: break
                if not data: break
                self.raw.extend(data)
                self.stream.feed(self.decoder.decode(data))
    def content(self): return '\n'.join(self.screen.display)
    def send(self,data,seconds=.15):
        os.write(self.master,data)
        self.pump(seconds)
    def wait(self,predicate,timeout=5):
        deadline=time.monotonic()+timeout
        while not predicate() and time.monotonic()<deadline:self.pump(.05)
        assert predicate(),self.content()
    def locate(self,text,last=False,left=False):
        def matches():
            found=[]
            for y,line in enumerate(self.screen.display):
                if left:line=line[:int(self.screen.columns*.65)]
                x=line.find(text)
                if x>=0:found.append((x+max(0,len(text)//2),y))
            return found
        # A PTY may split one frame across reads. Wait for the actual target,
        # including controls near the bottom, before synthesizing its click.
        self.wait(lambda:bool(matches()))
        found=matches()
        return found[-1] if last else found[0]
    @staticmethod
    def packet(code,point,up=False):
        return f'\x1b[<{code};{point[0]+1};{point[1]+1}{"m" if up else "M"}'.encode()
    def click(self,text,last=False,left=False,double=False):
        point=self.locate(text,last=last,left=left)
        pair=self.packet(0,point)+self.packet(0,point,True)
        self.send(pair*(2 if double else 1),.25)
    def wheel(self,point,down=True,count=1):self.send(self.packet(65 if down else 64,point)*count,.2)
    def snapshot(self,name):
        if not screens:return
        (screens/(name+'.txt')).write_text(self.content())
        cells=[]
        for y in range(self.screen.lines):
            for x in range(self.screen.columns):
                c=self.screen.buffer[y][x]
                if c.data.strip() or c.bg!='default':cells.append({'x':x,'y':y,'text':c.data,'fg':c.fg,'bg':c.bg,'bold':c.bold})
        (screens/(name+'.json')).write_text(json.dumps({'columns':self.screen.columns,'rows':self.screen.lines,'background':self.screen.buffer[0][0].bg,'cells':cells}))
    def finish(self,terminate=False):
        if terminate:
            # Signal the TUI child, not its shell wrapper, so stty can verify cleanup.
            children=subprocess.check_output(['pgrep','-P',str(self.process.pid)],text=True).split()
            assert children,'No TUI child found'
            for child in children:os.kill(int(child),signal.SIGTERM)
        else:self.send(b'\x11')
        self.wait(lambda:self.process.poll() is not None,timeout=8)
        self.process.wait(timeout=1)
        self.pump(.15)
        assert self.process.returncode==0,self.process.returncode
        assert self.before==self.restore_file.read_text().strip(),'Terminal raw/echo settings were not restored'
        assert b'\x1b[?1006h' in self.raw and b'\x1b[?1006l' in self.raw,'Mouse reporting was not enabled/restored'
        assert b'\x1b[?2004h' in self.raw and b'\x1b[?2004l' in self.raw,'Bracketed paste was not enabled/restored'
    def close(self):
        if self.process.poll() is None:
            self.send(b'\x11')
            try:self.process.wait(timeout=8)
            except subprocess.TimeoutExpired:self.process.kill();self.process.wait()
        os.close(self.master);os.close(self.slave)

with tempfile.TemporaryDirectory(prefix='axit-ui-',dir='/tmp') as temporary:
    base=Path(temporary).resolve();root=base/'files';state=base/'state';recovery=base/'recovery'
    for name in ['documents','downloads','projects']:(root/name).mkdir(parents=True)
    (root/'documents/budget.txt').write_text('fixture document\n'*1024)
    (root/'downloads/installer.dmg').write_bytes(b'0'*65536)
    (root/'projects/source.rs').write_text('fn main() {}\n')
    for n in range(50):(root/f'file-{n:03}.txt').write_text('fixture\n')
    subprocess.run([binary,'--root',str(root),'--state-dir',str(state),'scan'],capture_output=True,check=True)
    app=Session(root,state,recovery,base)
    try:
        assert 'AXIT' in app.content() and 'WCKD.ai' in app.content(),app.content()
        assert len({c.fg for row in app.screen.buffer.values() for c in row.values()})>=5,'Theme colors missing'
        app.snapshot('space')
        app.send(b'\x1b[A\x1b[C\x1b[B')  # First row -> navigation -> Find -> query.
        app.wait(lambda:'Search · typing' in app.content())
        app.send(b'file-003')
        app.wait(lambda:'1 match' in app.content())
        app.send(b'\x15')  # Clear the fixture query before later editing tests.
        app.send(b'\x1b[1;5A\x1b[D\x1b[B')  # Ctrl-Up -> navigation -> Space -> list.
        app.wait(lambda:'Space map' in app.content())
        app.send(b'1234')
        app.wait(lambda:'Operations' in app.content())
        app.send(b'\x1b[6;5~')  # Ctrl-PageDown wraps to Space without Escape.
        app.wait(lambda:'Space map' in app.content())
        app.click('downloads',left=True,double=True)
        app.wait(lambda:'installer.dmg' in app.content())
        app.click('Scope',left=True)
        app.wait(lambda:'file-000.txt' in app.content())
        header=app.locate('Name',left=True)[1]
        before='\n'.join(line[:90] for line in app.screen.display[header+1:-3])
        app.wheel((20,header+2),count=7)
        assert '\n'.join(line[:90] for line in app.screen.display[header+1:-3])!=before,'Touchpad wheel did not scroll the table'
        app.send(b'\x1b[F')
        assert 'file-049.txt' in app.content(),'End did not reveal the last row'
        app.send(b'\x06')  # Ctrl-F, even after scrolling.
        app.send(b'\x1b[200~budgeXt\x1b[201~')
        app.send(b'\x1b[D\x7f\x1b[C')  # Fix an interior typo: budgeXt -> budget.
        app.wait(lambda:'budget.txt' in app.content() and '1 match' in app.content())
        app.snapshot('find')
        app.send(b'\r')  # Query -> results.
        app.send(b'\x1b[3~')  # Delete opens a plan, never deletes immediately.
        app.wait(lambda:'Review plan' in app.content())
        assert (root/'documents/budget.txt').exists(),'Delete key skipped review'
        app.snapshot('preview')
        app.send(b'\x1b')
        app.send(b'\x1b[3~')
        app.wait(lambda:'Review plan' in app.content())
        app.send(b'\t\r')  # Cancel starts focused; Tab selects the explicit action.
        app.wait(lambda:not (root/'documents/budget.txt').exists())
        assert len(list((recovery/'files').iterdir()))==1,'Trash escaped the private fixture'
        app.wait(lambda:'No matching files' in app.content())
        assert 'Scanning:' not in app.content(),'A small deletion forced a full index rebuild'
        app.send(b'u')
        app.wait(lambda:(root/'documents/budget.txt').exists() and 'budget.txt' in app.content())
        app.send(b'1')
        app.wait(lambda:'file-000.txt' in app.content())
        app.click('file-000.txt',left=True)
        trash_button=app.locate('Trash',last=True)
        app.send(app.packet(0,trash_button))
        assert 'Review plan' not in app.content(),'Button activated before release'
        app.send(app.packet(0,trash_button,True))
        app.wait(lambda:'Review plan' in app.content())
        app.click('Cancel')
        assert (root/'file-000.txt').exists(),'Cancel deleted the file'
        app.click('Trash',last=True)
        app.wait(lambda:'Review plan' in app.content())
        confirm=app.locate('Move to Trash',last=True)
        app.send(app.packet(0,confirm))
        assert (root/'file-000.txt').exists(),'Confirmation activated before release'
        app.send(app.packet(0,confirm,True))
        app.wait(lambda:not (root/'file-000.txt').exists())
        assert (root/'documents/budget.txt').exists() and (root/'downloads/installer.dmg').exists(),'Unselected fixture changed'
        app.click('?',last=True)
        app.wait(lambda:'AXIT / keyboard & mouse' in app.content())
        app.wheel((30,15),count=20)
        app.send(b'\x1b[F')
        assert 'Hold Shift' in app.content(),'Dialog could not scroll to its final lines'
        app.resize(80,24)
        assert 'Close' in app.content(),app.content()
        app.snapshot('compact')
        app.resize(48,16)
        assert 'Close' in app.content(),app.content()
        app.click('Close')
        app.resize(140,38)
        app.snapshot('after')
        app.finish()
    finally:app.close()
    app=Session(root,state,recovery,base)
    try:app.finish(terminate=True)
    finally:app.close()
print('PTY checks passed: keyboard editing/paging, SGR mouse clicks/double-clicks, wheel/touchpad input, reviewed Trash, Undo, modal scrolling, resize, Ctrl-Q/SIGTERM and terminal-mode restoration')
