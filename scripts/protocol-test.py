#!/usr/bin/env python3
"""Exercise the real stdio processes. Mutations are confined to temporary fixtures."""
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading
import time

BINARY = str(Path(sys.argv[1]).resolve())
checks = 0


def check(condition, message):
    global checks
    assert condition, message
    checks += 1


class Client:
    def __init__(self, args, env=None):
        self.process = subprocess.Popen([BINARY, *args], stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                        text=True, bufsize=1, env=env)
        self.messages = queue.Queue()
        self.notifications = []
        self.sequence = 0
        def reader():
            for line in self.process.stdout:
                try:
                    self.messages.put(json.loads(line))
                except Exception as error:
                    self.messages.put(error)
        self.reader = threading.Thread(target=reader, daemon=True)
        self.reader.start()

    def send(self, value):
        self.process.stdin.write(json.dumps(value) + '\n')
        self.process.stdin.flush()

    def request(self, method, params=None):
        self.sequence += 1
        current = self.sequence
        self.send({'jsonrpc': '2.0', 'id': current, 'method': method,
                   'params': {} if params is None else params})
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            value = self.messages.get(timeout=max(.01, deadline-time.monotonic()))
            if isinstance(value, Exception):
                raise value
            check(value.get('jsonrpc') == '2.0', 'Invalid JSON-RPC envelope')
            if value.get('id') == current:
                return value
            if 'method' in value:
                self.notifications.append(value)
            else:
                raise AssertionError(('Unexpected response', value))
        raise AssertionError('Protocol request timed out')

    def tool(self, name, arguments=None):
        return self.request('tools/call', {'name': name, 'arguments': arguments or {}})['result']

    def close(self):
        self.process.stdin.close()
        self.process.wait(timeout=10)
        stderr = self.process.stderr.read()
        check(self.process.returncode == 0, f'Server exited abnormally: {stderr}')


