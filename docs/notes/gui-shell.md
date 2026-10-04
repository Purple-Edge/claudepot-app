# The Tauri shell — measurements and reversals

Working notes, moved out of `AGENTS.md` on 2026-09-17. `AGENTS.md` is
`@`-included into every session and states the rules in one line each;
this file carries what was measured in the renderer and the data directory, and which obvious next steps were tried and rejected.

Read it before changing anything it covers. The one-liners say *what*;
this says *why*, and why the obvious alternative is wrong — which is the
part that stops a decision being re-litigated or silently reverted.

## GUI (Tauri)

- `src-tauri/src/commands/` — async Tauri commands wrapping `claudepot-core`,
  sliced by domain (`mod.rs` + one file per surface). NO business logic.
- `src-tauri/src/dto.rs` — serde DTOs crossing to JS. Credentials never cross.
- `src/App.tsx` + `src/api/` (sliced by domain — `account`, `project`,
  `notification`, `activity`, etc., merged in `index.ts`) + `src/types/`
  (sliced by domain, merged in `index.ts`) — React UI, plain CSS.
- `AccountStore.db` is `Mutex<Connection>` so stores can cross `await` points in Tauri commands.

**Opening a transcript parses it once.** It used to parse it four
times: `read_session_detail_at_path` called `scan_session` (the row
fold) *and* `parse_events` (the event fold), and the viewer issued
`session_read_path` and `session_chunks` in a `Promise.all`, each of
which ran that same pair. The comment defending the double fetch said
"typical sessions are <1 MB"; measured on this machine, 375 transcripts
are over 1 MB, 100 over 10 MB, and the largest is **181 MB**. The two
folds now share one `serde_json::from_str` per line
(`scan_session_with_events`), and chunks — a pure function of the
events — ride back on `SessionDetailDto` instead of being their own
command. The *Tauri command* `session_chunks` was deleted rather than left unread — `claudepot_core::session_chunks` is still the module that builds them and the CLI still calls it, so a grep for the name finds nine files and none of them is a command.

`core_tests::scan_with_events_matches_the_two_separate_passes` pins the
one-pass fold against the two-pass one, over a fixture with a blank
line and a malformed line, because those are exactly where the two
loops differed.

**What is NOT worth doing here: windowing the transcript at the IPC
boundary.** It looks like the obvious next step and it is not — the
181 MB file yields **0.9 MB** of event text, because base64 image
payloads are dropped during parse (`tool_result` keeps only `text`
parts). The cost was never the payload; it was the parse. Measure
before adding a paging protocol the search box would then have to work
around.

**`project_list` caches only the nested half of its directory walk.**
The recursive size+mtime walk is 29,810 `stat` calls over 11 GB here —
1.1–1.3 s in release, paid on every mount of the Projects tab and every
⌘R — and ~90% of it is below the top level, in the per-session folders
CC writes beside each transcript. `src-tauri/src/project_size_cache.rs`
holds that share in memory (not persisted: it is a pure function of the
filesystem, so a file in the data dir would be a thing to migrate and
invalidate for a number rebuilt in under a second). The top level is
measured fresh every listing, so counts, transcript bytes and every
flag that gates behaviour are exact; what lags by at most one listing
is the nested contribution to one size column and a sort key.

Two details are load-bearing:

- **`is_empty` may not read a cached number.** It is the flag that can
  put a project in front of a delete button. It now tests
  `!has_subdirs` and the *top-level-only* byte sum, both from the
  shallow pass, so it answers identically with or without a cache. That
  is also strictly safer than the recursive test it replaces: a
  directory holding an empty subtree summed to under 4 KiB and read as
  "empty".
- **Parallelising instead was measured and is not enough** — 1.25 s →
  0.72 s, because the work is stat-bound and one slug dominates. The
  listing runs on rayon *as well*, but the cache is what makes a repeat
  listing cheap.

**The data directory has its own note.** The store inventory, the pragma
rule and the `sessions.db` invariants that used to follow here are in
[`data-dir.md`](data-dir.md); the index loop that broke the last of
those invariants is further down this file.

