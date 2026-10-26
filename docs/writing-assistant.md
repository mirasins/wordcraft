# Writing assistant

> **Status:** implemented and checked end to end with a real, signed-in Claude Code account (Claude Code 2.1.310, Windows 11): connection check, a rewrite, word-by-word review, apply, undo and redo, through the desktop app (control channel), the headless MCP path and the client library. Repository gates: `cargo xtask ci` passed on 2026-10-23 (fmt, clippy, 3,981 tests, assets, layers, wasm). Still open: a screen-reader
> pass, and runs on macOS and Linux. Gap 25 in [gaps.md](gaps.md) tracks this. Revision history is
> at the end. **Landed (2026-10-25):** the resizable 280 pt right panel, Mirai theme tokens, compact
> composer, Spanish prompts, conditional connection error and local New conversation action are in
> place. Focused assistant UI tests and `cargo check -p wordcraft-ui-egui --locked` passed; a dark
> headless screenshot is available under the OpenCode temp directory. No authentication/backend
> changes or live-account checks were made. **Landed (2026-10-26):** formatted and structural
> suggestions (Markdown), a visible scope switch, a skippable whole-document warning instead of the
> consent checkbox, and no console windows on Windows. Offline tests passed; no live-account check
> was run for these changes.

The writing assistant drafts, rewrites, translates and summarises text inside WordCraft. It uses
the **official Claude Code CLI** (`claude`) and your own account. WordCraft has no API keys, no
HTTP calls and no other providers, and it never reads, copies or stores your credentials.

## Before you start

