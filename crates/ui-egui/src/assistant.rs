//! Non-modal writing review. The provider never edits the document: accepted text goes through
//! `assistant.apply`, with the engine's stale-snapshot, protection and undo checks.
use std::collections::VecDeque;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{Receiver, TryRecvError},
};

use serde_json::{Value, json};
use wordcraft_assistant::{AuthState, Provider, Request, Response};

use crate::WordApp;

const HISTORY_LIMIT: usize = 8;
const HISTORY_BYTES: usize = 1024 * 1024;

enum Answer {
    Auth(Result<AuthState, String>),
    Suggestion(Result<(AuthState, Response), String>),
}

struct Pending {
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Answer>,
    context: Option<Value>,
    /// The user's own words, before the explain prefix and language line are added.
    instruction: String,
    explain_only: bool,
}

struct Proposal {
    id: u64,
    context: Value,
    /// Original display text is immutable even when the engine returns a refreshed snapshot.
    originals: Vec<String>,
    /// What Regenerate sends again: the user's instruction and whether it was an explain request.
    instruction: String,
    explain_only: bool,
    response: Response,
    applied: Vec<usize>,
    dismissed: bool,
    undo_revision: Option<u64>,
}

/// Ephemeral, per-window state. Deliberately not part of `UiState` / `ui.json`.
pub struct State {
    pub open: bool,
    scope: String,
    instruction: String,
    language: String,
    tone: String,
    /// Claude model alias or id; empty keeps Claude Code's default.
    model: String,
    include_document: bool,
    confirmed: bool,
    connection: Option<AuthState>,
    message: String,
    pending: Option<Pending>,
    history: VecDeque<Proposal>,
    next_id: u64,
    focus: bool,
    /// Fold the request controls on the next frame (a proposal just arrived).
    collapse_request: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            open: false,
            scope: "auto".into(),
            instruction: String::new(),
            language: String::new(),
            tone: String::new(),
            // A default for people who always want one model; typed per session otherwise.
            model: std::env::var("WORDCRAFT_ASSISTANT_MODEL").unwrap_or_default(),
            include_document: false,
            confirmed: false,
            connection: None,
            message: String::new(),
            pending: None,
            history: VecDeque::new(),
            next_id: 1,
            focus: false,
            collapse_request: false,
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(pending) = &self.pending {
            pending.cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// UI/control command metadata. Merely enumerating commands never starts Claude Code.
pub fn commands() -> Vec<Value> {
    [
        ("ui.assistant", "Writing Assistant", "value?: bool; omitted toggles the floating window"),
        ("assistant.auth", "Check Claude connection", "Checks the official Claude Code account/OAuth login; no document is sent"),
        ("assistant.suggest", "Request writing suggestions", "instruction: string; scope?: auto|selection|paragraph|section|document; confirm_document?: bool; include_document_context?: bool; explain_only?: bool; model?: string (Claude alias or id; omitted: Claude Code default). Sends the chosen text to Claude. Returns a job; poll assistant.status."),
        ("assistant.status", "Writing assistant status", "Returns busy, connection, message and local proposals"),
        ("assistant.cancel", "Cancel writing request", "Cancels the running local Claude Code process; cannot retract already transmitted text"),
        ("assistant.accept", "Apply writing suggestion", "id: proposal id; index?: target index (omitted applies all pending suggestions)"),
        ("assistant.discard", "Discard writing suggestion", "id: proposal id"),
    ].into_iter().map(|(id, label, params)| json!({"id":id,"label":label,"location":"Review › Writing Assistant",
        "shortcut":if id == "ui.assistant" { Some("Mod+Alt+Shift+A") } else { None },"params":params,"enabled":true})).collect()
}

fn boolean(params: &Value, name: &str) -> Result<bool, String> {
    params.get(name).map(|v| v.as_bool().ok_or_else(|| format!("`{name}` must be a boolean"))).transpose().map(|v| v.unwrap_or(false))
}

/// Same entry point for the window and trusted control/MCP callers; never opens login dialogs.
pub fn command(app: &mut WordApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    Some(match id {
        "ui.assistant" => {
            let on = match params.get("value") {
                None => !app.assistant.open,
                Some(Value::Bool(on)) => *on,
                Some(_) => return Some(Err("`value` must be a boolean".into())),
            };
            app.assistant.open = on;
            app.assistant.focus = on;
            if !on {
                cancel(&mut app.assistant);
            }
            Ok(json!({"open":on}))
        }
        "assistant.auth" => start_auth(app),
        "assistant.suggest" => start_suggestion(app, params),
        "assistant.status" => {
            poll(app);
            Ok(status(&app.assistant))
        }
        "assistant.cancel" => {
            cancel(&mut app.assistant);
            Ok(json!({"cancelling":app.assistant.pending.is_some()}))
        }
        "assistant.accept" => accept(app, params),
        "assistant.discard" => {
            let proposal = params.get("id").and_then(Value::as_u64).and_then(|id| app.assistant.history.iter_mut().find(|p| p.id == id));
            match proposal {
                Some(p) => {
                    p.dismissed = true;
                    Ok(json!({"discarded":p.id}))
                }
                None => Err("unknown proposal id".into()),
            }
        }
        _ => return None,
    })
}

fn cancel(state: &mut State) {
    if let Some(pending) = &state.pending {
        pending.cancel.store(true, Ordering::Relaxed);
        state.message = tl!("Cancelling…").into();
    }
}

fn status(state: &State) -> Value {
    json!({"busy":state.pending.is_some(),"connection":match state.connection {
            Some(AuthState::Account) => "claude_account",
            Some(AuthState::OAuthToken) => "oauth_token",
            None => "disconnected",
        },
        "message":state.message,"proposals":state.history.iter().map(|p| json!({"id":p.id,"context":p.context,
            "suggestions":p.response.suggestions,"explanation":p.response.explanation,"applied":p.applied,"discarded":p.dismissed})).collect::<Vec<_>>()})
}

#[cfg(target_arch = "wasm32")]
fn launch(
    _app: &mut WordApp,
    _context: Option<Value>,
    _request: Option<Request>,
    _instruction: String,
    _explain: bool,
    _model: Option<String>,
) -> Result<Value, String> {
    Err("The writing assistant requires the desktop app and official Claude Code".into())
}

#[cfg(not(target_arch = "wasm32"))]
fn launch(
    app: &mut WordApp,
    context: Option<Value>,
    request: Option<Request>,
    instruction: String,
    explain: bool,
    model: Option<String>,
) -> Result<Value, String> {
    if app.assistant.pending.is_some() {
        return Err("a writing request is already running".into());
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    let wake = app.ctx.clone();
    std::thread::Builder::new()
        .name("writing-assistant".into())
        .spawn(move || {
            let answer = match request {
                Some(request) => Answer::Suggestion((|| {
                    let client = wordcraft_assistant::Client::discover().and_then(|c| c.with_model(model.as_deref())).map_err(|e| e.to_string())?;
                    let auth = client.auth(&worker_cancel).map_err(|e| e.to_string())?;
                    let response = client.generate(&request, &worker_cancel).map_err(|e| e.to_string())?;
                    Ok((auth, response))
                })()),
                None => Answer::Auth((|| {
                    wordcraft_assistant::Client::discover().map_err(|e| e.to_string())?.auth(&worker_cancel).map_err(|e| e.to_string())
                })()),
            };
            let _ = sender.send(answer);
            if let Some(ctx) = wake {
                ctx.request_repaint();
            }
        })
        .map_err(|_| "could not start the writing worker")?;
    app.assistant.pending = Some(Pending { cancel, receiver, context, instruction, explain_only: explain });
    app.assistant.message = tl!("Waiting for Claude…").into();
    Ok(json!({"started":true,"poll":"assistant.status"}))
}

fn start_auth(app: &mut WordApp) -> Result<Value, String> {
    launch(app, None, None, String::new(), false, None)
}

fn start_suggestion(app: &mut WordApp, params: &Value) -> Result<Value, String> {
    let instruction = params
        .get("instruction")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty() && s.len() <= 4096)
        .ok_or("give a nonempty instruction of at most 4096 bytes")?
        .to_owned();
    let scope = params.get("scope").map(|s| s.as_str().ok_or("scope must be a string")).transpose()?.unwrap_or("auto");
    let confirmed = boolean(params, "confirm_document")?;
    let include_document = boolean(params, "include_document_context")?;
    let explain = boolean(params, "explain_only")?;
    let model = match params.get("model") {
        None => None,
        Some(Value::String(m)) => {
            wordcraft_assistant::model_name(Some(m)).map_err(|_| "model must be a Claude model alias or id such as sonnet or opus")?
        }
        Some(_) => return Err("model must be a string".into()),
    };
    if include_document && !confirmed {
        return Err("confirm_document: true is required to send the whole document as context".into());
    }
    if app.assistant.pending.is_some() {
        return Err("a writing request is already running".into());
    }
    let context = app.session.run("assistant.context", &json!({"scope":scope,"confirm_document":confirmed})).map_err(|e| e.to_string())?;
    let targets = context
        .get("targets")
        .and_then(Value::as_array)
        .ok_or("invalid assistant context")?
        .iter()
        .map(|t| t.get("original").and_then(Value::as_str).map(str::to_owned).ok_or("invalid assistant target"))
        .collect::<Result<Vec<_>, _>>()?;
    let document_context = if include_document {
        let full = app.session.run("assistant.context", &json!({"scope":"document","confirm_document":true})).map_err(|e| e.to_string())?;
        Some(
            full.get("targets")
                .and_then(Value::as_array)
                .ok_or("invalid document context")?
                .iter()
                .filter_map(|t| t.get("original").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    } else {
        None
    };
    let sent =
        if explain { format!("Explain or suggest improvements only. Return no replacement suggestions. {instruction}") } else { instruction.clone() };
    let sent = explain_in_interface_language(sent, crate::i18n::current());
    let request = Request { instruction: sent, targets, context: document_context };
    request.validate().map_err(|error| error.to_string())?;
    launch(app, Some(context), Some(request), instruction, explain, model)
}

/// Outside English, asks for the explanation in the interface language, so it reads like the rest
/// of the window. The rewrites keep the document's own language. Skipped when it wouldn't fit.
fn explain_in_interface_language(instruction: String, lang: crate::i18n::Lang) -> String {
    if lang.code() == crate::i18n::Lang::EN.code() {
        return instruction;
    }
    let with = format!("{instruction}\n\nWrite the explanation in {}.", lang.name());
    if with.len() <= wordcraft_assistant::MAX_INSTRUCTION_BYTES { with } else { instruction }
}

/// Settle results on the UI thread, even when the floating window is closed.
pub fn poll(app: &mut WordApp) {
    let Some(pending) = &app.assistant.pending else { return };
    let answer = match pending.receiver.try_recv() {
        Ok(answer) => answer,
        Err(TryRecvError::Empty) => return,
        Err(TryRecvError::Disconnected) => Answer::Auth(Err("the writing worker stopped".into())),
    };
    let Some(pending) = app.assistant.pending.take() else { return };
    if pending.cancel.load(Ordering::Relaxed) {
        app.assistant.message = tl!("Request cancelled. Text already sent cannot be retracted.").into();
        return;
    }
    match answer {
        Answer::Auth(Ok(auth)) => {
            app.assistant.connection = Some(auth);
            app.assistant.message.clear();
        }
        Answer::Suggestion(Ok((auth, mut response))) => {
            app.assistant.connection = Some(auth);
            app.assistant.message.clear();
            if pending.explain_only {
                response.suggestions.clear();
            }
            if let Some(context) = pending.context {
                let originals: Vec<String> = context
                    .get("targets")
                    .and_then(Value::as_array)
                    .map(|targets| targets.iter().filter_map(|t| t.get("original").and_then(Value::as_str).map(str::to_owned)).collect())
                    .unwrap_or_default();
                response.suggestions.retain(|suggestion| originals.get(suggestion.index).is_some_and(|original| *original != suggestion.text));
                let id = app.assistant.next_id;
                app.assistant.next_id = id.saturating_add(1);
                app.assistant.history.push_front(Proposal {
                    id,
                    context,
                    originals,
                    instruction: pending.instruction,
                    explain_only: pending.explain_only,
                    response,
                    applied: Vec::new(),
                    dismissed: false,
                    undo_revision: None,
                });
                app.assistant.collapse_request = true;
                trim_history(&mut app.assistant);
            }
        }
        Answer::Auth(Err(error)) | Answer::Suggestion(Err(error)) => {
            app.assistant.connection = None;
            app.assistant.message = error;
        }
    }
}

fn trim_history(state: &mut State) {
    while state.history.len() > HISTORY_LIMIT
        || state
            .history
            .iter()
            .map(|p| {
                p.context
                    .to_string()
                    .len()
                    .saturating_add(p.originals.iter().map(String::len).sum::<usize>())
                    .saturating_add(p.response.explanation.len())
                    .saturating_add(p.response.suggestions.iter().map(|s| s.text.len()).sum::<usize>())
            })
            .sum::<usize>()
            > HISTORY_BYTES
    {
        if state.history.pop_back().is_none() {
            break;
        }
    }
}

fn accept(app: &mut WordApp, params: &Value) -> Result<Value, String> {
    let id = params.get("id").and_then(Value::as_u64).ok_or("give a proposal id")?;
    let index =
        params.get("index").map(|v| v.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or("index must be a nonnegative integer")).transpose()?;
    let proposal = app.assistant.history.iter().find(|p| p.id == id && !p.dismissed).ok_or("unknown or discarded proposal")?;
    let changes: Vec<Value> = proposal
        .response
        .suggestions
        .iter()
        .filter(|s| !proposal.applied.contains(&s.index) && index.is_none_or(|i| s.index == i))
        .map(|s| json!({"index":s.index,"text":s.text}))
        .collect();
    if changes.is_empty() {
        return Err("no pending suggestion to apply".into());
    }
    let context = proposal.context.clone();
    let result = app.session.run("assistant.apply", &json!({"context":context,"changes":changes})).map_err(|e| e.to_string())?;
    let revision = app.session.rev();
    if let Some(p) = app.assistant.history.iter_mut().find(|p| p.id == id) {
        if let Some(updated) = result.get("context") {
            p.context = updated.clone();
        }
        for change in &changes {
            if let Some(i) = change.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()) {
                p.applied.push(i);
            }
        }
        p.undo_revision = Some(revision);
    }
    app.canvas.want_focus = false;
    Ok(result)
}

/// One piece of a word diff: text both versions share, text only the original has, or text only
/// the suggestion has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Same,
    Removed,
    Added,
}

/// Most token pairs the word diff compares (about 1 MB of table). A longer changed middle is shown
/// as one removed and one added span instead.
const DIFF_CELLS: usize = 250_000;

/// Words (runs of letters and digits), runs of spaces, and single other characters, so a diff
/// marks whole words and keeps punctuation separate. Always on UTF-8 boundaries.
fn diff_tokens(s: &str) -> Vec<&str> {
    let class = |c: char| {
        if c.is_alphanumeric() {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let mut out = Vec::new();
    let mut start = 0;
    let mut prev: Option<u8> = None;
    for (i, c) in s.char_indices() {
        let k = class(c);
        if prev.is_some_and(|p| p != k || k == 2) {
            out.extend(s.get(start..i));
            start = i;
        }
        prev = Some(k);
    }
    out.extend(s.get(start..).filter(|t| !t.is_empty()));
    out
}

/// Word-level diff of `original` and `suggested`: shared tokens at both ends, then a longest
/// common subsequence of the middle when it is small enough. Adjacent pieces of a kind are merged.
/// Same + Removed pieces give `original` back; Same + Added give `suggested`.
fn word_diff(original: &str, suggested: &str) -> Vec<(Part, String)> {
    let a = diff_tokens(original);
    let b = diff_tokens(suggested);
    let pre = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let (ra, rb) = (a.get(pre..).unwrap_or_default(), b.get(pre..).unwrap_or_default());
    let suf = ra.iter().rev().zip(rb.iter().rev()).take_while(|(x, y)| x == y).count();
    let ma = ra.get(..ra.len().saturating_sub(suf)).unwrap_or_default();
    let mb = rb.get(..rb.len().saturating_sub(suf)).unwrap_or_default();
    let mut ops: Vec<(Part, &str)> = a.get(..pre).unwrap_or_default().iter().map(|t| (Part::Same, *t)).collect();
    let (n, m) = (ma.len(), mb.len());
    if n.saturating_mul(m) <= DIFF_CELLS {
        // lcs[i * (m + 1) + j]: longest common subsequence of ma[i..] and mb[j..].
        let w = m + 1;
        let mut lcs = vec![0u32; (n + 1) * w];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                let v = if ma.get(i) == mb.get(j) {
                    lcs.get((i + 1) * w + j + 1).copied().unwrap_or(0).saturating_add(1)
                } else {
                    let down = lcs.get((i + 1) * w + j).copied().unwrap_or(0);
                    let right = lcs.get(i * w + j + 1).copied().unwrap_or(0);
                    down.max(right)
                };
                if let Some(cell) = lcs.get_mut(i * w + j) {
                    *cell = v;
                }
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            let (x, y) = (ma.get(i), mb.get(j));
            if x.is_some() && x == y {
                ops.extend(x.map(|t| (Part::Same, *t)));
                i += 1;
                j += 1;
            } else if j < m && (i == n || lcs.get(i * w + j + 1).copied().unwrap_or(0) >= lcs.get((i + 1) * w + j).copied().unwrap_or(0)) {
                ops.extend(y.map(|t| (Part::Added, *t)));
                j += 1;
            } else {
                ops.extend(x.map(|t| (Part::Removed, *t)));
                i += 1;
            }
        }
    } else {
        ops.extend(ma.iter().map(|t| (Part::Removed, *t)));
        ops.extend(mb.iter().map(|t| (Part::Added, *t)));
    }
    ops.extend(ra.get(ra.len().saturating_sub(suf)..).unwrap_or_default().iter().map(|t| (Part::Same, *t)));
    let mut merged: Vec<(Part, String)> = Vec::new();
    for (part, text) in ops {
        match merged.last_mut() {
            Some((last, s)) if *last == part => s.push_str(text),
            _ => merged.push((part, text.to_owned())),
        }
    }
    merged
}

/// The original with removed words struck through, then the suggestion with added words
/// underlined. Labels and text decoration make it understandable without relying on colour,
/// including when read by AccessKit.
fn draw_diff(ui: &mut egui::Ui, original: &str, suggested: &str) {
    let tokens = crate::theme::Tokens::get(ui.ctx());
    let parts = word_diff(original, suggested);
    for (heading, shown) in [(crate::i18n::tc("assistant", "Original"), Part::Removed), (tl!("Suggested"), Part::Added)] {
        ui.label(egui::RichText::new(heading).strong());
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for (part, text) in parts.iter().filter(|(p, _)| *p == Part::Same || *p == shown) {
                let rich = egui::RichText::new(text.as_str());
                ui.label(match (*part == shown, shown) {
                    (true, Part::Removed) => rich.strikethrough().color(tokens.text_dim),
                    (true, _) => rich.underline().color(tokens.accent_text),
                    (false, _) => rich,
                });
            }
        });
    }
}

/// A button press in the window, run after the window is drawn.
type Action = Option<(&'static str, Value)>;

/// Compact non-modal egui window; no provider call happens merely by opening it. The request
/// controls fold away when a proposal arrives, so the review fills the window.
pub fn show(app: &mut WordApp, ctx: &egui::Context) {
    if !app.assistant.open {
        return;
    }
    let mut open = true;
    let mut action: Action = None;
    // Escape closes the window like a dialog (the document doesn't take Escape while it's open),
    // unless a popup such as the scope list was open and takes it. Read before the window draws,
    // so the Escape that closes the popup doesn't close the window too.
    let escape = !egui::Popup::is_any_open(ctx) && ctx.input(|i| i.key_pressed(egui::Key::Escape));
    let live = (app.session.document_id(), app.session.rev());
    egui::Window::new(tl!("Writing Assistant"))
        .id(egui::Id::new("writing-assistant"))
        .open(&mut open)
        // Opens beside the page on the right, away from the text being reviewed.
        .pivot(egui::Align2::RIGHT_TOP)
        .default_pos(ctx.content_rect().right_top() + egui::vec2(-24.0, 190.0))
        .default_width(440.0)
        .default_height(620.0)
        .vscroll(false)
        .resizable(true)
        .show(ctx, |ui| {
            let state = &mut app.assistant;
            let busy = state.pending.is_some();
            account(ui, state, &mut action);
            ui.separator();
            let header = egui::CollapsingHeader::new(tl!("Request")).id_salt("assistant-request").default_open(true);
            let header = if std::mem::take(&mut state.collapse_request) { header.open(Some(false)) } else { header };
            header.show(ui, |ui| ui.add_enabled_ui(!busy, |ui| request(ui, state, &mut action)));
            if busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tl!("Claude is processing…"));
                    if ui.button(tl!("Cancel")).clicked() {
                        action = Some(("assistant.cancel", json!({})));
                    }
                });
            }
            ui.small(tl!(
                "Only explicitly requested text is sent. Review before applying. History stays in this window's memory and is cleared on exit."
            ));
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt("assistant-history")
                .auto_shrink([false, false])
                .show(ui, |ui| history(ui, state, live, &mut action));
        });
    if escape {
        open = false;
    }
    if !open {
        let _ = command(app, "ui.assistant", &json!({"value":false}));
    }
    if let Some((id, params)) = action {
        let result = app.execute(id, params);
        if id == "assistant.suggest" && result.is_ok() {
            app.assistant.confirmed = false;
        }
        if let Err(error) = result {
            app.assistant.message = error;
        }
    }
}

