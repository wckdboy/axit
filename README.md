# SCRY

**See what is hiding on your disk.** A WCKD.ai terminal tool for macOS and Linux.

SCRY combines indexed filename search, a storage browser, file organization and guarded
cache cleanup in a small Rust binary. Everything runs locally. No account, model,
telemetry, network service or file-content indexing is required.

## Install

Release packages support Apple Silicon, Intel Macs, Linux ARM64 and Linux x86-64.
Linux packages are statically linked with musl. The installer checks SHA-256 before
installing to `~/.local/bin`; it does not modify your shell configuration or run a scan.

```sh
sh install.sh
scry
```

For a private GitHub release, the installer uses your existing `gh` authentication.
Public releases use HTTPS downloads when `gh` is unavailable.

From a source checkout with Rust 1.88 or newer:

```sh
sh install.sh --local
```

Set `SCRY_INSTALL_DIR` to change the install location, or `SCRY_VERSION=v0.1.0` to pin
a release. Re-run the installer to upgrade. No sudo is needed for installation.

## Use

```sh
scry                               # TUI; creates an index on first launch
scry --root "$HOME" scan            # refresh your home index
scry --root /System/Volumes/Data scan  # all reachable Mac Data-volume files
scry search 'ext:pdf size:>100mb'
scry search 'path:projects !target re:report.*2026' --sort modified
scry browse                        # largest children of the index root
scry status                        # free space, index size and coverage
scry doctor                        # physical volumes and access failures
```

TUI keys:

- `1` Space, `2` Find, `3` Clean, `4` Activity.
- `/` edits search; `Tab` moves between search and results.
- Arrow keys or `j/k` navigate; `Enter` opens a folder; `Backspace` goes up.
- `Space` selects; `Esc` clears selection.
- `t` previews Trash, `m` previews a move, `x` verifies a disposable cache.
- `u` undoes a reversible move, `a` configures automatic cleanup, `r` rescans.
- `s` cycles search sorting; `?` opens help; `Ctrl-Q` always quits.

Search supports quoted terms, `*.zip` wildcards, `!excluded`, `re:expression`,
`ext:pdf;png`, `type:file`, `type:dir`, `size:>2gib`, `older:30d` and `path:project`.
Plain terms use case-insensitive ASCII matching. Terms are combined with AND.
Size filters use allocated disk space. `gb` is decimal; `gib` is binary.

## Understand the numbers

The volume meter reports filesystem free space separately from indexed folder totals.
Folder sizes include descendants, so parent and child rows overlap. Hardlinks count
once; APFS clones can share blocks. Sparse files can have large logical sizes and
small allocated sizes. An allocated-size estimate is not a promise of recovered space.

Scans visit every reachable entry within the selected filesystem. They include hidden
files, do not follow symlinks, skip other mounted filesystems and exclude SCRY's own
state directory. Restricted/missing locations are counted and listed under Activity
and in `latest-scan.json`. On macOS, grant your terminal Full Disk Access for protected
locations. Administrator access alone does not remove macOS privacy restrictions.

For a complete physical-drive view, combine the Data-volume browser with `scry doctor`:
APFS boot, recovery, preboot and swap volumes share storage with your files. Mounted
simulator images are separate views of backing files, rather than additional capacity.

## File operations

Every operation starts with a preview. Nothing selected is moved or removed when a
plan is created. Applying requires the exact `CLEAN`, `TRASH` or `MOVE` confirmation.
Plans expire after ten minutes and recheck file identity and active processes.

```sh
scry plan 12345 --action clean
scry apply PLAN_ID --confirm CLEAN
scry plan 54321 --action move --to "$HOME/Documents/organized"
scry apply PLAN_ID --confirm MOVE
scry undo
```

