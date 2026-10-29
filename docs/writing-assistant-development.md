# Writing assistant: developer notes

> **Status:** implemented and checked end to end with a real, signed-in Claude Code account (Claude Code 2.1.310, Windows 11): connection check, a rewrite, word-by-word review, apply, undo and redo, through the desktop app (control channel), the headless MCP path and the client library. Repository gates: `cargo xtask ci` passed on 2026-10-23 (fmt, clippy, 3,981 tests, assets, layers, wasm). See [writing-assistant.md](writing-assistant.md)
> for the user view. Follow-up review fixes passed `cargo xtask ci` on 2026-10-24 (3,984 tests; 8 ignored). Revision history is at the end.
> **2026-10-26:** Markdown suggestions, structure descriptions, the warning that replaces the
> consent checkbox, lenient whole-document scope and hidden console windows landed with offline
> tests; `cargo xtask ci` passed on Windows on 2026-10-26 (8 gates, 4,610 tests, 8 ignored). No
> live-account check was run.

> **Upstream integration, 2026-10-24:** merged `upstream/main` at `254351c0` (79 commits), preserving
> the writing assistant. Final `cargo xtask ci`: all 8 gates passed, 4,081 tests passed, 0 failed,
> 8 ignored, with external catalogs required and embedded. An English-only `cargo check -p
> wordcraft-ui-egui --locked` also passed. No live requests were made for this integration.

## Rules this code follows

- Only the official `claude` CLI runs, with the user's account. No API-key path, no HTTP client,
  no other provider, no shell. Don't add a fallback; if the CLI or its login is missing, report it.
- Authentication: the CLI's stored session first, then the official OAuth token from the
  environment. Never an API key.
- Credentials are never read, copied, stored or logged by WordCraft. The child process loses only
  the override variables (`ANTHROPIC_*`, `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX`,
  `CLAUDE_CODE_USE_FOUNDRY`, `AWS_BEARER_TOKEN_BEDROCK`); the parent environment is untouched.
- Document text is data. It reaches the CLI only on stdin, as JSON, and the system prompt says so.
- Nothing is sent before the consent rules pass (in the panel: a warning, then a second send),
  and nothing changes the document except `assistant.apply` and `assistant.resolve` through the engine.
- Never crash: no `unwrap`, `expect` or panics in these crates (see [AGENTS.md](../AGENTS.md)).

## Layers and files

`crates/assistant` is layer **L4** in `xtask/src/layers.rs`, beside `engine`. It depends on no
workspace crate (only `serde`, `serde_json`, `thiserror`). The MCP server (L5) and the UI (L6)
depend on it; the engine does not, so `assistant.context`/`assistant.apply` never reach Claude.

| File | Job |
|---|---|
| `crates/assistant/src/lib.rs` | Public API: `Client`, `Request`, `Response`, `Suggestion`, `AuthState`, `Error`, size limits |
| `crates/assistant/src/client.rs` | `Client::discover` (absolute `PATH` entries only), `auth`, `generate` |
| `crates/assistant/src/process.rs` | `std::process::Command` only (no shell): scratch working directory, stdin, bounded stdout, deadline, cancel, kill-and-reap. Native targets only |
| `crates/assistant/src/protocol.rs` | Request validation, system prompt, JSON schema, parsing of the auth status and the output envelope |
| `crates/assistant/src/tests.rs` | Unit tests for the above |
| `crates/engine/src/assistant.rs` | `context` (scopes, consent, bounds), `apply` (validate, one undoable edit, returns the new context) |
| `crates/engine/src/cmd/assistant.rs` | Registers `assistant.context` and `assistant.apply` |
| `crates/engine/src/tests_assistant.rs` | Scopes, consent, stale ids, bounds, undo |
| `crates/ui-egui/src/assistant.rs` | Window, `ui.assistant`, `assistant.*` UI commands, worker thread, history, accept |
| `crates/ui-egui/src/ribbon.rs`, `canvas.rs`, `keys.rs`, `widgets.rs` | Review ribbon button (`Mirai\nAssistant`), context menu item, Ctrl/Cmd+Alt+Shift+A and its shortcut text |
| `crates/ui-egui/src/lib.rs`, `control.rs` | `WordApp::assistant` state, dispatch, per-frame `poll`, Escape and refocus rules; control-channel command list |
| `crates/ui-egui/src/assistant-i18n/*.tsv` | Assistant-only translation overlays, preserved from the fork's original catalogs (drafts pending native review) |
| `crates/ui-egui/build.rs`, `src/i18n/assistant_catalog.rs` | Merge the overlays into the external `craft-i18n` app catalogs at build time |
| `crates/engine/src/lib.rs`, `cmd/mod.rs` | Module and command registration |
| `crates/mcp/src/assistant.rs` | Headless `assistant.suggest`: synchronous, local, no window |
| `crates/mcp/src/tools.rs`, `lib.rs` | `assistant_context`, `assistant_suggest`, `assistant_apply` tools and their dispatch |
| `Cargo.toml`, `crates/*/Cargo.toml`, `xtask/src/layers.rs` | Workspace member, dependencies, layer L4 |