/// Connection state, the check button and the official sign-in instructions.
fn account(ui: &mut egui::Ui, state: &State, action: &mut Action) {
    ui.horizontal_wrapped(|ui| {
        ui.strong(if state.connection.is_some() { tl!("Connected with Claude account") } else { tl!("Disconnected / authentication required") });
        if ui.add_enabled(state.pending.is_none(), egui::Button::new(tl!("Check connection"))).clicked() {
            *action = Some(("assistant.auth", json!({})));
        }
    });
    ui.small(tl!("Uses official Claude Code with your account/OAuth. No API keys or fallback providers."));
    ui.collapsing(tl!("Sign in or change account"), |ui| {
        ui.label(tl!(
            "Install Claude Code, then run `claude auth login` in your terminal. To change account, use its official logout/login flow and check the connection again."
        ));
        ui.hyperlink_to(tl!("Official authentication instructions"), "https://code.claude.com/docs/en/authentication");
    });
    if !state.message.is_empty() {
        ui.label(&state.message);
    }
}

/// Scope, consent, the instruction and the quick actions.
fn request(ui: &mut egui::Ui, state: &mut State, action: &mut Action) {
    ui.horizontal(|ui| {
        ui.label(tl!("Scope"));
        egui::ComboBox::from_id_salt("assistant-scope").selected_text(scope_label(&state.scope)).show_ui(ui, |ui| {
            for scope in ["auto", "selection", "paragraph", "section", "document"] {
                if ui.selectable_value(&mut state.scope, scope.into(), scope_label(scope)).changed() {
                    state.confirmed = false;
                }
            }
        });
    });
    ui.small(tl!("Automatic: selected text, or the current paragraph when nothing is selected."));
    ui.horizontal(|ui| {
        ui.label(tl!("Model"));
        ui.add(egui::TextEdit::singleline(&mut state.model).hint_text(tl!("Claude Code default")).char_limit(64).desired_width(180.0));
    });
    if ui.checkbox(&mut state.include_document, tl!("Include the whole document as context")).changed() {
        state.confirmed = false;
    }
    // Consent is per request, not persisted or inferred from a previous request.
    ui.checkbox(&mut state.confirmed, tl!("I approve sending the whole document for this request"));
    ui.label(tl!("What would you like to do with this text?"));
    let field = ui.add(
        egui::TextEdit::multiline(&mut state.instruction)
            .id_salt("assistant-instruction")
            .hint_text(tl!("What would you like to do with this text?"))
            .desired_rows(2)
            .char_limit(4096)
            .desired_width(f32::INFINITY),
    );
    if std::mem::take(&mut state.focus) {
        field.request_focus();
    }
    ui.add_enabled_ui(may_send(state), |ui| {
        ui.horizontal_wrapped(|ui| {
            for (label, instruction) in [
                ("Improve writing", "Improve the writing, preserving its meaning and language."),
                ("Make concise", "Make this text more concise, preserving its meaning and language."),
                ("More formal", "Rewrite in a more formal tone, preserving its language."),
                ("More informal", "Rewrite in a more informal tone, preserving its language."),
                ("Correct spelling and grammar", "Correct spelling and grammar only, preserving the language and meaning."),
                ("Summarize", "Summarize concisely in the same language. Keep the response mapped to the supplied paragraph indices."),
            ] {
                if ui.add_sized([135.0, 32.0], egui::Button::new(tl!(label))).clicked() {
                    *action = Some(("assistant.suggest", request_params(state, instruction, false)));
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label(tl!("Tone"));
            ui.add(egui::TextEdit::singleline(&mut state.tone).char_limit(128).desired_width(180.0));
            if ui.add_enabled(!state.tone.trim().is_empty(), egui::Button::new(tl!("Rewrite tone"))).clicked() {
                *action = Some(("assistant.suggest", request_params(state, &format!("Rewrite in this tone: {}", state.tone), false)));
            }
        });
        ui.horizontal(|ui| {
            ui.label(tl!("Language"));
            ui.add(egui::TextEdit::singleline(&mut state.language).char_limit(128).desired_width(180.0));
            if ui.add_enabled(!state.language.trim().is_empty(), egui::Button::new(tl!("Translate"))).clicked() {
                *action = Some(("assistant.suggest", request_params(state, &format!("Translate into {}", state.language), false)));
            }
        });
        ui.horizontal(|ui| {
            if ui.add_enabled(!state.instruction.trim().is_empty(), egui::Button::new(crate::i18n::tc("assistant", "Generate"))).clicked() {
                *action = Some(("assistant.suggest", request_params(state, &state.instruction, false)));
            }
            if ui.button(tl!("Explain / suggest improvements")).clicked() {
                let instruction = format!("Explain the text and suggest improvements. {}", state.instruction);
                *action = Some(("assistant.suggest", request_params(state, &instruction, true)));
            }
        });
    });
}

/// Whether the chosen scope may be sent: anything but the whole document, unless approved.
fn may_send(state: &State) -> bool {
    (state.scope != "document" && !state.include_document) || state.confirmed
}

/// The proposals, newest first, each with its diffs and buttons. `live` is the open document's
/// identity and revision: a proposal read at another one can't be applied.
fn history(ui: &mut egui::Ui, state: &State, live: (u64, u64), action: &mut Action) {
    let busy = state.pending.is_some();
    for proposal in state.history.iter().filter(|p| !p.dismissed) {
        ui.push_id(proposal.id, |ui| {
            ui.strong(format!("{} #{}", tl!("Proposal"), proposal.id));
            if !proposal.response.explanation.is_empty() {
                ui.label(&proposal.response.explanation);
            }
            let same_doc = proposal.context.get("document_id").and_then(Value::as_u64) == Some(live.0);
            let fresh = same_doc && proposal.context.get("revision").and_then(Value::as_u64) == Some(live.1);
            if !fresh {
                ui.label(tl!("The document changed. Regenerate before applying this proposal."));
            }
            for suggestion in &proposal.response.suggestions {
                let original = proposal.originals.get(suggestion.index).map(String::as_str).unwrap_or("");
                draw_diff(ui, original, &suggestion.text);
                ui.horizontal(|ui| {
                    let applied = proposal.applied.contains(&suggestion.index);
                    if ui.add_enabled(fresh && !applied && !busy, egui::Button::new(tl!("Apply"))).clicked() {
                        *action = Some(("assistant.accept", json!({"id":proposal.id,"index":suggestion.index})));
                    }
                    if applied {
                        ui.label(tl!("Applied"));
                    }
                    if ui.button(tl!("Copy")).clicked() {
                        ui.ctx().copy_text(suggestion.text.clone());
                    }
                });
            }
            ui.horizontal_wrapped(|ui| {
                let pending = proposal.response.suggestions.iter().any(|s| !proposal.applied.contains(&s.index));
                if ui.add_enabled(fresh && pending && !busy, egui::Button::new(tl!("Apply all"))).clicked() {
                    *action = Some(("assistant.accept", json!({"id":proposal.id})));
                }
                if ui.button(crate::i18n::tc("assistant", "Discard")).clicked() {
                    *action = Some(("assistant.discard", json!({"id":proposal.id})));
                }
                if ui.add_enabled(!busy && may_send(state), egui::Button::new(tl!("Regenerate"))).clicked() {
                    *action = Some(("assistant.suggest", request_params(state, &proposal.instruction, proposal.explain_only)));
                }
                let can_undo = same_doc && proposal.undo_revision == Some(live.1);
                if ui.add_enabled(can_undo, egui::Button::new(tl!("Undo last application"))).clicked() {
                    *action = Some(("edit.undo", json!({})));
                }
            });
            ui.separator();
        });
    }
}

fn request_params(state: &State, instruction: &str, explain: bool) -> Value {
    let mut params = json!({"instruction":instruction,"scope":state.scope,"confirm_document":state.confirmed,
        "include_document_context":state.include_document,"explain_only":explain});
    if !state.model.trim().is_empty() {
        params["model"] = json!(state.model.trim());
    }
    params
}

fn scope_label(scope: &str) -> &'static str {
    match scope {
        "selection" => tl!("Selection"),
        "paragraph" => tl!("Paragraph"),
        "section" => tl!("Section"),
        "document" => tl!("Whole document"),
        _ => tl!("Automatic"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reviewed_app() -> WordApp {
        let mut app = WordApp::new(
            wordcraft_engine::Session::new(wordcraft_doc::Document::from_text("A very long sentence.\nKeep this paragraph.")),
            Default::default(),
        );
        app.session.run("select.text", &json!({"text":"A very long sentence."})).unwrap();
        let context = app.session.run("assistant.context", &json!({"scope":"auto"})).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        app.assistant.pending = Some(Pending {
            cancel: Arc::new(AtomicBool::new(false)),
            receiver,
            context: Some(context),
            instruction: "Make concise".into(),
            explain_only: false,
        });
        sender
            .send(Answer::Suggestion(Ok((
                AuthState::Account,
                Response {
                    suggestions: vec![wordcraft_assistant::Suggestion { index: 0, text: "A short sentence.".into() }],
                    explanation: "Removed unnecessary words.".into(),
                },
            ))))
            .unwrap();
        poll(&mut app);
        app.assistant.open = true;
        app
    }

    #[test]
    fn mocked_request_review_apply_and_native_undo() {
        let mut app = reviewed_app();
        assert_eq!(app.assistant.history.len(), 1);
        assert_eq!(app.session.doc.plain_text(wordcraft_doc::StoryRef::Body), "A very long sentence.\nKeep this paragraph.");
        app.execute("assistant.accept", json!({"id":1})).unwrap();
        assert_eq!(app.session.doc.plain_text(wordcraft_doc::StoryRef::Body), "A short sentence.\nKeep this paragraph.");
        assert_eq!(app.assistant.history.front().unwrap().originals[0], "A very long sentence.");
        app.execute("edit.undo", json!({})).unwrap();
        assert_eq!(app.session.doc.plain_text(wordcraft_doc::StoryRef::Body), "A very long sentence.\nKeep this paragraph.");
    }

    #[test]
    fn successive_applications_of_one_proposal_each_undo_separately() {
        let mut app = WordApp::new(
            wordcraft_engine::Session::new(wordcraft_doc::Document::from_text("First draft line.\nSecond draft line.\nUntouched.")),
            Default::default(),
        );
        let pos = |block: u32, off: usize| json!({"story":"body","path":[block],"off":off});
        app.session.run("select.range", &json!({"anchor":pos(0, 0),"focus":pos(1, "Second draft line.".len())})).unwrap();
        let context = app.session.run("assistant.context", &json!({"scope":"selection"})).unwrap();
        assert_eq!(context["targets"].as_array().map(Vec::len), Some(2));
        let (sender, receiver) = std::sync::mpsc::channel();
        app.assistant.pending = Some(Pending {
            cancel: Arc::new(AtomicBool::new(false)),
            receiver,
            context: Some(context),
            instruction: "Tidy".into(),
            explain_only: false,
        });
        sender
            .send(Answer::Suggestion(Ok((
                AuthState::Account,
                Response {
                    suggestions: vec![
                        wordcraft_assistant::Suggestion { index: 0, text: "First line.".into() },
                        wordcraft_assistant::Suggestion { index: 1, text: "Second line.".into() },
                    ],
                    explanation: String::new(),
                },
            ))))
            .unwrap();
        poll(&mut app);
        let body = |app: &WordApp| app.session.doc.plain_text(wordcraft_doc::StoryRef::Body);
        app.execute("assistant.accept", json!({"id":1,"index":1})).unwrap();
        assert_eq!(body(&app), "First draft line.\nSecond line.\nUntouched.");
        // The refreshed context is at the new revision, so the other suggestion still applies.
        app.execute("assistant.accept", json!({"id":1,"index":0})).unwrap();
        assert_eq!(body(&app), "First line.\nSecond line.\nUntouched.");
        assert!(app.execute("assistant.accept", json!({"id":1})).is_err(), "nothing left to apply");
        app.execute("edit.undo", json!({})).unwrap();
        assert_eq!(body(&app), "First draft line.\nSecond line.\nUntouched.");
        app.execute("edit.undo", json!({})).unwrap();
        assert_eq!(body(&app), "First draft line.\nSecond draft line.\nUntouched.");
    }

    #[test]
    fn successive_applications_work_with_track_changes() {
        let mut app = WordApp::new(
            wordcraft_engine::Session::new(wordcraft_doc::Document::from_text("First draft line.\nSecond draft line.")),
            Default::default(),
        );
        app.session.doc.settings.track_changes = true;
        let context = app.session.run("assistant.context", &json!({"scope":"document","confirm_document":true})).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        app.assistant.pending = Some(Pending {
            cancel: Arc::new(AtomicBool::new(false)),
            receiver,
            context: Some(context),
            instruction: "Tidy".into(),
            explain_only: false,
        });
        sender
            .send(Answer::Suggestion(Ok((
                AuthState::Account,
                Response {
                    suggestions: vec![
                        wordcraft_assistant::Suggestion { index: 0, text: "First line.".into() },
                        wordcraft_assistant::Suggestion { index: 1, text: "Second line.".into() },
                    ],
                    explanation: String::new(),
                },
            ))))
            .unwrap();
        poll(&mut app);
        app.execute("assistant.accept", json!({"id":1,"index":0})).unwrap();
        // The proposal follows the document's revision, so the other suggestion still applies.
        app.execute("assistant.accept", json!({"id":1,"index":1})).unwrap();
        assert!(app.execute("assistant.accept", json!({"id":1})).is_err(), "nothing left to apply");
    }

    #[test]
    fn proposal_keeps_the_users_instruction_and_explain_mode_for_regenerate() {
        let mut app = WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::from_text("Hello there.\nSecond.")), Default::default());
        let context = app.session.run("assistant.context", &json!({"scope":"auto"})).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        app.assistant.pending = Some(Pending {
            cancel: Arc::new(AtomicBool::new(false)),
            receiver,
            context: Some(context),
            instruction: "Why?".into(),
            explain_only: true,
        });
        sender.send(Answer::Suggestion(Ok((AuthState::Account, Response { suggestions: Vec::new(), explanation: "Because.".into() })))).unwrap();
        poll(&mut app);
        let proposal = app.assistant.history.front().unwrap();
        assert_eq!(proposal.instruction, "Why?", "no explain prefix or language line to send again");
        assert!(proposal.explain_only);
        let params = request_params(&app.assistant, &proposal.instruction, proposal.explain_only);
        assert_eq!(params["instruction"], "Why?");
        assert_eq!(params["explain_only"], true);
    }

    #[test]
    fn stale_and_discarded_proposals_cannot_be_applied() {
        let mut app = reviewed_app();
        app.execute("text.insert", json!({"text":"Someone edited this"})).unwrap();
        assert!(app.execute("assistant.accept", json!({"id":1})).is_err());
        app.execute("assistant.discard", json!({"id":1})).unwrap();
        assert!(app.execute("assistant.accept", json!({"id":1})).is_err());
    }

    #[test]
    fn review_exposes_original_suggested_and_apply_to_accesskit() {
        use egui_kittest::kittest::Queryable;
        let app = reviewed_app();
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(1100.0, 1000.0)).build_ui_state(
            |ui, app: &mut WordApp| {
                app.logic(ui.ctx());
                app.ui(ui);
            },
            app,
        );
        harness.run_steps(6);
        assert!(harness.query_by_label("Original").is_some());
        assert!(harness.query_by_label("Suggested").is_some());
        harness.get_by_label("Apply").click();
        harness.run_steps(3);
        assert_eq!(harness.state().session.doc.plain_text(wordcraft_doc::StoryRef::Body), "A short sentence.\nKeep this paragraph.");
    }

    #[test]
    fn shortcut_opens_and_escape_closes_without_sending() {
        let app = WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::from_text("Hello")), Default::default());
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1100.0, 900.0)).build_ui_state(
            |ui, app: &mut WordApp| {
                app.logic(ui.ctx());
                app.ui(ui);
            },
            app,
        );
        h.run_steps(3);
        h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::ALT | egui::Modifiers::SHIFT, egui::Key::A);
        h.run_steps(3);
        assert!(h.state().assistant.open);
        assert!(h.state().assistant.pending.is_none(), "opening never starts Claude Code");
        h.key_press(egui::Key::Escape);
        h.run_steps(3);
        assert!(!h.state().assistant.open);
        assert_eq!(h.state().session.doc.plain_text(wordcraft_doc::StoryRef::Body), "Hello");
    }

    /// Explicit opt-in render check; synthetic text and a fake provider result only.
    #[test]
    #[ignore = "offscreen GPU screenshot; run explicitly with WORDCRAFT_ASSISTANT_SCREENSHOT"]
    fn assistant_review_screenshot() {
        let _gpu = crate::gpu_test_lock();
        let path = std::env::var("WORDCRAFT_ASSISTANT_SCREENSHOT").expect("set a screenshot output path");
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(1100.0, 1000.0)).with_pixels_per_point(1.0).wgpu().build_ui_state(
            |ui, app: &mut WordApp| {
                app.logic(ui.ctx());
                app.ui(ui);
            },
            reviewed_app(),
        );
        harness.input_mut().max_texture_side = Some(8192);
        harness.run_steps(8);
        harness.render().unwrap().save(path).unwrap();
    }
    #[test]
    fn unicode_diff_is_bounded_and_preserves_text() {
        let side = |parts: &[(Part, String)], keep: Part| -> String {
            parts.iter().filter(|(p, _)| *p == Part::Same || *p == keep).map(|(_, t)| t.as_str()).collect()
        };
        let long_a = "word ".repeat(600);
        let long_b = "other ".repeat(600);
        for (a, b) in [
            ("café corto", "café breve"),
            ("你好", "你很好"),
            ("", "new"),
            ("old", ""),
            ("same", "same"),
            ("Ths sentense has two erors.", "This sentence has two errors."),
            ("a, b; c", "a; b, c!"),
            (long_a.as_str(), long_b.as_str()),
        ] {
            let parts = word_diff(a, b);
            assert_eq!(side(&parts, Part::Removed), a);
            assert_eq!(side(&parts, Part::Added), b);
        }
        // Only the misspelt words are marked; the words in between stay shared.
        let parts = word_diff("Ths sentense has two erors.", "This sentence has two errors.");
        let changed = |kind: Part| parts.iter().filter(|(p, _)| *p == kind).map(|(_, t)| t.as_str()).collect::<Vec<_>>();
        assert_eq!(changed(Part::Removed), ["Ths", "sentense", "erors"]);
        assert_eq!(changed(Part::Added), ["This", "sentence", "errors"]);
        assert!(parts.contains(&(Part::Same, " has two ".to_owned())));
    }
    #[test]
    fn opening_does_not_authenticate_or_send_anything() {
        let mut app = WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::new()), Default::default());
        assert!(app.execute("ui.assistant", json!({"value":true})).is_ok());
        assert!(app.assistant.open);
        assert!(app.assistant.pending.is_none());
        assert!(app.assistant.connection.is_none());
    }
    #[test]
    fn a_model_name_that_could_be_a_flag_is_refused_before_starting() {
        let mut app = WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::from_text("One.\nTwo.")), Default::default());
        for model in [json!("--tools"), json!("opus model"), json!(7)] {
            assert!(app.execute("assistant.suggest", json!({"instruction":"Improve","model":model})).is_err());
            assert!(app.assistant.pending.is_none());
        }
    }

    #[test]
    fn no_full_document_without_explicit_boolean_consent() {
        let mut app = WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::new()), Default::default());
        for confirm in [json!(false), json!("true"), Value::Null] {
            assert!(
                app.execute("assistant.suggest", json!({"instruction":"Improve","include_document_context":true,"confirm_document":confirm}))
                    .is_err()
            );
            assert!(app.assistant.pending.is_none());
        }
    }
}