Moves atomically refuse to replace an existing file. Moves and Trash are reversible
through the local journal; moving to Trash does not release space until Trash is emptied.
Permanent cleanup is limited to recognized generated cache directories. It requires
ownership, cache/project markers, Git-ignored status where applicable, no tracked
source, age checks and no current-user active files/builds. Cache fingerprints are
checked before and after an exclusive claim. Source markers, releases, archives,
signing files, model weights, symlinks and special files block permanent cleanup.
Shared external Rust targets are left for manual review.

`lsof` is required for file mutations. If process inspection fails, mutations are blocked;
scanning and search remain available. Unusual control-character/non-UTF-8 filenames
remain searchable; mutation of those names is delegated to the system file manager.

## Automatic space saving

Automatic cleanup is **off by default**. Enable it for explicit project/cache roots in
the TUI, or configure it from the CLI:

```sh
scry auto --enable --allow "$HOME/projects/example"
scry auto --run
scry auto --watch                   # check every five minutes while this command runs
scry auto --disable
```

Defaults: only below 20 GiB free; minimum age seven days; at most 2 GiB removed per
run; at most once a day. Entire home and disk roots are rejected. The TUI checks once
a minute while open. `auto --watch` runs in the foreground; no background service is
installed automatically. Each automatic run uses the same verification rules and
journal as manual cleanup.

## MCP

```sh
scry mcp --root "$HOME"
```

Example MCP client configuration:

```json
{
  "mcpServers": {
    "scry": {
      "command": "scry",
      "args": ["mcp", "--root", "/home/you"]
    }
  }
}
```

Tools: `scry_status`, `scry_scan`, `scry_search`, `scry_browse`, `scry_cleanup_plan`,
`scry_move_plan` and `scry_trash_plan`. Scans run in the background; poll status.
The launch scope is fixed. Mutations are disabled and unadvertised by default.
Explicit `--allow-mutations` adds annotated apply/undo tools; clients must obtain
user approval for destructive calls. Plans still require exact confirmation and live
revalidation. The server uses bounded newline-delimited stdio JSON-RPC, accepts at
most four active requests, and supports search cancellation.

## ACP

```sh
scry acp --root "$HOME"
```

This Agent Client Protocol v1 adapter exposes deterministic metadata commands:
`/search QUERY`, `/largest`, `/status`, `/scan` and `/plan ID…`. Sessions are scoped to
their working directory within the server's launch scope. Results stream as agent
message chunks. No language model runs inside SCRY. File execution stays in the TUI
or explicitly enabled MCP tools. External MCP servers and conversation history are
not advertised or launched by this adapter.

## Design and resource budget

- Streaming filesystem traversal and fixed-width, little-endian records.
- Original filenames and parent ids stored once; no database server or full-path index.
- Immutable generations are memory mapped for search and published atomically.
- Search runs outside the UI thread and cancels superseded queries.
- At most 1 GiB per index; temporary construction can briefly use twice that space.
- Previous complete index survives cancellation, invalid output or a failed rebuild.
- Private state directory permissions are `0700`; index and operation records are private.

State defaults to `~/Library/Application Support/ai.wckd.scry` on macOS and
`$XDG_STATE_HOME/scry` or `~/.local/state/scry` on Linux. Override with `--state-dir`
or `SCRY_STATE_DIR`. File contents and credentials are never included in the index.

## Build and verify

```sh
cargo fmt --check
cargo clippy --all-targets --locked --jobs 2 -- -D warnings
cargo test --locked --jobs 2 -- --test-threads=1
cargo build --release --locked --jobs 2
python3 scripts/protocol-test.py target/release/scry
sh scripts/installer-test.sh target/release/scry
```

The release workflow packages all four targets and SHA-256 manifests. Development
and test builds disable incremental compilation and debug information to limit local
build storage. Filesystem mutation tests operate only on temporary fixtures.

SCRY is an independent MIT-licensed implementation inspired by
[FSearch](https://github.com/cboxdoerfer/fsearch)'s indexed-search interaction model.
Protocol references: [MCP tools](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)
and [Agent Client Protocol v1](https://agentclientprotocol.com/protocol/v1/overview).
