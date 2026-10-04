# Core modules — pricing, the env vault, rotation, token refresh, the corpus

Working notes, moved out of `AGENTS.md` on 2026-10-02. `AGENTS.md` is
`@`-included into every session and states the rules in one line each;
this file carries five `claudepot-core` modules whose rules are easy to
break from outside because the reason is not visible in the code.

Read the section before changing the module it covers.

## Pricing (Activities → Cost, and every "on API" figure)

Cost figures answer "what would pay-per-call have cost me". Rates are
**dated**, because a rate change must not silently re-score the past.

- `claudepot-core::session_live::pricing` — `RATE_TIERS` is the single
  source of truth: each model carries a list of `RatePeriod`s
  (`starts: Option<Ymd>` + rates), oldest first. `FAMILY_CURRENT` maps
  `claude-<family>-` to the model an unlisted member falls back to;
  it is explicit because a family can span tiers (current Opus at
  $5/$25 vs retired Opus 4.1 at $15/$75).
- `claudepot-core::pricing::PriceBook` — **the** resolution surface.
  `resolve(model, day)` returns rates plus a `RateConfidence`
  (`Exact` | `FamilyEstimate`). Nothing else should resolve rates.
- `claudepot-core::pricing::history` — observed rate changes
  (`pricing-history.json`), merged over the bundled periods. An
  observation dated `D` means "first seen on `D`", an upper bound on
  when the change landed, so observations never override a bundled
  period that already covers that day.
- `src/costs.ts` mirrors `PriceBook::resolve` and `PriceBook::cost`
  for client-side aggregation. **The two are locked together by
  `crates/claudepot-core/testdata/rate-resolution-vectors.json` and
  `cost-vectors.json`** — both run both. Change one, change the other,
  add a vector. Every bundled model needs an exact rate vector, which
  is what keeps the hand-built book in `costs.test.ts` complete.
- **Usage is counted per API message, not per transcript line.** CC
  writes a message as one line per content block, each repeating the
  full `usage`; summing lines overstated every stored total 2–6×.
  `session::usage::UsageLedger` charges a message once, and the index
  re-reads every transcript the first time a build with it opens
  `sessions.db` (`token_accounting`, a guard reset — never a
  `SCHEMA_VERSION` bump, which would cascade into shared memory).
- A cost is priced the way CC's own cost function prices it: one-hour
  cache writes at 2× input rather than 1.25× (65% of all writes on the
  reference machine), fast-mode tokens (`usage.speed`) at the model's
  fast table, US-only inference (`usage.inference_geo`) at 1.1×, and
  web searches at $0.01 each. `SessionRow::premium` carries the fast
  and US-only buckets.
- Family estimates are always marked in the UI (a leading `≈` plus a
  `title`), never presented as a quote. `ProjectUsageRow` carries
  `estimated_sessions` for the same reason.

The session figure still prices every token at the session's first
model; a session that switched models mid-way is approximate.

## Env secret vault (Keys → Secret vault, ProjectDetail → Environment files)

Optional feature: a fully-local named-secret vault plus
format-preserving per-project `.env*` editing — copy a secret out,
inject it into a project's `.env`, comment/uncomment/delete keys.
Movement layer only, not a text editor.

- Pure logic in `claudepot-core::env_vault`: `env_file` (line-
  oriented `.env` editor — every mutation touches only the target
  key's line; `parse` exposes the active/commented/absent
  tri-state), `store` (the SQLite vault).
- Tauri commands in `src-tauri/src/commands/env_secret.rs` —
  `env_vault_*` (vault) and `env_file_*` (per-project). Inbound
  secret args zeroized on every exit path; outbound values cross
  only via the Rust-side clipboard write + `KeyCopyReceiptDto`,
  never rendered. Renderer-supplied `.env` file names are validated
  as safe bare dotenv filenames (no separators / `..` / NUL).

## Auto-rotation (Settings → Rotation)

Optional feature: when the active CLI account's Anthropic
utilization on a configured window crosses a user-set threshold,
swap to a chosen alternate.