## Providers and models

`Provider` (`client.rs`) is the backend interface: `auth` and `generate`. `Client`, the official
Claude Code CLI, is the only implementation. Another backend must keep every rule above (the
user's own sign-in, no keys stored by WordCraft, document text as data, bounded and cancellable)
and needs the owner's approval first; `AGENTS.md` records the current directive.

Future Claude models need no code change: `Client::with_model` passes an alias or id as
`--model`. `protocol::model_name` (public as `wordcraft_assistant::model_name`) checks it first:
1 to 64 bytes of ASCII letters, digits and `._-[]`, starting with a letter or digit, so it can't
be read as a flag. The UI has a **Model** field (default from `WORDCRAFT_ASSISTANT_MODEL`), and
`assistant.suggest` takes `model` in the UI, the control channel and MCP.

## The CLI call

Generation runs `claude` with these flags (`process.rs`, `GENERATION_ARGS`):

- `--safe-mode`, `-p`, `--output-format json`, `--json-schema` (the output schema)
- `--system-prompt` (fixed text in `protocol.rs`)
- `--tools ""` and `--disallowedTools mcp__*`, `--strict-mcp-config --mcp-config {"mcpServers":{}}`:
  no tools and no MCP servers
- `--no-session-persistence`: no session files
- `--permission-mode dontAsk --permission-prompts none`: no prompts can appear
- On Windows the process is created with `CREATE_NO_WINDOW` (`process::run_claude`): `claude.exe`
  is a console program and the release app has no console, so without it every check and request
  flashed a console window

The request payload goes to stdin. The flags are fixed apart from an optional, checked `--model`;
without it the CLI's default model is used. `--bare` is not used because it would not use the account login.

The panel and the headless MCP path call `Client::generate_with_auth`, which checks the sign-in
and generates (two processes per request); they no longer run a separate `auth` first.

Auth is checked with `claude --safe-mode auth status --json`. Only `authMethod` `claude.ai` (stored
session probe) or `oauth_token` (environment token probe) with `loggedIn: true` is accepted, and
`apiProvider`, when present, must be `firstParty`: Bedrock, Vertex, Foundry or a gateway are
refused as `UnsupportedAuth` (`protocol::parse_auth_status`). The field names were checked against
the output of Claude Code 2.1.310 (field names and method only; no account data was recorded), and
every flag above appears in that version's `--help`. Generation re-checks
auth just before it runs, so a sign-out in between is caught.

Limits (constants in `protocol.rs`, the engine's in `engine/src/assistant.rs`):

| Limit | Value |
|---|---|
| Total text per request (instruction, targets, document context) | 65,536 bytes |
| Instruction | 4,096 bytes (UI and MCP) |
| Targets | 256 paragraphs |
| Output | 1 MiB; explanation 8,192 bytes |
| Auth timeout / generation timeout | 30 s / 120 s |

Output is validated before use: envelope not an error, `structured_output` present, indices
unique and in range, no control characters except tab (and newlines in the explanation and in
Markdown suggestions), no object-replacement characters.

Protocol additions: `Request::structure` (one description per target, at most
`MAX_STRUCTURE_BYTES` = 64 bytes each, sent as `structure` and omitted when empty) and
`Suggestion::markdown` (optional `markdown` boolean in the output schema). The system prompt tells
Claude to prefer plain suggestions for wording, to use Markdown for formatting and structure (empty
Markdown removes a target, merging puts the text in one target and empties the others), and to
answer questions in the explanation without suggestions.

## Engine: context and apply

`assistant.context {scope, confirm_document?}` builds a snapshot: `document_id`, `revision`, the
effective `scope` and `targets` (`start`, `end`, `original`). `auto` resolves to `selection` when
text is selected, or `document` when there is no text selection. Column and table-cell selections
are refused. Paragraphs with object or field
characters are refused.

Consent: `confirm_document: true` is required whenever the chosen scope covers every body
paragraph in full (`document`, `auto` without a selection, a one-section `section`, or any other
scope that covers the whole body). No whole body is ever sent implicitly.

`document` and `section` leave out empty paragraphs and paragraphs `selection`/`paragraph` would
refuse (objects, fields, tracked deletions, hidden text) through `Collector::add_rewritable`; if
nothing is left, the caret's paragraph is the one target. Every target carries `structure`
(`paragraph`, `heading 2`, `bulleted list item, level 1`, `…, in a table cell`) from
`wordcraft_formats::model::para_kind`.

`assistant.apply {context, changes}` validates everything before changing anything:

- `document_id` and `revision` must match the session, else the caller must read the context again.
- Each change: index in range, used once, one paragraph (no line breaks or objects), and the
  current text of the target must equal its `original`.
- There can be only one target per paragraph. Targets already marked `applied` cannot be changed
  again; their original text is not rechecked because tracked deletions remain in the paragraph.
- Each change is reduced to its differing middle (common prefix and suffix are kept).
- Changes apply last-paragraph-first, as one undoable step through `Session::run`.
- A change with `markdown: true` replaces its whole target range: the Markdown is imported
  (`paste::markdown_fragment`), plain paragraphs take the target's character formatting under the
  Markdown's emphasis (and the paragraph formatting of a plain body paragraph), and the fragment
  goes in through `paste::insert` (so Track Changes marks it). A whole-paragraph target then takes
  the first block's paragraph properties (heading, list, or plain again). Empty Markdown removes
  the target: joined to the paragraph before it, else `remove_block` (never a paragraph holding a
  section break). Blocks added or removed are counted per container, and the returned context
  moves later targets' paths by that count (`Shift`). Markdown targets are marked `applied`;
  applied targets are left out of the one-target-per-paragraph check.

On success it returns an updated context: the new `revision` and new target `original` texts and
`end` offsets, so the UI can apply the pending targets of a proposal one after another. With
Track Changes on, the changed targets are marked `applied` instead of having their text and offsets
updated, because their paragraphs retain tracked deletions. Those targets cannot be applied again;
the remaining targets can still be applied with the refreshed context.
Reading a new context for a paragraph with tracked deletions is still refused to avoid sending
hidden text. Accept or reject those revisions before regenerating over that paragraph.

Suggestion mode: `assistant.apply {..., suggest: true}` turns Track Changes on and sets the author
to `SUGGESTION_AUTHOR` ("Mirai") for the apply only, then restores both. Before each change
`fresh_revisions` pushes new Insert/Delete/Format revisions, because `new_revision` reuses the
latest matching one with the same author and date; the result's `revisions: [{index, revisions}]`
lists each change's own ids. While tracking, a plain change deletes first and inserts after the
tracked deletion, so it reads old-then-new; a whole Markdown target's paragraph-props change gets
a tracked `fmt_change` against the destination's props, so rejecting restores the style.
`assistant.resolve {revisions, accept}` resolves only runs, paragraph marks and paragraph
formatting carrying those ids (`review::resolve_revisions`), as one undo step, and returns
`{resolved}` (paragraphs touched; 0 when they are gone).

Known limits of the process handling (from a review of `process.rs` and `client.rs`):

- Timeout and cancel kill the `claude` process itself, not processes it may have started. With
  tools and MCP servers off it starts none for these requests; killing a whole process tree would
  need platform calls that `unsafe_code = "forbid"` rules out.
- On Windows only `claude.exe` is accepted: a `.cmd`/`.bat` shim runs through `cmd.exe`, which
  can't take the JSON arguments safely (`Client::with_executable` refuses it).
- The child inherits the user's environment apart from the removed credential overrides.
  Claude Code needs its proxy, certificate and configuration variables, and it is the user's own
  official binary.

## UI: the panel and the worker

- `State` is per window and not part of `ui.json`. Its `Drop` cancels a running request.
- A request runs on a `writing-assistant` thread. `Client::discover`, `auth`, `generate` and the
  result go back through an `mpsc` channel. `assistant::poll` (called each frame, also when the
  window is closed) settles the result on the UI thread. Cancel sets an `AtomicBool`; a result
  that arrives after cancel is discarded and says so.
- Context is taken on the UI thread through `assistant.context` before the thread starts. Whole
  document context is a second `assistant.context` call with `document` scope.
- No consent checkbox: when `sends_whole_document` holds (scope `document`, `auto` without a text
  selection, `section` in a one-section document, or whole-document context), the first send sets
  `State::warned` and shows the warning instead of sending; the next send passes
  `confirm_document: true` and clears it. Changing the scope or New conversation clears it.
  Errors from start, poll and apply are shown in the panel.
- `scope_bar` above the composer switches Automatic, Selection and Whole document and says what
  the next request sends. `assistant.accept` passes each suggestion's `markdown` flag to
  `assistant.apply`; the review labels Markdown as *Suggested (formatted)* or *Suggested: remove
  this paragraph*.
- History: `VecDeque` of proposals, newest first, trimmed to 8 entries and 1 MiB of estimated size.
  Nothing is persisted.
- Regenerate keeps the user's original instruction and explicit explain-only mode; the explanation
  prefix and interface-language instruction are added afresh, not accumulated on each retry.
- A proposal records the context it was made from. Apply and Undo check its `document_id` and
  `revision` against the session; a stale proposal shows a regenerate message and no Apply.
- `ui.assistant {value?}` toggles the resizable, right-docked panel, initially 280 points wide.
  Opening it does not authenticate or send. Escape closes it (and cancels a running request)
  unless a popup takes the Escape.
- Layout, top to bottom: Mirai header/status/New conversation, the copy/privacy note, optional
  connection error, then a scrollable conversation with a left-aligned welcome and three chips
  that fill/focus the draft without sending. The compact composer is pinned to the bottom; consent
  stays explicit and visible, while scope/model/authentication and secondary actions are under
  **Writing options**. Proposal diffs and Apply/Undo are under **Review changes**.
- Enter sends; Shift+Enter inserts a line; Ctrl/Cmd+Enter remains a send shortcut. egui IME
  preedit/commit events suppress submission during composition. New conversation clears only local
  transcript/draft/consent/error state, not auth or the document's native undo stack.
- Colours are the `assistant_*` tokens in `theme.rs`. Dark values follow Mirai's `global.css`
  (`#191622` background, `#201b2d` surface, `#2a2438` raised surface, `#988bc7` purple,
  `#ff79c6` pink, `#e96379` red); light values remain a coherent light counterpart and do not
  force the app's global appearance. The wordmark capsule intentionally stays dark cyan (`#12383f`)
  in both themes, with `#d7c8ff` italic Inter lettering; no new font files are bundled.
- The review diff is word by word (`word_diff`): shared tokens at both ends, then a longest common
  subsequence of the middle, capped at 250,000 token pairs (one removed and one added span past
  that).
- Outside English, the instruction asks for the explanation in the interface language.
- Strings are `tl!`. Upstream's interface catalogs now come from `storytold/craft-i18n`, pinned in
  `craft-i18n.rev` and embedded with `CRAFT_I18N_DIR`. The fork keeps only its 42 added rows per
  language in `crates/ui-egui/src/assistant-i18n/*.tsv` (26 languages), not copies of the full catalogs.
  `build.rs` merges these over the corresponding external app catalog by context and source key,
  preserving unrelated upstream rows and avoiding duplicate keys. Without `CRAFT_I18N_DIR`, the
  build stays English-only, as upstream intends. No external catalog checkout is modified.
  The shared labels **Generate**, **Discard** and **Original** use the `assistant` translation
  context, so preserving their wording here does not change those labels in other dialogs.
  Command labels in `commands()` are plain English metadata, not translated.

## Commands and MCP

| Command | Where | Behaviour |
|---|---|---|
| `ui.assistant` | UI | Opens, closes or toggles the right panel |
| `assistant.auth` | UI | Starts an auth check on a worker thread; returns `{started, poll}` |
| `assistant.suggest` | UI | Requires `confirm_document: true` where the scope needs it; returns `{started, poll}` |
| `assistant.status` | UI | `{busy, connection, message, proposals}` with proposal ids |
| `assistant.cancel` | UI | Cancels the running worker |
| `assistant.accept` | UI | `{id, index?}`: applies one or all pending suggestions of a proposal |
| `assistant.resolveSuggestion` | UI | `{id, index?, accept}`: accepts or rejects one or every pending suggestion shown in the document |
| `assistant.retry` | UI | `{id}`: rejects the pending suggestions, discards the proposal and asks again |
| `assistant.discard` | UI | `{id}` |
| `assistant.context` | engine | Read-only snapshot |
| `assistant.apply` | engine | Validated, undoable apply; returns the updated context; `suggest: true` writes tracked changes by Mirai |
| `assistant.resolve` | engine | `{revisions, accept}`: resolves only those revision ids |

MCP: the headless server answers `assistant.suggest` synchronously through
`assistant::generate_locally` and returns `{context, suggestions, explanation}` without changing
the document. The connected (`--connect`) backend starts a job in the app and the caller polls
`assistant.status`. `assistant_apply` takes the context and `changes` from either.
Unchanged suggestions are removed before the headless response is returned, as in the UI, so an
unchanged target does not cause the whole apply batch to be refused.

## Tests and gates

- Unit tests: the crate (`tests.rs`: process limits, cancellation, auth precedence, auth-status
  parsing including `apiProvider`, output validation, model names, Windows `.exe` rule), the engine
  (`tests_assistant.rs`: scopes, consent, stale checks, bounds, hidden text, protection, undo), the
  UI (`assistant.rs`: simulated proposal, apply and native undo, successive applications, stale and
  discarded proposals, AccessKit labels, shortcut and Escape, model refusal, consent, word diff) and
  MCP (parameter rules, fake provider, apply/undo/redo, DOCX round trip).
- `hostile_params_never_panic` in `crates/engine/src/tests.rs` runs every command, including
  `assistant.context` and `assistant.apply`, with junk params.
- Opt-in live tests, one tiny synthetic request each with the signed-in account:
  `WORDCRAFT_ASSISTANT_LIVE=1 cargo test -p wordcraft-assistant live_generation -- --ignored` and
  `WORDCRAFT_ASSISTANT_LIVE=1 cargo test -p wordcraft-mcp --lib live_suggest -- --ignored`
  (`WORDCRAFT_ASSISTANT_MODEL` picks a model). Both passed on 2026-10-23 with the default model and
  with `haiku`.
- The desktop app was driven through the control channel with an isolated
  `WORDCRAFT_PROFILE_DIR`: `ui.assistant`, `assistant.suggest`, polling `assistant.status` (about
  6 s to the proposal), `assistant.accept`, `edit.undo`, with screenshots of the application window showing the panel.
- Headless window screenshot: `WORDCRAFT_ASSISTANT_SCREENSHOT=<png> cargo test -p
  wordcraft-ui-egui --lib assistant_review_screenshot -- --ignored`.
- Gates: `cargo xtask ci` passed on 2026-10-23 (fmt, clippy, 3,981 tests, assets, layers, wasm).
- Follow-up gates: `cargo xtask ci` passed on Windows on 2026-10-24 after integrating `e6ee258e`:
  fmt, clippy, 3,984 tests passed (8 ignored), assets, layers and wasm. No live requests were run.
- Upstream merge gates: final `cargo xtask ci` passed on Windows on 2026-10-24 with
  `CRAFT_I18N_DIR` pointing to `craft-i18n.rev` and `CRAFT_I18N_REQUIRED=1`: fmt, clippy,
  4,081 tests (8 ignored), i18n-report, doctest, assets, layers and wasm. The tests include the
  overlay merge regressions, context isolation and embedding of the assistant-specific labels.
  A separate `cargo check -p wordcraft-ui-egui --locked` without either catalog variable passed.
- Tests that render through wgpu (`Harness::render`) hold `crate::gpu_test_lock()`. Before it,
  `cargo test -p wordcraft-ui-egui` crashed now and then on Windows (access violation in
  `d3d10warp.dll`, the WARP software rasterizer) when two such tests drew at once; this predates
  the assistant (seen at `3a4770ef`) and six parallel runs passed after the lock.
- The wasm gate needs `rustup target add wasm32-unknown-unknown`.
- `.github/workflows/assistant.yml` adds offline client tests on Windows, macOS and Linux for
  assistant-related pull requests and manual dispatch. It never runs ignored live tests or installs
  Claude Code. The workflow has not yet been run; it does not replace desktop or screen-reader checks.
- Not done: a screen-reader pass on each desktop platform, and runs on macOS and Linux.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-26 | minor | Suggestion mode: `poll` shows each new proposal in the document (`show_in_document`, `assistant.apply` with `suggest`), `Preview` per suggestion, `assistant.resolve`/`resolveSuggestion`/`retry`, deletion-before-insertion for tracked plain changes, tracked paragraph formatting for Markdown. CI result in the commit |
| 2026-10-26 | minor | Markdown suggestions (`Suggestion::markdown`, engine apply with block shifts and removal), `Request::structure`, lenient document/section scopes, warning instead of the consent checkbox, visible scope switch, `CREATE_NO_WINDOW` on Windows and one auth check per request. `cargo xtask ci` passed on Windows (8 gates, 4,610 tests, 8 ignored); no live-account check |
| 2026-10-25 | minor | Reworked the UI into a docked, resizable 280 pt right panel. Mirai palette tokens, conditional error card, draft-filling welcome chips, fixed compact composer, Enter/Shift+Enter with IME protection, and UI-only New conversation; review/apply/undo and backend/auth contracts retained. Focused tests and native `cargo check` passed; headless dark screenshot reviewed |
| 2026-10-23 | major | First version: draft implementation. Not yet compiled in the repository gates, not run against a live account |
| 2026-10-23 | minor | `assistant` moved to L4; `Provider` trait and `--model`; live validation; process limits documented; `apiProvider` check; word diff; tests and gates updated |
| 2026-10-24 | minor | Integrated review fixes: successive tracked applications, unchanged MCP suggestions omitted, and regeneration preserves the original instruction and explain-only mode. Windows CI passed all six gates (3,984 tests; 8 ignored); added an unrun offline client workflow for Windows/macOS/Linux and clarified the tracked-deletion regeneration limit |
| 2026-10-24 | minor | Merged upstream/main `254351c0`: adapted translations to the external-catalog architecture with 26 assistant-only overlays, isolated shared labels with a translation context and added regression tests. Final Windows CI passed all 8 gates (4,081 tests; 8 ignored), and an English-only build check passed; no new live-account checks |
| 2026-10-24 | minor | Renamed to Mirai Assistant (window, ribbon, context menu, command labels, 26 overlays; 11 languages' context/apply labels hand-written, pending native review). Larger draft (40%/24% of the window), quick-action chips above it, Ctrl/Cmd+Enter to send, palette and radii from the owner's design system; wasm-only unused-import and dead-variant warnings fixed. `cargo xtask ci` passed (8 gates); headless light and dark screenshots reviewed; no live-account checks |
