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


with tempfile.TemporaryDirectory(prefix='axit-protocol-') as temporary:
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
                                    'clientInfo': {'name': 'axit-test', 'version': '1'}})
    check(init['result']['protocolVersion'] == '2025-11-25', 'Wrong MCP version')
    mcp.send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})
    tools = mcp.request('tools/list')['result']['tools']
    check(len(tools) == 10, 'Unexpected read-only tool surface')
    check(all(t['annotations']['readOnlyHint'] for t in tools), 'Read-only tools mislabelled')
    check(all(t['name'].startswith('axit_') for t in tools), 'Old brand still advertised')
    check(not mcp.tool('scry_status')['isError'], 'Legacy MCP status alias failed')
    check(mcp.tool('scry_apply_plan', {'plan_id':'0'*20,'confirm':'CLEAN'})['isError'], 'Legacy alias bypassed mutation gate')
    import plistlib
    app_path=root/'project/Scoped.app';(app_path/'Contents').mkdir(parents=True)
    (app_path/'Contents/Info.plist').write_bytes(plistlib.dumps({'CFBundleIdentifier':'org.wckd.Scoped','CFBundleName':'Scoped'}))
    outside=base/'Outside.app';(outside/'Contents').mkdir(parents=True)
    (outside/'Contents/Info.plist').write_bytes(plistlib.dumps({'CFBundleIdentifier':'org.wckd.Private','CFBundleName':'Private'}))
    (root/'Escape.app').symlink_to(outside,target_is_directory=True)
    app_list=mcp.tool('axit_apps',{'query':'Scoped'})
    check(not app_list['isError'] and app_list['structuredContent']['matched']==1,app_list)
    app_key=app_list['structuredContent']['apps'][0]['key']
    files=mcp.tool('axit_app_files',{'app_key':app_key})
    check(not files['isError'] and all(f['path'].startswith(str(root)) for f in files['structuredContent']['files']),files)
    check(mcp.tool('axit_apps',{'query':'Private'})['structuredContent']['matched']==0,'App inventory followed an escape symlink')
    check(mcp.tool('axit_apps',{'limit':0})['isError'],'Invalid app limit accepted')
    check(mcp.tool('axit_app_data_plan',{'app_key':app_key,'paths':[str(outside)]})['isError'],'App plan escaped scope')
    check(mcp.tool('axit_app_data_plan',{'app_key':app_key,'paths':[str(app_path)]})['isError'],'Noninteractive app removal accepted')
    found = mcp.tool('axit_search', {'query': 'ext:txt', 'limit': 5})
    check(not found['isError'], found)
    check(found['structuredContent']['matched'] == 3, 'Wrong search results')
    check(all(e['path'].startswith(str(root)) for e in found['structuredContent']['entries']), 'Scope escaped')
    denied = mcp.tool('axit_apply_plan', {'plan_id': '0'*20, 'confirm': 'CLEAN'})
    check(denied['isError'], 'Writes unexpectedly allowed')
    check(mcp.tool('axit_search', {'query': '', 'limit': 501})['isError'], 'Unbounded result limit accepted')
    check(mcp.tool('axit_search', {'query': '', 'limit': 0})['isError'], 'Zero result limit accepted')
    check(mcp.tool('axit_search', {'query': '', 'sort': 42})['isError'], 'Invalid sort type accepted')
    check(mcp.tool('axit_search', {'query': '', 'unexpected': True})['isError'], 'Unknown argument accepted')
    check(mcp.tool('axit_browse', {'entry_id': 2**40})['isError'], 'Invalid entry accepted')
    check(mcp.request('no_such_method')['error']['code'] == -32601, 'Unknown method error incorrect')
    check('result' in mcp.request('ping'), 'Ping failed')
    mcp.close()

    writable = Client([*args, 'mcp', '--allow-mutations'])
    writable.request('initialize', {'protocolVersion': '2025-06-18'})
    tools = writable.request('tools/list')['result']['tools']
    check(any(t['name'] == 'axit_apply_plan' and t['annotations']['destructiveHint'] for t in tools), 'Mutation annotations missing')
    entry = writable.tool('axit_search', {'query': 'move-me.txt'})['structuredContent']['entries'][0]
    planned = writable.tool('axit_move_plan', {'entry_ids': [entry['id']], 'destination': str(root/'organized')})
    check(not planned['isError'], planned)
    plan = planned['structuredContent']
    check(len(plan['entries']) == 1 and (root/'move-me.txt').exists(), 'Plan changed files or had no eligible entry')
    bad = writable.tool('axit_apply_plan', {'plan_id': plan['id'], 'confirm': 'yes'})
    check(bad['isError'] and (root/'move-me.txt').exists(), 'Weak confirmation accepted')
    applied = writable.tool('axit_apply_plan', {'plan_id': plan['id'], 'confirm': 'MOVE'})
    check(not applied['isError'] and len(applied['structuredContent']['completed']) == 1, applied)
    check(applied['structuredContent'].get('needs_reconcile') is False, 'Normal move reported an uncertain outcome')
    check((root/'organized/move-me.txt').read_text() == 'recover me', 'Move lost data')
    restored = writable.tool('axit_undo')
    check(not restored['isError'] and (root/'move-me.txt').read_text() == 'recover me', 'Undo failed')
    writable.close()

    # Trash tests use a private recovery fixture, never the real user's Trash.
    fake_trash=base/'trash-fixture'
    fake_env=dict(os.environ, AXIT_TRASH_DIR=str(fake_trash))
    trash=Client([*args,'mcp','--allow-mutations'],env=fake_env)
    trash.request('initialize',{'protocolVersion':'2025-11-25'})
    entry=trash.tool('axit_search',{'query':'move-me.txt'})['structuredContent']['entries'][0]
    plan=trash.tool('axit_trash_plan',{'entry_ids':[entry['id']]})['structuredContent']
    result=trash.tool('axit_apply_plan',{'plan_id':plan['id'],'confirm':'TRASH'})
    check(not result['isError'] and len(result['structuredContent']['completed'])==1,result)
    check(fake_trash.stat().st_mode & 0o077 == 0, 'Recovery directory is not private')
    check(len(list((fake_trash/'files').iterdir())) == 1, 'Custom recovery files escaped their root')
    check(not (root/'move-me.txt').exists(),'Trash did not move the fixture')
    if sys.platform.startswith('linux'):
        info=list((fake_trash/'info').glob('*.trashinfo'))
        check(len(info)==1 and '[Trash Info]' in info[0].read_text(),'Freedesktop Trash metadata missing')
    check(not (fake_trash.parent/'info').exists(), 'Trash metadata escaped the custom recovery root')
    result=trash.tool('axit_undo')
    check(not result['isError'] and (root/'move-me.txt').read_text()=='recover me','Scoped Trash undo failed')
    fake_trash.chmod(0o755)
    plan=trash.tool('axit_trash_plan',{'entry_ids':[entry['id']]})['structuredContent']
    result=trash.tool('axit_apply_plan',{'plan_id':plan['id'],'confirm':'TRASH'})
    check(not result['structuredContent']['completed'] and result['structuredContent']['blocked'], 'Public recovery folder accepted')
    check(result['structuredContent'].get('needs_reconcile') is False, 'Unchanged rejection reported a post-mutation failure')
    check((root/'move-me.txt').read_text()=='recover me', 'Blocked Trash changed the source')
    fake_trash.chmod(0o700)
    trash.close()

    acp = Client([*args, 'acp'])
    init = acp.request('initialize', {'protocolVersion': 1, 'clientCapabilities': {}})
    check(init['result']['protocolVersion'] == 1, 'Wrong ACP version')
    check(init['result']['agentCapabilities']['loadSession'] is False, 'Unsupported history advertised')
    session = acp.request('session/new', {'cwd': str(root/'project'), 'mcpServers': []})['result']['sessionId']
    check(session.startswith('axit-'), 'Missing ACP session')
    result = acp.request('session/prompt', {'sessionId': session, 'prompt': [{'type': 'text', 'text': '/search ext:txt'}]})
    check(result['result']['stopReason'] == 'end_turn', 'ACP prompt did not end')
    chunks = [n for n in acp.notifications if n.get('params', {}).get('update', {}).get('sessionUpdate') == 'agent_message_chunk']
    check(bool(chunks), 'ACP did not emit an agent chunk')
    value = json.loads(chunks[-1]['params']['update']['content']['text'])
    check(value['matched'] == 1, 'ACP session scope includes sibling data')
    check(value['entries'][0]['name'] == 'public.txt', 'ACP returned private sibling')
    acp.request('session/prompt',{'sessionId':session,'prompt':[{'type':'text','text':'/apps Scoped'}]})
    chunks=[n for n in acp.notifications if n.get('params',{}).get('update',{}).get('sessionUpdate')=='agent_message_chunk']
    value=json.loads(chunks[-1]['params']['update']['content']['text'])
    check(value['matched']==1 and value['apps'][0]['application']['id']=='org.wckd.Scoped','ACP apps ignored the session scope')
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