**Launch-at-login is release-only, and a release launch re-registers it
every time.** `tauri-plugin-autostart` writes `current_exe()` verbatim
into `~/Library/LaunchAgents/Claudepot.plist`, so flipping the toggle
inside a `tauri dev` build installed `target/debug/claudepot-tauri` as
the login item; every login after that started a bare debug binary whose
`devUrl` had no Vite behind it — a blank window that looked like a
webview or proxy fault, and was blamed on one for a while (2026-08-26).
Two halves, both in `lib.rs`: the plugin is registered under
`#[cfg(not(debug_assertions))]` only, so a dev build has nothing to call
(`GeneralPane` hides the row via `AppStatus.dev_build`); and a release
build calls `enable()` again at setup whenever `is_enabled()`, which is a
plain plist rewrite — no `launchctl` — so a moved bundle or a stale dev
registration heals itself on the next launch instead of persisting until
someone toggles the switch twice.

**The status bar's pin is `window_always_on_top` in `preferences.json`,
and a launch re-applies it.** `set_always_on_top` is window state, not
a setting the window reads, so a fresh process starts at the normal
level whatever the file says; `setup()` re-applies it beside the
show-on-startup check. `preferences_set_window_always_on_top` changes
the level *before* it persists and puts both the level and the
in-memory field back if the save fails — file, window and button must
never disagree, because a pressed pin over a window that sinks behind
the next click is the one state the control exists to prevent. The
button sits at the bar's right end, just before the service-status
dot, which keeps the corner. It is the same kind of control as the
sidebar toggle at the far left — it acts on the window itself rather
than reporting state, so design.md's "a surface that only reports
state is not a control" does not apply to it — and it lives among the
ambient chips only because that is where its owner wanted it, not
because it is one. `useWindowPin` is the renderer half —
optimistic, reverting on a rejected call, and following
`cp-prefs-changed` like every other reader of that file.

**A packaged build has no reload affordance, and the guard is in the
renderer because the ErrorBoundary's Reload must keep working.**
`useWebviewChromeGuard` cancels the keys a webview reads as reload — F5
and its Ctrl/Shift hard-reload variants, ⌘R / ⌃R, ⌘⇧R / ⌃⇧R — and
suppresses the native context menu, which carries the browser's Reload
(WebView2's and webkit2gtk's default menus do; WKWebView's was not
measured here), for any target that is not an editable field or the
element holding a live selection (Copy / Paste / Look Up are what that
menu is for in an app). Off in dev, where reload and Inspect Element are
the loop.

A reload here is a browser artifact: no address bar, no tab to restore,
and nothing on screen saying the window is disposable — while it
discards every piece of state that lives only in the renderer, an open
modal and a half-typed secret included. But `location.reload()` is
deliberately untouched: guarding the *input* is what leaves the
ErrorBoundary's recovery button working, and it is also why this is not
Tauri's navigation handler, which sees a reload and cannot tell a
keypress from the app's own decision.

Four details are load-bearing:

- **It cancels and never stops propagation**, so ⌘R still reaches the
  section handler that refreshes the list. Only Accounts and Projects
  pass one — on the other eight sections the documented ⌘R does nothing,
  which is how the key was reaching the webview at all.
- **It is deliberately not behind `isShortcutContextBlocked()`.** A
  suppression is not a shortcut: a focused field is where a reload costs
  the most, and cancelling a keystroke the webview would have eaten
  takes nothing from the person typing.
- **Capture phase**, so a modal or palette input that calls
  `stopPropagation` on its own keydown cannot hide the key from it. The
  context-menu half is the opposite — bubble phase on `document`, after
  React's root handlers, so every app context menu (account card,
  project row, session row) keeps the `preventDefault` it already does.
- **The exposure differs per platform, and only part of it is closed
  from here.** On macOS ⌘R appears never to have reached the webview
  at all: wry's `performKeyEquivalent` hands the key to the app menu,
  and `app_menu.rs` binds no accelerator on View items on purpose —
  read from that key path rather than measured, so treat the context
  menu as the Mac story and the keys as belt-and-braces. On WebView2 F5 / Ctrl+R /
  Ctrl+Shift+R are *browser accelerator keys*, on by default; wry can
  turn them off (`with_browser_accelerator_keys`) and Tauri 2.12 does
  not expose it, so the keydown is the only layer the renderer has —
  and whether Chromium lets a page cancel those is **not verified**.
  WebView2's own context menu is likewise still native. Closing either
  properly means `with_webview` plus `ICoreWebView2Settings3`, which no
  machine here can behaviourally test.

