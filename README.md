# Axit

**Find it. Axit.** A WCKD.ai terminal tool for macOS and Linux. Home: [axit.sh](https://axit.sh).

AXIT combines indexed filename search, a storage browser, file organization and guarded
cache cleanup in a small Rust binary. Everything runs locally. No account, model,
telemetry, network service or file-content indexing is required.

![AXIT terminal storage browser, captured with temporary test fixtures](assets/tui.svg)

AXIT's **Copper** palette uses `#D9966B` for its identity and navigation, charcoal
and graphite surfaces, chalk text, cool blue information and rose destructive actions.
[View the palette](assets/palette.svg). Runtime colors live in `src/tui/theme.rs`;
text and action colors have automated contrast checks.
The split-pixel mark in the app's top-left corner and website icon shares one 8×8 source
mask, [assets/mark.txt](assets/mark.txt). Two block fragments and a diagonal cut form the 8×8 mark, in one foreground color.
Run `python3 scripts/render-logo.py` after
editing it; site checks verify both generated SVGs match.

## Project status

[Plane: Axit / AXIT](https://app.plane.so/wckd/projects/21757fa0-759f-4ce5-a114-4620fd693302/issues/)
tracks current work and the roadmap. [Axit 0.5.0](https://github.com/wckdboy/axit/releases/tag/v0.5.0)
is released. Space, Find and Clean can reach every indexed result with bounded
windows, stable sorting, first/last and previous/next navigation. Marks keep their
file identities across windows, and the storage map summarizes the whole folder.
CLI, MCP and ACP share the same cursor engine. The Apps view, reviewed file operations,
Copper palette and shared pixel mark remain included.
[axit.sh](https://axit.sh) is deployed through the standard [Wrangler workflow](site/README.md).

[Source CI](https://github.com/wckdboy/axit/actions/runs/40188996121) passed 93 Rust
tests and 227 MCP/ACP checks on macOS, and 90 Rust tests and 228 protocol checks on
Linux, plus installer/tamper, PTY/native-manager fixtures and site checks. The
[release workflow](https://github.com/wckdboy/axit/actions/runs/40189836072) built
and checked all four platform packages. Public archives match SHA256SUMS and GitHub
asset digests. Intel Mac protocol, installer and PTY checks pass through Rosetta;
the Apple Silicon public-installer upgrade preserves the existing index and
preferences. Production desktop, mobile and dark browser checks pass.
[VALIDATION.json](https://github.com/wckdboy/axit/releases/download/v0.5.0/VALIDATION.json)
records package, installation, benchmark and deployment evidence.

CodeRabbit's history-overlay and drag-anchor findings are fixed with regressions.
Full and focused CLI follow-ups report zero findings; the GitHub check passed and
the review thread is resolved in [PR #8](https://github.com/wckdboy/axit/pull/8).
The PTY harness waits for the complete reviewed file identities before checking a
terminal frame; its [CI run](https://github.com/wckdboy/axit/actions/runs/40190209154)
checks the same released application.

Pagination (`AXIT-6`) is shipped. Next: incremental filesystem updates (`AXIT-5`)
with bounded resource use and missed-event recovery. Native Linux distro ownership
and removal (`AXIT-18`) remains planned work.

## Install

Release packages support Apple Silicon, Intel Macs, Linux ARM64 and Linux x86-64.
Linux packages are statically linked with musl. The installer checks SHA-256 before
installing to `~/.local/bin`; it does not modify your shell configuration or run a scan.

```sh
curl -fsSL https://axit.sh/install.sh | sh
axit
```

Release downloads need no Rust toolchain or GitHub sign-in. The same installer is
available from [GitHub Releases](https://github.com/wckdboy/axit/releases/latest/download/install.sh).
From an existing checkout, you can also run `sh install.sh`.

From a source checkout with Rust 1.88 or newer:

```sh
sh install.sh --local
```

Set `AXIT_INSTALL_DIR` to change the install location, or `AXIT_VERSION=v0.5.0` to pin
a release. Re-run the installer to upgrade. No sudo is needed for installation.

Renamed from SCRY: existing private indexes, cleanup preferences and the undo journal
are reused in place when no Axit store exists. No index copy is needed.
Legacy `SCRY_STATE_DIR`, `SCRY_TRASH_DIR` and installer variables remain accepted;
the `AXIT_*` equivalents take precedence. MCP advertises `axit_*` tools and accepts
the previous `scry_*` names for existing clients.

## Use

```sh
axit                               # TUI; creates an index on first launch
axit --root "$HOME" scan            # refresh your home index
axit --root /System/Volumes/Data scan  # all reachable Mac Data-volume files
axit search 'ext:pdf size:>100mb'
axit search 'path:projects !target re:report.*2026' --sort modified
axit browse                        # largest children of the index root
axit status                        # free space, index size and coverage
axit doctor                        # physical volumes and access failures
```

TUI keys:

- `1` Space, `2` Find, `3` Clean, `4` Activity, `5` Apps keeps focus on navigation, so
  repeated number shortcuts never get trapped in a text field. `Enter` enters a section.
- `Alt-1…5` works even in text fields and dialogs. `Ctrl-Tab` /
  `Ctrl-Shift-Tab` switches sections; `Ctrl-PageDown/Up` is an alternative for terminals
  that intercept those keys. Switching sections safely cancels a pending review.
- `F6` toggles navigation/content focus. In navigation, arrows choose a section and
  `Enter` or `Down` enters it. `/` or `Ctrl-F` edits search; plain digits remain text there.
- `Up` at the first row, search field or top of a panel returns to navigation.
  `Left` / `Right` then changes sections; `Down` returns to content. `Ctrl-Up`
  jumps straight to navigation, safely canceling any pending review.
- `Tab` / `Shift-Tab` cycles navigation, query, results, inspector and action buttons.
  Arrow keys choose a focused button; `Enter` activates it.
- Arrow keys or `j/k` navigate; `PageUp` / `PageDown` moves by the viewport and
  crosses result windows. `Home` / `End` jumps to the first/last result.
- `[` / `]` or Prev/Next buttons switch bounded result windows. Wheel/touchpad
  scrolling crosses windows at their boundaries. The list title shows its full range.
- `Enter` opens a folder or inspects a file; `Backspace` goes up. `Alt-Left` /
  `Alt-Right` retraces history; `Alt-Up` goes up; `Alt-Home` returns to the scope root.
- `Space` toggles a mark, `Shift-Up/Down` selects a range, and `Esc` clears marks.
  Up to 128 marked items can be reviewed together, including marks on other windows.
  Sorting preserves marks; changing folder or query clears them. `Ctrl-A` marks the
  current window when it fits the 128-item review limit.
- `Delete` / `t` previews Trash, `m` previews a move, `x` verifies a disposable cache.
- `u` undoes a reversible move, `a` configures automatic cleanup, `r` rescans.
- `s` sorts the current list; `p` shows an item's containing folder; `g` toggles the Space map.
- `?` opens scrollable help; `q` / `Ctrl-Q` quits. An in-progress file operation
  finishes before exit; canceling a preview never changes files.

The minimum terminal size is 48 columns × 16 rows; smaller windows show a resize hint.

Mouse and touchpad:

- Click tabs, breadcrumbs and action buttons. Buttons activate on release.
- Click a row to focus it; double-click to open/inspect; right-click for item actions.
- Click a storage-map tile to select its row; double-click a folder tile to enter it.
- Click `[ ]` or Ctrl-click to toggle marks; Shift-click or drag selects a range.
- Wheel / touchpad scrolling works in lists, the inspector and dialogs. The scrollbar
  supports clicking and dragging.
- A deletion opens a reviewed plan with Cancel selected. Click its explicit action
  button, or use Tab then Enter. Escape cancels; typing and paste never confirm a dialog.

Search and destination fields support cursor editing, Unicode, horizontal scrolling
and bracketed paste. `Ctrl-A/E` moves to the start/end; `Ctrl-U` clears the field.
Background scans preserve your current input focus and query.
Mouse input requires a terminal that forwards mouse events. Use your terminal's
selection modifier (commonly Shift) to select and copy text normally.

Completed operations update rows immediately. Undo restores them without forcing a
full disk scan when the original path is still indexed. If you have rescanned since
the move and its original path is absent, Undo starts a background refresh so the
restored file reappears. A notice identifies stale indexed sizes; press `r` when ready to
refresh. The current folder and cursor survive a refresh. A small private
`tui-state.json` remembers removed rows for the matching index generation.

Search supports quoted terms, `*.zip` wildcards, `!excluded`, `re:expression`,
`ext:pdf;png`, `type:file`, `type:dir`, `size:>2gib`, `older:30d` and `path:project`.
Plain terms use case-insensitive ASCII matching. Terms are combined with AND.
Size filters use allocated disk space. `gb` is decimal; `gib` is binary.

### Result windows

Space, Find and Clean keep at most 1,000 visible rows and can continue through every
match. Sorts apply to the full result set: name ascending, allocated size descending,
or modification time descending, with raw filename and entry id breaking ties.
The Space map always summarizes the largest eight children across the whole folder.
Marks refer to entry ids, so reviews include marked files outside the visible window.

CLI search JSON and MCP search/browse include `page` with zero-based `offset`,
`next_cursor`, `previous_cursor`, `first_cursor` and `last_cursor`. A missing direction
is `null`. Supply the chosen token as `--cursor TOKEN` (CLI) or `cursor` (MCP) with the
same query/folder and sort. `matched` counts all results. Limits are 1–1,000 for CLI
and 1–500 for MCP; ACP returns 100 per window. The page size can change between calls.

```sh
axit search 'ext:pdf' --json --limit 100
axit search 'ext:pdf' --json --limit 100 --cursor TOKEN
axit browse --page-json --sort name --limit 100
axit browse --page-json --sort name --limit 100 --cursor TOKEN
```

`browse --json` preserves its original bare array. Use `--page-json` for the envelope
and continuation tokens. Text output prints the result range and next token to stderr.
Tokens are opaque and tied to one immutable index generation, filter, scope, sort
and removed-row overlay. Age filters hold their clock steady while paging. Restart
from the first window after a new scan or a changed query; stale/mismatched tokens
are rejected. Tokens are continuation hints, not authentication credentials.

## Applications

Open `axit apps` or choose `5 Apps`. This view reads bounded application metadata
without building a disk index. `/` filters; `i` installs; `Delete` removes;
`f` reviews related files; `l` switches installed apps/data candidates; `r` reloads apps.
Click rows and buttons, or use arrows, Tab and Enter. Files reviews support Space,
Home/End, PageUp/Down and the mouse wheel. Personal data stays unchecked by default.

```sh
axit apps list --json
axit apps inspect 'Application Name'
axit apps install firefox --provider brew       # preview exact native command
axit apps install org.mozilla.firefox --provider flatpak
axit apps uninstall 'Application Name'          # preview only
axit apps leftovers --json
axit apps trash-data 'leftover:org.example.App' --leftover --include-data
```

On macOS, standalone `.app` bundles use a reviewed, reversible Trash plan. Common
identifier-based caches are selected with the bundle. Preferences, support files,
containers, name-only matches and package-recipe hints need explicit selection.
No vendor-prefix sweeps, shared group-container deletion or credential-store cleanup
is performed. A changed or running primary bundle blocks its related-data removal.
Normal writable `/Applications` bundles can be removed without elevating Axit;
protected system apps and folders needing administrator authorization stay blocked.

Homebrew casks on macOS and Flatpak apps in the standard user/system Linux
installations use their native package manager.
Review the exact command, then confirm; its original prompts remain available while
Axit temporarily returns the terminal. No `--zap`, force removal or user-data deletion
is added. Axit Undo covers files moved to Trash, not native package-manager actions.
CLI execution needs `--confirm INSTALL` / `UNINSTALL` for native operations, or
`--confirm TRASH` for standalone/data Trash. Without these flags, commands only preview.

Data candidates combine bounded identity history with exact identifiers in known
home folders. Malformed history is recoverable; the next write keeps one private
`apps-history.unreadable.json` backup. Unmatched folders may belong to services or shared software; nothing is
selected automatically for them. `--include-data` and `--include-review` are separate
CLI choices. Shared identifiers keep data protected. Native Linux distribution
packages and arbitrary installer scripts are not uninstalled by Axit; their desktop
launchers and related data can be inspected. No real user applications are modified
by the test suite; package operations use a marked temporary-fixture helper.

Apps and its Undo reload the app list without forcing a disk scan. Directory sizes
without index metadata are labeled unknown; indexed estimates may be stale. Choosing
Space/Find/Clean without an index starts the normal metadata scan.

## Understand the numbers

The volume meter reports filesystem free space separately from indexed folder totals.
The Space map compares the largest eight direct children with all remaining children
grouped as Other items. These are disjoint child estimates, with sizes taken from
the index. Refresh after filesystem changes. `g` hides the map for a taller file list.
Folder sizes include descendants, so parent and child rows overlap. Hardlinks count
once; APFS clones can share blocks. Sparse files can have large logical sizes and
small allocated sizes. An allocated-size estimate is not a promise of recovered space.

Scans visit every reachable entry within the selected filesystem. They include hidden
files, do not follow symlinks, skip other mounted filesystems and exclude AXIT's own
state directory. Restricted/missing locations are counted and listed under Activity
and in `latest-scan.json`. On macOS, grant your terminal Full Disk Access for protected
locations. Administrator access alone does not remove macOS privacy restrictions.

For a complete physical-drive view, combine the Data-volume browser with `axit doctor`:
APFS boot, recovery, preboot and swap volumes share storage with your files. Mounted
simulator images are separate views of backing files, rather than additional capacity.

## File operations

Every operation starts with a preview. Nothing selected is moved or removed when a
plan is created. The TUI asks a specific question, shows paths and consequences,
and defaults to Cancel. Confirm its explicit action button to proceed.
CLI and MCP calls require the exact confirmation phrase (`CLEAN`, `TRASH` or `MOVE`).
Plans expire after ten minutes and recheck file identity and active processes.
An apply result with `needs_reconcile: true` identifies an error after paths changed.
The TUI preserves a stale-index notice across restarts; refresh before retrying.

```sh
axit plan 12345 --action clean
axit apply PLAN_ID --confirm CLEAN
axit plan 54321 --action move --to "$HOME/Documents/organized"
axit apply PLAN_ID --confirm MOVE
axit undo
```

Moves atomically refuse to replace an existing file. Moves and Trash are reversible
through the local journal; moving to Trash does not release space until Trash is emptied.
Permanent cleanup is limited to recognized generated cache directories. It requires
ownership, cache/project markers, Git-ignored status where applicable, no tracked
source, age checks and no current-user active files/builds. Cache fingerprints are
checked before and after an exclusive claim. Source markers, releases, archives,
signing files, model weights, symlinks and special files block permanent cleanup.
Whole Rust target directories are never permanently cleaned. This preserves custom
release profiles and packages with unfamiliar extensions. Debug and incremental
compiler caches can be cleaned independently.
Shared external Rust targets are left for manual review.

`lsof` is required for file mutations. If process inspection fails, mutations are blocked;
scanning and search remain available. Unusual control-character/non-UTF-8 filenames
remain searchable; mutation of those names is delegated to the system file manager.

## Automatic space saving

Automatic cleanup is **off by default**. Enable it for explicit project/cache roots in
the TUI, or configure it from the CLI:

```sh
axit auto --enable --allow "$HOME/projects/example"
axit auto --run
axit auto --watch                   # check every five minutes while this command runs
axit auto --disable
```

Defaults: only below 20 GiB free; minimum age seven days; at most 2 GiB removed per
run; at most once a day. Entire home and disk roots are rejected. The TUI checks once
a minute while open. `auto --watch` runs in the foreground; no background service is
installed automatically. Each automatic run uses the same verification rules and
journal as manual cleanup.
An empty or fully blocked check can retry after five minutes without consuming the
daily allowance. A batch accepts at most 16 caches within the verified byte budget;
oversized entries are skipped so smaller eligible caches can fit. Candidate metadata
is reused, but every destructive claim gets fresh process and identity checks.
Unavailable and out-of-scope roots are skipped without losing saved preferences.
Blocked automatic previews are not persisted, so retries do not accumulate empty plans.

## MCP

```sh
axit mcp --root "$HOME"
```

Example MCP client configuration:

```json
{
  "mcpServers": {
    "axit": {
      "command": "axit",
      "args": ["mcp", "--root", "/home/you"]
    }
  }
}
```

Tools: `axit_status`, `axit_scan`, `axit_search`, `axit_browse`, `axit_cleanup_plan`,
`axit_move_plan`, `axit_trash_plan`, `axit_apps`, `axit_app_files` and
`axit_app_data_plan`. Scans run in the background; poll status.
Search and browse accept `cursor` and `sort`; see [result windows](#result-windows).
The launch scope is fixed. Mutations are disabled and unadvertised by default.
Explicit `--allow-mutations` adds annotated apply/undo tools; clients must obtain
user approval for destructive calls. Plans still require exact confirmation and live
revalidation. The server uses bounded newline-delimited stdio JSON-RPC, accepts at
most four active requests, and supports search cancellation.

App tools `axit_apps`, `axit_app_files` and `axit_app_data_plan` inspect bounded
metadata and preview explicitly selected related-data Trash within the fixed server
scope. They do not run native package operations or uninstall app bundles. Their
plans use the existing mutation gate and exact confirmation.

## ACP

`/apps [QUERY]` lists app metadata inside the session folder; `/app KEY` inspects
related files within that same scope. Neither requires an existing disk index.

```sh
axit acp --root "$HOME"
```

This Agent Client Protocol v1 adapter exposes deterministic metadata commands:
`/search [--cursor TOKEN] QUERY`, `/largest [--cursor TOKEN]`, `/status`, `/scan`,
`/apps [QUERY]`, `/app KEY` and `/plan ID…`. Page metadata is included in each search
response. A continuation cannot be reused under a different session folder.
Sessions are scoped to
their working directory within the server's launch scope. Results stream as agent
message chunks. No language model runs inside AXIT. File execution stays in the TUI
or explicitly enabled MCP tools. External MCP servers and conversation history are
not advertised or launched by this adapter.

## Design and resource budget

- Streaming filesystem traversal and fixed-width, little-endian records.
- Original filenames and parent ids stored once; no database server or full-path index.
- Immutable generations are memory mapped for search and published atomically.
- One search worker and one listing worker run outside the UI thread. Superseded
  requests are canceled and coalesced to the latest request.
- Result windows use bounded heaps instead of sorting or retaining every match.
  Each uncached window scans the immutable metadata; deep pages do not grow the heap.
- A 32-entry generation-aware window cache keeps recent pages and folder revisits
  fast; cache candidate hints are shared for each index. Neither certifies deletion.
- At most 1 GiB per index; refreshing keeps the previous generation plus up to
  2 GiB of temporary construction space until publication completes.
- Previous complete index survives cancellation, invalid output or a failed rebuild.
- Private state directory permissions are `0700`; index and operation records are private.

State defaults to `~/Library/Application Support/ai.wckd.axit` on macOS and
`$XDG_STATE_HOME/axit` or `~/.local/state/axit` on Linux. Override with `--state-dir`
or `AXIT_STATE_DIR`. File contents and credentials are never included in the index.
`AXIT_TRASH_DIR` selects a dedicated recovery root instead of the system Trash; it is
also used by isolated integration tests. Files stay inside its `files/` directory;
Linux metadata stays inside its `info/` directory. Recovery directories must be owned
and private (`0700`); AXIT rejects publicly accessible destinations without changing
their permissions. The operation journal provides undo.

On the development Mac, a 5.98-million-entry index used 490 MiB on disk. Seven
representative searches took 20–181 ms (median of three runs each) over a persistent
MCP connection. The initial metadata scan took 297 seconds, including access errors
and cloud-file timeouts. These are measurements from one busy machine; filesystem,
query and storage performance affect results.

A read-only 0.3 benchmark on the Mac's current 5.52-million-entry index found 1,208
cache hints in 26 ms. Dispatching a cold folder load took 52 µs; section-switch
handlers took at most 21 µs while that load ran. These measure metadata discovery
and event dispatch, not complete frame latency. The benchmark reuses an existing
index and never scans or mutates indexed files:

```sh
AXIT_BENCH_STATE=/path/to/existing/state cargo test --release --locked --jobs 2 \
  benchmark_existing_index_navigation -- --ignored --nocapture
```

A read-only 0.5 run used the existing 5,518,083-entry, 473,933,635-byte index.
500-row windows across 4,705,831 file matches took 59–80 ms in the engine and
63–86 ms over persistent MCP, including last and previous windows. The index and
preferences were unchanged. These are single-machine measurements, not a latency
guarantee; the [release proof](https://github.com/wckdboy/axit/releases/download/v0.5.0/VALIDATION.json)
includes the individual measurements.

## Build and verify

```sh
cargo fmt --check
cargo clippy --all-targets --locked --jobs 2 -- -D warnings
cargo test --locked --jobs 2 -- --test-threads=1
cargo build --release --examples --bin axit --locked --jobs 2
python3 scripts/protocol-test.py target/release/axit
sh scripts/installer-test.sh target/release/axit
python3 -m venv .venv-ui
.venv-ui/bin/pip install pyte==0.8.2
.venv-ui/bin/python scripts/tui-test.py target/release/axit /tmp/axit-screens target/release/examples/apps_fixture
```

The release workflow packages all four targets and SHA-256 manifests. It tests the
native Apple Silicon and both Linux musl release binaries with the protocol,
installer and PTY suites; Intel Mac packages are also exercised locally under
Rosetta. The standard Check workflow runs on macOS and Linux, including site checks.
Run `npm --prefix site run check` to build and verify the website independently.
Fixtures with more than 10,000 files exercise every window, duplicate sort keys,
changing page sizes, stale cursors, scoped ACP continuations, off-screen marked-file
reviews, cancellation and keyboard/mouse navigation in a real PTY.
Development and test builds disable incremental compilation and debug information to limit local
build storage. Filesystem mutation tests operate only on temporary fixtures.

AXIT is an independent MIT-licensed implementation inspired by
[FSearch](https://github.com/cboxdoerfer/fsearch)'s indexed-search interaction model.
Protocol references: [MCP tools](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)
and [Agent Client Protocol v1](https://agentclientprotocol.com/protocol/v1/overview).

Native adapter references: [Flatpak list implementation](https://github.com/flatpak/flatpak/blob/main/app/flatpak-builtins-list.c), [Homebrew cask artifacts](https://docs.brew.sh/Cask-Cookbook).
