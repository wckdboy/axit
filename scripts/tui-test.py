#!/usr/bin/env python3
"""Real PTY smoke test. Optional development dependency: pip install pyte==0.8.2."""
import fcntl
import codecs
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
    raise SystemExit('Install the optional test dependency: python3 -m pip install pyte==0.8.2')

binary = str(Path(sys.argv[1]).resolve())
screens = Path(sys.argv[2]).resolve() if len(sys.argv) > 2 else None
if screens:
    screens.mkdir(parents=True, exist_ok=True)

with tempfile.TemporaryDirectory(prefix='scry-ui-', dir='/tmp') as temporary:
    base = Path(temporary).resolve()
    root, state = base/'files', base/'state'
    root.mkdir()
    (root/'documents').mkdir()
    (root/'downloads').mkdir()
    (root/'projects').mkdir()
    (root/'documents/budget.txt').write_text('fixture document\n'*1024)
    (root/'downloads/installer.dmg').write_bytes(b'0'*65536)
    (root/'projects/source.rs').write_text('fn main() {}\n')
    args = [binary, '--root', str(root), '--state-dir', str(state)]
    subprocess.run([*args, 'scan'], capture_output=True, text=True, check=True)
    master, slave = os.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 38, 140, 0, 0))
    before = subprocess.run(['stty','-g'],stdin=slave,capture_output=True,text=True,check=True).stdout.strip()
    def terminal_session():
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
    restore_file=base/'restored-terminal.txt'
    env = dict(os.environ, TERM='xterm-256color', COLORTERM='truecolor',SCRY_TEST_RESTORE=str(restore_file))
    env.pop('NO_COLOR', None)  # Exercise the theme even from a monochrome CI shell.
    wrapper='"$@"\nscry_test_status=$?\nstty -g > "$SCRY_TEST_RESTORE"\nexit "$scry_test_status"\n'
    process = subprocess.Popen(['/bin/sh','-c',wrapper,'scry-ui-test',*args], stdin=slave, stdout=slave, stderr=slave,
                               preexec_fn=terminal_session, close_fds=True, env=env)
    screen = pyte.Screen(140, 38)
    stream = pyte.Stream(screen)
    decoder=codecs.getincrementaldecoder('utf-8')(errors='replace')
    def pump(seconds=.5):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            ready, _, _ = select.select([master], [], [], min(.05, max(0, deadline-time.monotonic())))
            if ready:
                data = os.read(master, 65536)
                if not data:
                    break
                stream.feed(decoder.decode(data))
    def content():
        return '\n'.join(screen.display)
    def key(value, seconds=.4):
        os.write(master, value)
        pump(seconds)
    def snapshot(name):
        if screens:
            (screens/(name+'.txt')).write_text(content().replace(str(base), '/tmp/scry-demo'))
            # Actual terminal cell colors for optional visual review.
            cells=[]
            for y in range(screen.lines):
                for x in range(screen.columns):
                    cell=screen.buffer[y][x]
                    if cell.data.strip() or cell.bg!='default':
                        cells.append({'x':x,'y':y,'text':cell.data,'fg':cell.fg,'bg':cell.bg,'bold':cell.bold})
            (screens/(name+'.json')).write_text(json.dumps({'columns':screen.columns,'rows':screen.lines,'cells':cells}))
    try:
        pump(1)
        assert 'SCRY' in content() and 'WCKD.ai' in content(), content()
        assert 'downloads' in content(), content()
        assert len({cell.fg for row in screen.buffer.values() for cell in row.values()}) >= 5, 'Theme colors were not emitted'
        snapshot('space')
        key(b'/budget')
        assert 'budget.txt' in content() and '1 matches' in content(), content()
        snapshot('find')
        key(b'\r')
        key(b't', 1)
        assert 'Review plan' in content() and 'TRASH' in content(), content()
        assert (root/'documents/budget.txt').exists(), 'Preview changed a file'
        snapshot('preview')
        key(b'\x1b')
        key(b'?')
        assert 'SCRY / keyboard' in content(), content()
        key(b'\x1b')
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 80, 0, 0))
        screen.resize(lines=24,columns=80)
        process.send_signal(signal.SIGWINCH)
        pump(.5)
        assert 'SCRY' in content(), content()
        snapshot('compact')
        key(b'\x11')
        process.wait(timeout=5)
        assert process.returncode==0, process.returncode
        after=restore_file.read_text().strip()
        assert before==after, 'Terminal echo/raw mode was not restored'
        assert (root/'documents/budget.txt').exists(), 'UI preview deleted a file'
    finally:
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=5)
        os.close(master)
        os.close(slave)
print('Native PTY checks passed: navigation, live search, non-mutating preview, help, resize, quit, terminal restore')

