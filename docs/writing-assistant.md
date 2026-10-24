# Writing assistant

> **Status:** implemented and checked end to end with a real, signed-in Claude Code account (Claude Code 2.1.310, Windows 11): connection check, a rewrite, word-by-word review, apply, undo and redo, through the desktop app (control channel), the headless MCP path and the client library. Repository gates: `cargo xtask ci` passed on 2026-10-23 (fmt, clippy, 3,981 tests, assets, layers, wasm). Still open: a screen-reader
> pass, and runs on macOS and Linux. Gap 25 in [gaps.md](gaps.md) tracks this. Revision history is
> at the end. **Landed (2026-10-24):** the window layout and model choice described below are
> in place. `cargo xtask ci` passed (8 gates); headless light and dark screenshots reviewed; no live-account check.

The writing assistant drafts, rewrites, translates and summarises text inside WordCraft. It uses
the **official Claude Code CLI** (`claude`) and your own account. WordCraft has no API keys, no
HTTP calls and no other providers, and it never reads, copies or stores your credentials.

## Before you start

1. Install [Claude Code](https://code.claude.com) so that `claude` is on your `PATH`.
2. Sign in with Claude Code's own flow, for example `claude auth login` in a terminal.
3. In WordCraft, open **Review › Writing Assistant** and choose **Check connection**.

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

- **Review › Writing Assistant** in the ribbon.
- **Writing Assistant** in the right-click menu.
- **Ctrl+Alt+Shift+A** on Windows and Linux, **Cmd+Option+Shift+A** on macOS.

Opening the window sends nothing and starts no process. **Escape** closes it (and cancels a
running request). It opens beside the page, on the right, so the text under review stays visible.

## The window

- **Conversation:** your requests and Claude's proposals appear in order, from top to bottom. A
  proposal shows its suggestions for review. This is a log of what you asked and what came back;
  it is not a chat.
- **Composer:** a fixed box at the bottom of the window, five lines tall, holds the instruction
  and the Send button. It stays in place while the conversation scrolls.
- **Welcome:** an empty window shows a centred WordCraft welcome with quick-action chips. Clicking
  a chip requests that action on the selected scope; the same consent checks apply.
- **Colours:** a lavender and lilac palette in both the light and the dark theme.
- **Writing options:** scope, a custom model field and the whole-document context switch are folded
  away so the conversation has room. The model drop-down stays inside the composer. The consent checkbox for whole-body
  requests stays next to the composer and is never folded away.

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
| Automatic | The selected text, or the paragraph with the caret when nothing is selected |
| Selection | The selected text |
| Paragraph | The paragraph with the caret |
| Section | The paragraphs of the body section that holds the caret |
| Whole document | Every body paragraph |

When the chosen scope covers every body paragraph in full (a one-paragraph document in Automatic,
a one-section document in Section, or Whole document), the request needs your approval. The
window shows an explicit consent checkbox for each request, and it is never remembered: after
each request it is cleared and must be ticked again. The whole body is never sent implicitly.
Adding **Include the whole document as context** needs the same approval.

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

Rewrites are per paragraph. The assistant cannot merge or split paragraphs, and it cannot edit
fields or objects. When it changes a paragraph, only the changed middle is replaced: text that
stays the same at the start and end keeps its formatting. Formatting inside the changed part is
not reproduced exactly when the original mixed several formats, so check rewritten text that
used bold, links or similar.

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
**Original**, **Suggested** and **Apply** labels reach the accessibility tree (AccessKit) and that
the shortcut opens the window and Escape closes it. This has not yet been checked with a screen
reader on any platform.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-23 | major | First version: draft implementation. Not yet compiled in the repository gates, not run against a live account |
| 2026-10-23 | minor | Validated with a live account (desktop app, MCP, library); model choice (`--model`); word-by-word diff; explanation in the interface language; Escape closes the window; request controls fold when a proposal arrives; Bedrock/Vertex/Foundry/gateway setups refused |
| 2026-10-24 | minor | Review fixes allow successive tracked applications; clarified that regeneration over tracked deletions requires accepting or rejecting those revisions first. No new live-account validation |
| 2026-10-24 | minor | Landed: chronological conversation with request and proposal entries; composer at the bottom (up to five lines, scroll capped at 160px); lavender/lilac palette in light and dark themes; centred welcome with quick chips; original bird icon (@mirasins, MIT OR Apache-2.0, in ATTRIBUTION) on the Review button, header and shortcut, not the app icon; Model drop-down (Default, Sonnet, Opus, Haiku) with a free-text alias or id under Writing options; authentication and options folded. Each request stays independent (no chat history sent, not multi-turn). `cargo xtask ci` passed (8 gates), release x64 static build produced, headless light and dark screenshots reviewed, synthetic capture-proposal test passed |