## The index loop, and the 34 GB it wrote (2026-09-22)

The GUI stayed up for 33 hours with its window and tray icon
unresponsive while the 2-minute index loop (`lib.rs`, "search index")
kept logging, and it was restarted without anyone learning what the main
thread was doing. The freeze itself is **still unexplained**; what the
investigation did establish is below, and the watchdog at the end exists
so the next one is not.

**What was measured.** macOS's resource report
(`/Library/Logs/DiagnosticReports/claudepot-tauri_*.diag`, event
"disk writes") recorded **34.36 GB written in 72,949 s** — 471 KB/s,
sustained — with 3,074 stack samples. The release binary is stripped,
so the frames were symbolicated by disassembling around each return
address and reading the SQL string each call site loads: 820 samples in
`INSERT INTO tool_calls`, 729 in `INSERT INTO exchanges` (with the FTS
trigger beneath it), 901 in the backfill's `COMMIT`. **80% of the
writes were `backfill_claude_exchanges`; 89% were the loop.** Refreshes
that normally take 50–300 ms took 13–75 s whenever one long session was
live, and every other writer of `sessions.db` logged "database is
locked".

**The mechanism.** For every transcript whose `(size, mtime, inode)`
moved, the backfill deleted all of that file's exchange rows and
re-inserted them, and the whole pass ran inside one transaction holding
the `SessionIndex` mutex — parse included. A live transcript moves on
every pass, and the one live here was 263 MB, so it was rewritten in
full every two minutes and the write lock was held for most of each
two. `session_index::refresh` did the same, on a smaller scale, to
`session_turns` and `usage_event` (about 3,400 event rows a refresh).

**What replaced it: write only what differs.** `exchange_rows::reconcile`
(both the Claude and the Codex backfill), `turns::replace_turns` and
`usage_store::reconcile_events_for_file` make a file's rows equal to a
fresh parse and touch only rows that differ. An `UPDATE … WHERE <any
column IS NOT the new value>` that finds nothing to change dirties no
page — measured, 2,000 unchanged upserts over a 38.8 MB table added
**0 bytes** to the WAL, one changed row 53 KB — so an append costs what
was appended. The end state is exactly delete-and-reinsert's, and
`backfill_after_every_append_matches_one_backfill_of_the_whole_file`
pins that at every line boundary and mid-line cut of an edge-case
transcript. Two latent bugs went with the full rewrite: `memory_links` rows
pointing at an exchange were cascade-deleted on every pass, because the
exchange was deleted before its identical replacement was inserted; and
every re-scan after the daily raw-event GC re-inserted the evicted
events, counting their days in `usage_daily` a second time. A re-scan of
a file the cache already held now skips events older than the last GC
cutoff (`a_rescan_after_gc_does_not_count_evicted_events_again`).
Rollups inflated before this fix stay inflated until Settings → Cleanup
rebuilds the index.

**Why not the obvious incremental design.** Indexing only the bytes
appended since a stored offset was designed first and sent to Codex in
refute mode. It survives on paper, but needs: one line-framing policy
shared by full and incremental parses (today's parser indexes a final
line with no newline, which an offset would then replay), migration of
every legacy row before the unchanged-file skip, invalidating the cursor
when `forget` or an older binary rewrites the rows, and a coherent seed
snapshot across processes. Exchange-granular resumption is not enough
either: the longest exchange here was 64.6 MB, and a 46 MB subagent
transcript was a single exchange. Reconciling by row id has none of that
machinery, at the cost of still *reading and parsing* each changed file
every pass — about 1 s of CPU for 263 MB, against the writes it removes.
Revisit only if that parse becomes the measured cost.

