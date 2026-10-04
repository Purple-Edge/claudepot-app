# The website — `web/`

Working notes, moved out of `AGENTS.md` on 2026-10-02. Schema changes
have their own always-loaded rule, `.claude/rules/db-migrations.md`:
never `drizzle-kit push` against production.

## Web (claudepot.com)

`web/` is a self-contained Next.js 15 app that ships
`https://claudepot.com`. Independent install (its own
`package.json` + `pnpm-lock.yaml`); not a workspace member of the
root Tauri app. Two surfaces in one app:

- `/` — **reader**: resource aggregator for one-man companies
  building with AI.
- `/app/*` — **product docs**: 15 routes (landing + why + install
  + 9 features + features index + changelog + download), MDX
  under `web/src/app/(reader)/app/`.

Stack: Next.js 15 + Drizzle/Neon + Auth.js v5 (GitHub + Google +
Resend magic-link) + Resend + boring-avatars. `editorial/` carries
the editorial spec read at runtime by the bot office (a separate
private repo).

Deploy: Vercel project `<vercel-org>/claudepot-com`, Root Directory
`web/`. CF DNS for the `claudepot.com` zone is unproxied A
records to `76.76.21.21`. Phase-1 plan and full migration log in
`dev-docs/archive/domain-realignment.md`.

CI: `.github/workflows/ci-web.yml` runs typecheck + tests on
`web/**` changes (no build — Vercel handles the build per push).

`pnpm test` here is `node scripts/run-tests.mjs`, which **discovers**
`tests/*.test.ts` rather than listing them. It used to be 23 filenames
chained with `&&`, so adding a test file did not add it to the suite —
and three never ran anywhere: `username.test.ts` (reserved names and
self-rename cooldown, the impersonation surface of a public site),
`editorial-routing.test.ts`, and `social-format.test.ts`. All three
passed once run, which is the bad case: 45 assertions looked like
coverage while CI was green without them. A list of files is a cache of
the directory; read the directory. `tests/integration/` stays out on
purpose — it needs `--env-file=.env.local` and a live Neon connection,
so it keeps its own `test:integration` script.

The `web/.tokenize/` config currently runs the hook in
`{"mode": "maintainer", "strictness": "advisory"}` — it flags
hardcoded values but does not block. Promote to strict only after
the residual hardcoded values in the imported codebase are absorbed,
and diff-scan TS/TSX after any `/ui-tokenize:fix` run (the hook has
corrupted non-CSS files before).