# Multiple windows and client isolation use an independent large temporary index.
with tempfile.TemporaryDirectory(prefix='axit-pages-') as temporary:
    base = Path(temporary).resolve()
    root, state = base/'files', base/'state'
    (root/'private').mkdir(parents=True)
    for n in range(10057):
        (root/f'file-{n:05}.txt').touch()
    for n in range(125):
        (root/'private'/f'private-{n:03}.txt').touch()
    args = ['--root', str(root), '--state-dir', str(state)]
    subprocess.run([BINARY, *args, 'scan'], capture_output=True, check=True)

    def cli(*command, ok=True):
        result = subprocess.run([BINARY, *args, *command], capture_output=True, text=True)
        check((result.returncode == 0) == ok, result.stderr)
        return json.loads(result.stdout) if ok else result.stderr

    first = cli('search', 'ext:txt', '--limit', '337', '--json')
    check(first['matched'] == 10182 and len(first['entries']) == 337, first)
    token = first['page']['next_cursor']
    second = cli('search', 'ext:txt', '--limit', '337', '--json', '--cursor', token)
    check(second['page']['offset'] == 337, second)
    check(not {e['id'] for e in first['entries']} & {e['id'] for e in second['entries']}, 'CLI page duplicates')
    back = cli('search', 'ext:txt', '--limit', '337', '--json', '--cursor', second['page']['previous_cursor'])
    check(back['entries'] == first['entries'], 'CLI previous window changed ordering')
    check(isinstance(cli('browse', '--json'), list), 'Legacy browse JSON array changed shape')
    browsed = cli('browse', '--page-json', '--sort', 'name', '--limit', '337')
    check(browsed['matched'] == 10058 and browsed['page']['offset'] == 0, browsed)
    check(len(cli('browse', '--json', '--sort', 'name', '--limit', '337', '--cursor', browsed['page']['next_cursor'])) == 337, 'Legacy browse cannot continue')
    cli('search', 'different', '--json', '--cursor', token, ok=False)
    cli('search', 'ext:txt', '--json', '--limit', '1001', ok=False)
    cli('browse', '--page-json', '--cursor', token, ok=False)

    mcp = Client([*args, 'mcp'])
    mcp.request('initialize', {'protocolVersion': '2025-11-25'})
    current = mcp.tool('axit_search', {'query': 'ext:txt', 'limit': 500})['structuredContent']
    entries = []
    while True:
        check(current['page']['offset'] == len(entries), 'MCP skipped a window')
        entries.extend(current['entries'])
        cursor = current['page']['next_cursor']
        if cursor is None:
            break
        current = mcp.tool('axit_search', {'query': 'ext:txt', 'limit': 500, 'cursor': cursor})['structuredContent']
    check(len(entries) == 10182 and len({e['id'] for e in entries}) == 10182, 'MCP results truncated or duplicated')
    check([e['name'] for e in entries] == sorted(e['name'] for e in entries), 'MCP sort differs between windows')
    home = mcp.tool('axit_search', {'query': 'ext:txt', 'cursor': current['page']['first_cursor']})['structuredContent']
    check(home['page']['offset'] == 0 and home['entries'] == first['entries'][:100], 'MCP first window failed')
    end = mcp.tool('axit_search', {'query': 'ext:txt', 'limit': 71, 'cursor': home['page']['last_cursor']})['structuredContent']
    check(end['page']['offset'] == 10111 and end['entries'] == entries[-71:], 'Changed page size lost final entries')
    for bad in [None, 42, '', '00'*89, 'x'*178, token.upper()]:
        check(mcp.tool('axit_search', {'query': 'ext:txt', 'cursor': bad})['isError'], 'Invalid cursor accepted')
    check(mcp.tool('axit_search', {'query': 'ext:txt', 'sort': 'size', 'cursor': token})['isError'], 'Cursor reused with a new sort')
    check(mcp.tool('axit_browse', {'cursor': token})['isError'], 'Search cursor used to browse')
    directory = mcp.tool('axit_browse', {'sort': 'name', 'limit': 500})['structuredContent']
    check(directory['matched'] == 10058, 'Browse only counted the returned prefix')
    next_dir = mcp.tool('axit_browse', {'sort': 'name', 'limit': 500, 'cursor': directory['page']['next_cursor']})['structuredContent']
    check(next_dir['page']['offset'] == 500 and next_dir['children'][0]['name'] == 'file-00500.txt', 'Browse sorted a truncated prefix')
    private_id = cli('search', 'type:dir re:^private$', '--json')['entries'][0]['id']
    check(mcp.tool('axit_browse', {'entry_id': private_id, 'sort': 'name', 'cursor': directory['page']['next_cursor']})['isError'], 'Cursor reused in a sibling folder')

    acp = Client([*args, 'acp'])
    acp.request('initialize', {'protocolVersion': 1})
    scoped = acp.request('session/new', {'cwd': str(root/'private')})['result']['sessionId']
    wider = acp.request('session/new', {'cwd': str(root)})['result']['sessionId']

    def prompt(session, text, ok=True):
        response = acp.request('session/prompt', {'sessionId': session, 'prompt': [{'type': 'text', 'text': text}]})
        check(('result' in response) == ok, response)
        if not ok:
            return
        chunks = [n for n in acp.notifications if n.get('params', {}).get('update', {}).get('sessionUpdate') == 'agent_message_chunk']
        return json.loads(chunks[-1]['params']['update']['content']['text'])

    scoped_first = prompt(scoped, '/search ext:txt')
    check(scoped_first['matched'] == 125 and len(scoped_first['entries']) == 100, 'ACP scoped total incorrect')
    scoped_token = scoped_first['page']['next_cursor']
    scoped_last = prompt(scoped, f'/search --cursor {scoped_token} ext:txt')
    check(scoped_last['page']['offset'] == 100 and len(scoped_last['entries']) == 25, 'ACP cannot continue scoped search')
    check(all(e['path'].startswith(str(root/'private')+'/') for e in scoped_last['entries']), 'ACP continuation escaped its folder')
    prompt(wider, f'/search --cursor {scoped_token} ext:txt', ok=False)
    prompt(scoped, f'/search --cursor {token} ext:txt', ok=False)
    largest = prompt(scoped, '/largest')
    largest_end = prompt(scoped, '/largest --cursor '+largest['page']['next_cursor'])
    check(largest_end['page']['offset'] == 100 and not largest_end['page']['next_cursor'], 'ACP largest cannot continue')
    acp.close()

    # Publication invalidates server cursors; an open immutable reader remains usable until then.
    mcp.tool('axit_scan')
    deadline = time.monotonic()+15
    while time.monotonic() < deadline:
        if not mcp.tool('axit_status')['structuredContent']['scanning']:
            break
        time.sleep(.02)
    check(mcp.tool('axit_search', {'query': 'ext:txt', 'cursor': token})['isError'], 'Stale generation cursor accepted')
    check(mcp.tool('axit_search', {'query': 'ext:txt'})['structuredContent']['matched'] == 10182, 'Cannot restart after publication')
    cli('search', 'ext:txt', '--json', '--cursor', token, ok=False)
    mcp.close()

print(f'Protocol checks passed: {checks} · MCP read-only/write gates + ACP scoped sessions + framing + cursor pagination')