**The lock is held per file, not per pass.** Each changed file is parsed
with no lock, then written in its own `BEGIN IMMEDIATE` transaction that
first re-reads the file's `exchange_state` row and abandons the write if
another process indexed the file since this pass's snapshot — that
process may have written *newer* content, such as a redaction. Such a
file is reported as `conflicted`, which is why `session redact` now
calls `reindex_file_verified` and fails unless the file is indexed at
its rewritten bytes; "the backfill ran" used to be reported as "the
removed content is no longer searchable".

**One refresh at a time per handle.** Every read entry point refreshes
first, so N index-backed calls arriving together each walked the corpus
and each re-parsed and rewrote the live transcript. `RefreshGate`
serializes whole refreshes and lets a waiting caller return if a
refresh that *began* after it arrived has finished (the start, not the
finish: a refresh that began earlier may have walked past the file the
caller wants). The per-call `SessionIndex::open`s in `artifact_usage`,
`top_costly_prompts`, `session_worktree_groups`, `session_index_rebuild`
and the invalidation and agent-event orchestrators now borrow the shared
handle through `shared_or_open` — they bypassed the gate, and each open
queued for the write lock (`apply_schema` begins `IMMEDIATE`).

The refresh log line now carries `walk_ms`, `scan_ms`, `db_wait_ms`,
`write_ms` and `gate_wait_ms`, and the backfill line carries
`rows_written` and `elapsed_ms`. A 60 s refresh used to be one number
that could not say whether it was parsing, waiting or writing.

**The main-thread watchdog** (`main_thread_watchdog.rs`) pings the main
thread from its own OS thread once a second, logs a stall at 5 s and its
end, and on macOS runs `/usr/bin/sample` once a stall reaches 15 s,
leaving `main-thread-stall-*.txt` beside the log (at most one an hour,
newest five kept). The samples are unsymbolicated for the same reason
as above; the system frames and the string-reference method are enough.

## The shell and the section registry

Moved out of `AGENTS.md` on 2026-10-02.

GUI is paper-mono shell: custom 38px `WindowChrome` at top
(breadcrumb + ⌘K palette hint + bell + theme toggle), 240px `Sidebar`
on the left (swap targets + primary nav + live Activity strip
+ synced strip — **starts collapsed to its rail**; ⌘\ or either
chevron expands it, the choice is remembered per device in
localStorage as an explicit three-state value, and there is no
Settings toggle for it), content column, 24px `StatusBar` at bottom.
The screenshot capture expands it before navigating, because the
rail hides the labels the script matches on.
Primitives live in `src/components/primitives/`. Sections live
under `src/sections/`; the registry (`src/sections/registry.tsx`)
is the single source of truth for primary nav. Sections in order:
Accounts, Activities (id `events` for localStorage compatibility,
label "Activities" — live + today/month dashboard + cards stream),
Projects (hosts per-project sessions in ProjectDetail's
master-detail pane), Knowledge (id `shared-memory` — dashboard,
curated base, review queue, and recall over indexed Claude + Codex
transcripts, memories,
decisions), Keys, Providers (id `third-party`, localStorage
compatibility), Agents (id `automations`, ditto), Config (id `config`, renamed from Global —
the component is still `GlobalSection` and its tab metadata still lives
in `sections/global/tabs.ts`; `LEGACY_SECTION_IDS` maps the old id),
Boards (id `boards` — durable agent-written surfaces; see
`claudepot-core::board`), Settings.
**Ten** top-level tabs, one of which (Boards) is **off by default**
and toggled with ⌃⌥⌘B or Settings → General. The enabled list lives
in `src/lib/optionalSections.ts`, and every consumer — sidebar,
palette, ⌘ bindings, shortcuts modal, launch picker, deep-link
bridges — derives from it. Filtering only the sidebar would leave a
hidden section still reachable by ⌘9 and ⌘K, which is worse than
either state.
Boards sits ninth on purpose: `useSection`
binds ⌘1..⌘9 to the first nine, so that position gives it ⌘9 and
pushes Settings to tenth, which costs nothing because Settings has
its own ⌘, in `useShellShortcuts`.
Cleanup (session prune + trash) lives at Settings → Cleanup.