- Pure rule logic in `claudepot-core::rotation::eval` —
  `evaluate(rules, snapshot, active, audit, now) -> Vec<RuleDecision>`,
  no I/O. Tests inject the clock.
- Orchestrator at `src-tauri/src/rotation_orchestrator.rs` bridges
  to the Tauri runtime: confirm-mode emits `rotation-suggested`
  events for the toast, auto-mode calls
  `cli_backend::swap::switch_force` directly.
- Hooks into `usage_snapshot::run_tick` (the existing 5-min
  multi-account fetch). Zero overhead when no rules exist.
- Confirm is the default mode; promote to auto after watching the
  rule fire correctly. See `dev-docs/auto-rotation.md` for the
  full design including the policy framing.

## Proactive token refresh (no UI)

Always-on behavior, not a feature: keeps **inactive** accounts' access
tokens alive so every surface that needs a live token (usage windows,
Activity strip, tray report) doesn't read "Expired" for every account
except the one in use. Access tokens last about an hour; before this,
nothing refreshed a parked slot between an explicit "Verify all" and
the next account switch.

- Pure selection logic in `claudepot-core::token_refresh` —
  `is_eligible(facts, now_ms)` and
  `select_next(candidates, now, min_retry_gap) -> Option<Uuid>`, no
  I/O, clock injected.
- Orchestrator at `src-tauri/src/token_refresh_orchestrator.rs`, hooked
  into `usage_snapshot::run_tick` *before* the usage fetch so an
  account healed this tick reports live numbers in the same tick.
- **Does not implement a refresh.** It picks an account and calls
  `services::identity::verify_account_identity`, whose existing
  401 → refresh → CAS-write path already refuses to persist a rotated
  blob when the profile email drifts from the label. Reimplementing
  the exchange here would fork that protection.
- **Never the active account** — that token belongs to Claude Code,
  which rotates it on its own schedule; refreshing it from a
  background tick is the 0.2.10 sign-out bug. Also skips
  `drift`/`rejected` slots and any token that has not actually
  expired (a live token makes `/profile` return 200, so the refresh
  branch is never reached).
- **One account per tick**, ordered round-robin by last attempt rather
  than by staleness — staleness alone starves, because an account that
  fails every time stays the most stale forever. `reference.md`
  §III.4.1 records the token endpoint refusing three refreshes from one
  IP in ten minutes, so the 5-min cadence is the rate limit; there is
  deliberately no backoff state machine.

## Corpus + detectors (`claudepot corpus`)

`claudepot-core::corpus` builds `corpus.db` (see `data-dir.md` for
why it is a separate file). `corpus::normalize` is Tier 0,
`corpus::detect` is Tiers 1–3. No model calls anywhere in this path —
`claudepot corpus detect` is the free preview before any distillation.

Precision, not scale, is the problem: the naive "error then any later
success of the same tool" join yields ~358k useless pairs. The
constraints that make it usable are same-file, same **command family**,
first success only, and a bounded turn gap.

Two normalizers on purpose — `normalize_prompt` flattens numbers
wholesale; `error_signature` keeps small integers, because merging
`Exit code 1` with `Exit code 143` merges "failed" with "timed out".
Signatures take the **first line only**, redacted and capped: tool
output is arbitrary stdout and on a real machine contains financial
records.

Two filters exist because the real corpus demanded them, and removing
either re-floods the output:

- `is_harness_synthetic` — CC injects `<local-command-caveat>`,
  `<command-name>`, `<bash-stdout>`, `[Request interrupted by user]`.
  Before filtering, the largest "repeated request" in the corpus was
  harness plumbing at 1,258 occurrences.
- `command_family` skips segment-consuming shell words (`cd`, `source`)
  and comments. Real commands open `cd "/path"; …`, so a naive first
  token returns `cd`'s *argument*; and `#` produced a phantom `bash:#`
  family with 266 "verified recoveries".

**Vocabulary.** Nothing here is a *recurrence* — that word has a
precise, human-confirmed meaning in `shared_memory::recurrence` and
diluting it breaks the one honest signal the knowledge base has.
Repetition is a *repetition cluster*; a failure with no observed
success is `unresolved`, never "abandoned".