with tempfile.TemporaryDirectory(prefix='scry-protocol-') as temporary:
    base = Path(temporary).resolve()
    root, state = base/'files', base/'state'
    root.mkdir()
    (root/'project').mkdir()
    (root/'private').mkdir()
    (root/'organized').mkdir()
    (root/'project/public.txt').write_text('public')
    (root/'private/secret.txt').write_text('private')
    (root/'move-me.txt').write_text('recover me')
    scan = subprocess.run([BINARY, '--root', str(root), '--state-dir', str(state), 'scan'],
                          capture_output=True, text=True, check=True)
    check(json.loads(scan.stdout)['entries'] >= 7, 'Fixture index incomplete')
    args = ['--root', str(root), '--state-dir', str(state)]
    mcp = Client([*args, 'mcp'])
    init = mcp.request('initialize', {'protocolVersion': '2025-11-25', 'capabilities': {},
                                    'clientInfo': {'name': 'scry-test', 'version': '1'}})
    check(init['result']['protocolVersion'] == '2025-11-25', 'Wrong MCP version')
    mcp.send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})
    tools = mcp.request('tools/list')['result']['tools']
    check(len(tools) == 7, 'Unexpected read-only tool surface')
    check(all(t['annotations']['readOnlyHint'] for t in tools), 'Read-only tools mislabelled')
    found = mcp.tool('scry_search', {'query': 'ext:txt', 'limit': 5})
    check(not found['isError'], found)
    check(found['structuredContent']['matched'] == 3, 'Wrong search results')
    check(all(e['path'].startswith(str(root)) for e in found['structuredContent']['entries']), 'Scope escaped')
    denied = mcp.tool('scry_apply_plan', {'plan_id': '0'*20, 'confirm': 'CLEAN'})
    check(denied['isError'], 'Writes unexpectedly allowed')
    check(mcp.tool('scry_search', {'query': '', 'limit': 501})['isError'], 'Unbounded result limit accepted')
    check(mcp.tool('scry_search', {'query': '', 'limit': 0})['isError'], 'Zero result limit accepted')
    check(mcp.tool('scry_search', {'query': '', 'sort': 42})['isError'], 'Invalid sort type accepted')
    check(mcp.tool('scry_search', {'query': '', 'unexpected': True})['isError'], 'Unknown argument accepted')
    check(mcp.tool('scry_browse', {'entry_id': 2**40})['isError'], 'Invalid entry accepted')
    check(mcp.request('no_such_method')['error']['code'] == -32601, 'Unknown method error incorrect')
    check('result' in mcp.request('ping'), 'Ping failed')
    mcp.close()

    writable = Client([*args, 'mcp', '--allow-mutations'])
    writable.request('initialize', {'protocolVersion': '2025-06-18'})
    tools = writable.request('tools/list')['result']['tools']
    check(any(t['name'] == 'scry_apply_plan' and t['annotations']['destructiveHint'] for t in tools), 'Mutation annotations missing')
    entry = writable.tool('scry_search', {'query': 'move-me.txt'})['structuredContent']['entries'][0]
    planned = writable.tool('scry_move_plan', {'entry_ids': [entry['id']], 'destination': str(root/'organized')})
    check(not planned['isError'], planned)
    plan = planned['structuredContent']
    check(len(plan['entries']) == 1 and (root/'move-me.txt').exists(), 'Plan changed files or had no eligible entry')
    bad = writable.tool('scry_apply_plan', {'plan_id': plan['id'], 'confirm': 'yes'})
    check(bad['isError'] and (root/'move-me.txt').exists(), 'Weak confirmation accepted')
    applied = writable.tool('scry_apply_plan', {'plan_id': plan['id'], 'confirm': 'MOVE'})
    check(not applied['isError'] and len(applied['structuredContent']['completed']) == 1, applied)
    check((root/'organized/move-me.txt').read_text() == 'recover me', 'Move lost data')
    restored = writable.tool('scry_undo')
    check(not restored['isError'] and (root/'move-me.txt').read_text() == 'recover me', 'Undo failed')
    writable.close()

    # Trash tests use a private recovery fixture, never the real user's Trash.
    fake_trash=base/'trash-fixture'
    fake_env=dict(os.environ, SCRY_TRASH_DIR=str(fake_trash))
    trash=Client([*args,'mcp','--allow-mutations'],env=fake_env)
    trash.request('initialize',{'protocolVersion':'2025-11-25'})
    entry=trash.tool('scry_search',{'query':'move-me.txt'})['structuredContent']['entries'][0]
    plan=trash.tool('scry_trash_plan',{'entry_ids':[entry['id']]})['structuredContent']
    result=trash.tool('scry_apply_plan',{'plan_id':plan['id'],'confirm':'TRASH'})
    check(not result['isError'] and len(result['structuredContent']['completed'])==1,result)
    check(fake_trash.stat().st_mode & 0o077 == 0, 'Recovery directory is not private')
    check(len(list((fake_trash/'files').iterdir())) == 1, 'Custom recovery files escaped their root')
    check(not (root/'move-me.txt').exists(),'Trash did not move the fixture')
    if sys.platform.startswith('linux'):
        info=list((fake_trash/'info').glob('*.trashinfo'))
        check(len(info)==1 and '[Trash Info]' in info[0].read_text(),'Freedesktop Trash metadata missing')
    check(not (fake_trash.parent/'info').exists(), 'Trash metadata escaped the custom recovery root')
    result=trash.tool('scry_undo')
    check(not result['isError'] and (root/'move-me.txt').read_text()=='recover me','Scoped Trash undo failed')
    fake_trash.chmod(0o755)
    plan=trash.tool('scry_trash_plan',{'entry_ids':[entry['id']]})['structuredContent']
    result=trash.tool('scry_apply_plan',{'plan_id':plan['id'],'confirm':'TRASH'})
    check(not result['structuredContent']['completed'] and result['structuredContent']['blocked'], 'Public recovery folder accepted')
    check((root/'move-me.txt').read_text()=='recover me', 'Blocked Trash changed the source')
    fake_trash.chmod(0o700)
    trash.close()

    acp = Client([*args, 'acp'])
    init = acp.request('initialize', {'protocolVersion': 1, 'clientCapabilities': {}})
    check(init['result']['protocolVersion'] == 1, 'Wrong ACP version')
    check(init['result']['agentCapabilities']['loadSession'] is False, 'Unsupported history advertised')
    session = acp.request('session/new', {'cwd': str(root/'project'), 'mcpServers': []})['result']['sessionId']
    check(session.startswith('scry-'), 'Missing ACP session')
    result = acp.request('session/prompt', {'sessionId': session, 'prompt': [{'type': 'text', 'text': '/search ext:txt'}]})
    check(result['result']['stopReason'] == 'end_turn', 'ACP prompt did not end')
    chunks = [n for n in acp.notifications if n.get('params', {}).get('update', {}).get('sessionUpdate') == 'agent_message_chunk']
    check(bool(chunks), 'ACP did not emit an agent chunk')
    value = json.loads(chunks[-1]['params']['update']['content']['text'])
    check(value['matched'] == 1, 'ACP session scope includes sibling data')
    check(value['entries'][0]['name'] == 'public.txt', 'ACP returned private sibling')
    check(acp.request('session/new', {'cwd': str(base), 'mcpServers': []})['error']['code'] == -32602, 'ACP allowed widening server scope')
    check('error' in acp.request('session/prompt', {'sessionId': 'unknown', 'prompt': []}), 'Unknown ACP session accepted')
    acp.close()

    # Malformed/oversized messages must not corrupt the next request.
    invalid = Client([*args, 'mcp'])
    invalid.process.stdin.write('not json\n')
    invalid.process.stdin.flush()
    check(invalid.messages.get(timeout=5)['error']['code'] == -32700, 'Parse error missing')
    invalid.process.stdin.write('x'*(1024*1024+100) + '\n')
    invalid.process.stdin.flush()
    check(invalid.messages.get(timeout=5)['error']['code'] == -32600, 'Oversized message accepted')
    invalid.request('initialize', {'protocolVersion': '2024-11-05'})
    check('result' in invalid.request('ping'), 'Oversized input corrupted framing')
    invalid.close()

print(f'Protocol checks passed: {checks} · MCP read-only/write gates + ACP scoped sessions + framing')

