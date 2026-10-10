# Tauri → Wails (Go) migration

The desktop shell moved from Tauri (Rust) to Wails v2 (Go) in October 2026.
The React frontend stayed; the Rust backend that lived in `src-tauri/` was
rewritten in Go and removed. This document records the conventions the Go
backend follows.

## Principles

1. **Rewrite behaviour, not code.** Read the original module to learn what it does —
   inputs, outputs, side effects, failure modes, limits — then write the Go that
   a good Go engineer would write for that job. Do not mirror Rust's file split,
   type hierarchy or helper names. Prefer the standard library, small concrete
   types, early returns, `context.Context`, and errors wrapped with `%w`.
2. **Keep every external contract.** These must stay byte-compatible because users
   already have data and the frontend already speaks them:
   - IPC command names and argument keys (see below) and the JSON shape of results.
   - The SQLite schema (`internal/store/migrations`, never edit existing files),
     table semantics, and JSON stored in columns.
   - The data directory layout under `~/ImageClient` (or `IMAGE_CLIENT_DATA_DIR`):
     `assets/`, `logs/`, `promptlib-images/`, `image-client.db`, `backend-config.json`,
     and the folder/file naming of produced assets.
   - The local REST API on `127.0.0.1:8123` (routes, JSON, status codes).
   - User-facing error messages (Chinese). The frontend shows them and tests match them.
3. **Only port live code.** Tauri registered 60 commands; the frontend calls 47.
   `comic.rs`, `comic_visual*.rs`, `novel_adaptation.rs` and most of `novel.rs` are
   retired (unregistered). Port only what a registered command, the startup
   sequence, or the REST API reaches. When in doubt, check reachability.
4. **Moderate granularity.** One package per domain, a handful of files each. No
   package per Rust file, no interface without two implementations or a test need.

## Layout

| Package | Owns |
|---|---|
| `main.go` | Wails bootstrap |
| `internal/ipc` | Command router (`Router`, `Typed`, `Void`, `Input[T]`), `Emitter` |
| `internal/app` | Wiring, lifecycle, system commands (app info, dialogs, opener, config, db, history ack), local file server |
| `internal/platform` | Paths, logging (slog → JSONL), fs helpers, HTTP (`ClientFor`, `Do` + `Timeouts`, `ReadAllLimited`, `Download` with retry/resume), proxy |
| `internal/config` | API settings, `SaveRequest` validate/merge, model catalogues, `/v1/models` endpoints |
| `internal/store` | `DB` (zombiezen SQLite pool; `With`, `Tx`), migrations + pre-migration backup, frontend SQL channel, `PersistAssets`, `HistoryFeed`, `AssetRef` |
| `internal/media` | Asset files: save/detect/decode limits, image inspect/compress/convert, imports, text & document versions, promptlib image cache, video file helpers, media hosting |
| `internal/gen` | Image generation (gateway), video generation, LLM chat & agent, model listing, providers |
| `internal/comic` | Comic Markdown workspace (`comic_md_*`), startup recovery, legacy retirement |
| `internal/novel` | Novel works (`novel_*`) |
| `internal/restapi` | Local REST API server |

Domain packages expose `New(Deps) *Service` and `(*Service).Register(*ipc.Router)`.
They never import `internal/app` or the Wails runtime; events go through
`ipc.Emitter`.

## IPC contract

The frontend calls `invoke(command, args)`. The Go side receives the args object
as JSON. Tauri's rules, which we keep:

- Argument keys are the camelCase form of the Rust parameter names
  (`bind_values` → `bindValues`, `progress_id` → `progressId`). Many commands take a
  single object under `input` (use `ipc.Input[T]`) or `request` / `req` / `config`.
- Results are the serde JSON of the Rust return type. Check each struct's
  `#[serde(rename_all = "camelCase")]`, `skip_serializing_if`, `Option` (→ `null`
  unless skipped), and enums. In Go: use explicit `json` tags; `omitempty` only
  where Rust skips; **never return a nil slice where Rust returns `Vec`** (that is
  `null` instead of `[]`); `Option<T>` that is not skipped → pointer without
  `omitempty`.
- `Result::Err(String)` reaches the frontend as the message string. Return Go
  errors whose `Error()` is the same text.
- Commands that return `()` return `null` (`ipc.Void`).

## Conventions

- Logging: `log/slog` with the Rust event name as the message and camelCase keys:
  `slog.Info("image.request.start", "requestId", id, "model", m)`. Use
  `platform.SafeURL`, `platform.ErrorText`, `platform.TextStats` for URLs, errors and
  user text. Keys named prompt/text/content/... are redacted automatically.
- HTTP: `platform.ClientFor(url)` (loopback bypasses proxies) or
  `platform.SharedClient()`; send with `platform.Do(ctx, client, req, Timeouts{...})`;
  read bodies with `platform.ReadAllLimited`; GET downloads with `platform.Download`.
- SQLite: `db.With(ctx, func(conn *sqlite.Conn) error)` / `db.Tx(...)`;
  `conn.Prep` caches statements, `sqlitex.Execute(conn, q, &sqlitex.ExecOptions{Args, ResultFunc})`
  for one-offs. Times are Unix milliseconds unless the schema says otherwise.
- Files: `platform.WriteFileAtomic` for replace-in-place writes; assets are named
  `<uuid>.<ext>` unless the Rust code says otherwise.
- Tests: port the Rust tests that pin behaviour (contracts, limits, edge cases),
  table-driven where natural. Use `t.TempDir()`, `httptest.Server`, an in-temp-dir
  `store.Open`. No real network, no real user data.

## Working in this repo

- Before any `go` command: `source .test-tmp/goenv.sh` (Go caches live on D:, the
  system drive is full).
- Build/test only your package: `go build ./internal/<pkg>/... && go vet ./internal/<pkg>/... && go test ./internal/<pkg>/...`.
- Do not run `go mod tidy` or edit `go.mod`; if you need a module that is not
  already required, say so in your report.
- Do not edit files outside your package unless your task says so.
- The Rust sources are in git history (before the removal commit) if a behaviour needs checking.