Everything that enumerates the sections reads
`sections` from the registry — the ⌘K palette
(`usePaletteActions`), the ⌘1..⌘9 bindings (`useSection`), the
shortcuts reference (`ShortcutsModal`), and Settings → General's
"Open on launch" picker. Each of those four used to carry its own
hand-written copy, and three of the four had drifted: the modal
documented ⌘3 as "Sessions" and ⌘4 as "Config" (neither is a
section), the launch picker offered a `sessions` id that
`useSection` silently rejected back to Accounts, and the palette
reached three of the nine sections. A new section is one registry
entry — and that entry must say what ⌘R does there (`refresh`: the files
that bind it, or why nothing can be reloaded; `registry.refresh.test.ts`
checks each named file binds it). A new *list* of sections is a review
finding.

## Long-running operations

Long-running ops (project rename, repair resume/rollback) flow
through a single op-progress pipeline:
`Tauri *_start` cmd → spawns task → emits events on
`op-progress::<op_id>` channels → the op-progress modal subscribes
by op_id. The `RunningOps` map on the backend is the polling
backstop; see `src-tauri/src/ops.rs`.

## Event channels

**Every event channel `src-tauri/src/events.rs` declares must have a
subscriber in `src/`**, and `cargo xtask verify-docs` fails when one
doesn't. Seven had none: the four tray→Desktop channels
(`tray-desktop-switched`, `tray-desktop-switch-failed`,
`tray-desktop-launch-failed`, `desktop-reconciled`), so a tray
Desktop swap left the account cards stale and a *failed* one produced
no toast, no banner, nothing — while the CLI sibling had toast, OS
banner and Undo; plus three (`desktop-adopted`, `desktop-cleared`,
`desktop-running-changed`) that only re-announced what the invoking
command had already returned, now deleted. `events.rs` had a test
called "wire-contract lock" that compared each constant to its own
literal — a tautology inside one crate cannot see the far end of a
cross-boundary contract. Deliberate non-subscriptions go in
`UNSUBSCRIBED_BY_DESIGN` in `verify_docs.rs` **with the reason**;
entries there are validated in both directions, so one cannot outlive
its rationale. A tray action emits because nothing returns to a
caller — if nobody listens, the click silently does nothing.

## Command palette (⌘K)

`src/components/CommandPalette.tsx` + `usePaletteActions` +
`components/palette/rows.ts`. Three properties hold it together:

- **One ordering, not two.** `buildPaletteRows` emits a single
  `rows` array (what renders) and `selectable` (the same list minus
  headings). A row's cursor index is assigned where the row is
  created, so `selectable[i]` *is* the i-th visible row. The
  original bug was two orderings: rows rendered grouped by category
  while Enter indexed the ungrouped production order, so Enter on a
  highlighted "Open Projects" ran "Sign Desktop out". Keyboard and
  mouse now share one `activate(row)` — if you add a row kind, add
  it there, not at a second call site.
- **Deep targets are hidden until you type.** Settings panes and
  the Config section's tabs (`GLOBAL_TABS`) carry `deep: true`. They are real palette entries but
  listing them all on an empty query buries the ten sections.
- **Pane metadata lives outside the lazy sections.**
  `sections/settings/panes.ts` and `sections/global/tabs.ts` hold
  no JSX precisely so the palette can import them without dragging
  the Settings / Global chunks into the main bundle. Deep links
  reach Settings through `triggerSettingsTab` (cold-mount
  sessionStorage hint + hot-mount event) and Config through a
  one-shot `tab:<id>` sub-route that the section consumes and
  clears.

Matching is scored (`lib/paletteScore.ts`), not boolean: tiers are
spaced 100 apart and every within-tier penalty sums to under 100, so
a scattered subsequence can never outrank a real substring hit —
which is what let "Sign Desktop out" answer a search for "set".

Shortcut gating is shared: `isShortcutContextBlocked()` in
`useGlobalShortcuts` is the one predicate for "modal open or input
focused", and `useShellShortcuts` / `useGlobalShortcuts` / the
palette all defer to it. Forking a weaker check is how ⌘K ended up
able to open over an already-open dialog.