1. Install [Claude Code](https://code.claude.com) so that `claude` is on your `PATH`.
2. Sign in with Claude Code's own flow, for example `claude auth login` in a terminal.
3. In WordCraft, open **Review › Mirai Assistant** and choose **Check connection**.

If you can't sign in through a browser, `claude setup-token` makes a long-lived token for your
subscription; set it as `CLAUDE_CODE_OAUTH_TOKEN` before starting WordCraft. WordCraft uses a
signed-in session first and that token second, and never an API key.

On Windows, WordCraft looks for `claude.exe` (the native installer puts it in
`%USERPROFILE%\.local\bin`). A `claude.cmd` from an npm install is not used.

WordCraft never signs you in or out by itself. To change account, use Claude Code's official
login and logout flow, then check the connection again. The
[official authentication instructions](https://code.claude.com/docs/en/authentication) cover this.
The desktop app is required; the web build reports the assistant as unavailable.

## Opening it

- **Review › Mirai Assistant** in the ribbon.
- **Mirai Assistant** in the right-click menu.
- **Ctrl+Alt+Shift+A** on Windows and Linux, **Cmd+Option+Shift+A** on macOS.

Opening the panel sends nothing and starts no process. Claude Code runs hidden: on Windows no
console window appears for the connection check or a request, and each request starts two
processes (a sign-in check and the request), not three. **Escape** closes it (and cancels a
running request). It docks to the right so the text under review stays visible.

## The panel

- **Layout:** a resizable panel docked to the right, initially 280 points wide. The document keeps
  the remaining central space. Its Mirai palette follows the selected light or dark theme.
- **Header:** a one-line **Mirai** wordmark in a dark-cyan capsule, a small connection status and a
  discreet **New conversation** action. The longer no-auto-apply explanation is on the wordmark's
  tooltip. A failed connection check shows an error card with **Check connection**; before the
  first check, the action remains available under **Writing options**.
- **Privacy note:** the assistant reads a copy of the open document and does not change it merely by
  answering. Rewrites remain proposals: applying one is always a separate, explicit action.
- **Welcome:** left-aligned with a pink sparkle and three starter chips. A chip fills and focuses the
  draft; it does not send text. Requests and proposals remain in the scrollable conversation.
- **Composer:** compact and pinned to the bottom. Above it, **Automatic**, **Selection** and
  **Whole document** choose what is sent, and a line says what the next request will send.
  **Enter** sends, **Shift+Enter** inserts a line; Ctrl/Cmd+Enter also sends. Enter during an IME
  composition is not submitted. A request that sends the whole document first shows a warning;
  Enter or Send again sends it.
- **Writing options:** scope, model selection/custom id, whole-document context, connection check,
  tone and translation actions are folded away. Proposal review, Apply, Undo and Regenerate are
  available under each proposal's **Review changes** disclosure.
- **New conversation:** clears local proposals, draft, consent and errors, but leaves the document,
  authentication and native document undo history untouched. It is disabled while a request runs.

**Each request is independent.** WordCraft does not send earlier requests, proposals or the
conversation as context, and the window is not a multi-turn chat. Each request carries only its
own instruction and the text its scope selects. Consent, the consent checkbox and Apply/Undo
rules below apply to every request separately.

## Choosing the model

The **Model** box is a drop-down with **Default** (Claude Code's default model), **Sonnet**,
**Opus** and **Haiku**. Each name is sent as its alias to Claude Code as `--model`. In **Writing options**
you can type another alias or a full model id in the free-text field instead. A new Claude model
works as soon as your Claude Code knows it, without a WordCraft update. To start with the same
model every time, set the `WORDCRAFT_ASSISTANT_MODEL` environment variable. Names are checked
first: at most 64 characters of letters, digits and `. _ - [ ]`.

## What it sends

Only the text you choose is sent, and only when you press Send message or a writing-action button.

| Scope | What is sent |
|---|---|
| Automatic | The selected text, or every body paragraph when nothing is selected |
| Selection | The selected text |
| Paragraph | The paragraph with the caret |
| Section | The paragraphs of the body section that holds the caret |
| Whole document | Every body paragraph |

When the chosen scope covers the whole body (Automatic without a text selection, a one-section
document in Section, or Whole document), or **Include the whole document as context** is on, the
first Enter or Send shows a warning instead of sending. Enter or Send again sends that request;
the next one warns again. Programmatic callers (control channel, MCP) still pass
`confirm_document: true`.

With Whole document and Section, empty paragraphs and paragraphs the assistant can't rewrite
(pictures, equations, fields, tracked deletions, hidden text) are left out instead of refusing the
request; their text is not sent. In an empty document the caret's paragraph is sent, so the
assistant can write into it. The limits below still apply: a very long document is refused, so
send a part of it.

Limits: an instruction is at most 4,096 bytes; a request holds at most 64 KiB of text (instruction,
targets and any document context together) and 256 paragraphs. Larger selections are refused, so
select less. Paragraphs that contain objects or fields (pictures, equations, fields) are refused
rather than sent without them. Refusals and errors appear in the window.

## Reviewing and applying

When a proposal arrives, it appears in the conversation above the composer. To ask again, type
in the composer below; the options stay folded under **Options** unless you open them. Each suggestion appears with its original and suggested text,
changed words marked: struck through in the original, underlined in the suggestion. The
explanation is written in the interface language. Then:

- **Apply** applies one suggestion. **Apply all** applies the rest of the proposal.
- After an application, the window takes the document's new state, so the remaining suggestions
  can be applied one after another, also with **Track Changes** on. If the document is changed
  by something else, the remaining suggestions are stale: **Regenerate** asks again.
- To regenerate over text with tracked deletions, first accept or reject those revisions. The
  assistant refuses to send hidden deleted text; the remaining suggestions of the existing proposal
  can still be applied.
- **Copy** copies the suggested text to the clipboard without changing the document.
- **Discard** removes the proposal from the window.
- **Undo last application** undoes the last application in one step, while the document has not
  changed since.

Apply works only while the proposal matches the document. If you edit the document, the buttons
are disabled and the window asks you to regenerate. Each application is one undo step.

A suggestion is plain or formatted:

- **Plain** (wording changes): only the changed middle is replaced, so text that stays the same at
  the start and end keeps its formatting. Formatting inside the changed part is not reproduced
  exactly when the original mixed several formats.
- **Formatted** (*Suggested (formatted)* in the review): Claude writes Markdown that replaces the
  whole target. It can make headings, bulleted and numbered lists, bold, italic, strikethrough,
  code, quotes and tables, split a paragraph into several, add paragraphs before or after it,
  merge paragraphs, and remove a paragraph (*Suggested: remove this paragraph*). New body text
  keeps the target's font and colour and, for body paragraphs, its alignment and spacing;
  headings, lists, quotes and code take their styles. The review shows the Markdown as written
  (`#`, `-`, `**`).

Claude sees what each paragraph is (paragraph, heading level, list item, quote, code, table cell),
so it can keep or change the structure. Questions about the document (for example *Summarize the
main ideas*) are answered in the explanation, with no suggestions. Fields and objects are never
edited. A formatted change is one undo step like any other application; with Track Changes the
inserted and deleted text is tracked, but a paragraph style change (for example to a heading) is
not.

## Cancelling and timeouts

**Cancel** stops the local Claude Code process. A request times out after 120 seconds, and the
connection check after 30. Text already sent to Claude cannot be retracted by cancelling.

## Privacy and history

- WordCraft writes nothing to disk for the assistant. Proposals stay in the window's memory: at
  most 8 proposals and 1 MiB, cleared when you close WordCraft.
- Claude Code keeps its own records under its own rules; WordCraft turns off its session saving
  for these requests.
- The model is whatever your Claude Code uses unless you choose one under **Options** or set
  `WORDCRAFT_ASSISTANT_MODEL`.
- Only a Claude account sign-in (stored session, or the `CLAUDE_CODE_OAUTH_TOKEN` made by
  `claude setup-token`) is accepted. If your Claude Code is set up for an API key, a key helper,
  Amazon Bedrock, Google Vertex, Microsoft Foundry or a gateway, the assistant refuses to run.
- WordCraft runs the `claude` it finds on your `PATH`. That is the trust boundary: an official
  binary you installed, not a sandbox. Claude Code applies its own managed policies on your
  computer, and WordCraft does not override them.

## Accessibility

Labels say what each control does, and proposal state is written as text, not only colour:
removed text is struck through and added text underlined. Automated tests check that the
**Original**, **Suggested** and **Apply** labels reach the accessibility tree (AccessKit), the
starter chips fill the draft without sending, and the assistant shortcut opens the panel and Escape
closes it. This has not yet been checked with a screen reader on any platform.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-23 | major | First version: draft implementation. Not yet compiled in the repository gates, not run against a live account |
| 2026-10-23 | minor | Validated with a live account (desktop app, MCP, library); model choice (`--model`); word-by-word diff; explanation in the interface language; Escape closes the window; request controls fold when a proposal arrives; Bedrock/Vertex/Foundry/gateway setups refused |
| 2026-10-24 | minor | Review fixes allow successive tracked applications; clarified that regeneration over tracked deletions requires accepting or rejecting those revisions first. No new live-account validation |
| 2026-10-24 | minor | Landed: chronological conversation with request and proposal entries; composer at the bottom (up to five lines, scroll capped at 160px); lavender/lilac palette in light and dark themes; centred welcome with quick chips; original bird icon (@mirasins, MIT OR Apache-2.0, in ATTRIBUTION) on the Review button, header and shortcut, not the app icon; Model drop-down (Default, Sonnet, Opus, Haiku) with a free-text alias or id under Writing options; authentication and options folded. Each request stays independent (no chat history sent, not multi-turn). `cargo xtask ci` passed (8 gates), release x64 static build produced, headless light and dark screenshots reviewed, synthetic capture-proposal test passed |
| 2026-10-25 | minor | Replaced the floating window with a resizable 280 pt right panel; Mirai palette, conditional connection-error card, left-aligned welcome, three draft-filling chips, compact pinned composer and Enter/Shift+Enter/IME handling. New conversation clears local UI state only; proposals retain review/apply/undo disclosures, whole-document consent and authentication/backend behavior are unchanged. Focused assistant tests (20 passed) and `cargo check -p wordcraft-ui-egui --locked` passed; dark headless screenshot reviewed |
| 2026-10-25 | trivial | Reduced header noise: one-line italic Mirai wordmark in a dedicated dark-cyan capsule with light-lavender text, compact unframed connection status and text-only New conversation. Light theme keeps the capsule dark for contrast |
| 2026-10-25 | minor | Automatic scope uses selected text, or the whole body when there is no text selection. Whole-body consent remains explicit; the panel's automatic-scope hint and send gate reflect this default. |
| 2026-10-26 | minor | Formatted and structural suggestions (Markdown: headings, lists, emphasis, tables, split, merge, add and remove paragraphs) with the paragraph structure sent to Claude; Automatic/Selection/Whole document switch above the composer; a whole-document warning skipped with a second Enter replaces the consent checkbox; whole-document and section scopes leave out paragraphs that can't be rewritten; Claude Code runs without console windows on Windows, with one sign-in check per request. Offline tests only |
