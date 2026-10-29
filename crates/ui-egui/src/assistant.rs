//! Non-modal writing review. The provider never edits the document: accepted text goes through
//! `assistant.apply`, with the engine's stale-snapshot, protection and undo checks.
use std::collections::VecDeque;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{Receiver, TryRecvError},
};

use serde_json::{Value, json};
#[cfg(not(target_arch = "wasm32"))]
use wordcraft_assistant::Provider;
use wordcraft_assistant::{AuthState, Request, Response};

use crate::WordApp;

const HISTORY_LIMIT: usize = 8;
const HISTORY_BYTES: usize = 1024 * 1024;

/// Contributor-provided original artwork, embedded for offline/portable builds. Decode only
/// once per egui context (including a failed decode); no filesystem or provider access.
pub(crate) fn icon_texture(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let id = egui::Id::unique("writing-assistant-artwork");
    if let Some(cached) = ctx.data(|data| data.get_temp::<Option<egui::TextureHandle>>(id)) {
        return cached;
    }
    let texture = image::load_from_memory(include_bytes!("../../../assets/assistant/wordcraft-assistant.png")).ok().map(|image| {
        let image = image.resize(256, 256, image::imageops::FilterType::Lanczos3).into_rgba8();
        let pixels = egui::ColorImage::from_rgba_unmultiplied([image.width() as usize, image.height() as usize], image.as_raw());
        ctx.load_texture("writing-assistant-artwork", pixels, egui::TextureOptions::LINEAR)
    });
    ctx.data_mut(|data| data.insert_temp(id, texture.clone()));
    texture
}

/// Mirai wordmark logo embedded for the assistant header. Rasterise the original transparent SVG
/// only once per egui context so its palette and aspect ratio stay intact.
pub(crate) fn logo_texture(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let id = egui::Id::unique("mirai-assistant-logo");
    if let Some(cached) = ctx.data(|data| data.get_temp::<Option<egui::TextureHandle>>(id)) {
        return cached;
    }
    let texture = egui_extras::image::load_svg_bytes_with_size(
        include_bytes!("../../../assets/assistant/mirai-gorrion.svg"),
        egui::SizeHint::Width(256),
        &Default::default(),
    )
    .ok()
    .map(|image| ctx.load_texture("mirai-assistant-logo", image, egui::TextureOptions::LINEAR));
    ctx.data_mut(|data| data.insert_temp(id, texture.clone()));
    texture
}

enum Answer {
    Auth(Result<AuthState, String>),
    // The web build has no worker to send it (`launch` refuses there); tests still build it.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
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
    /// The whole-document warning was shown for the current draft: sending again confirms it.
    warned: bool,
    connection: Option<AuthState>,
    connection_error: Option<String>,
    message: String,
    pending: Option<Pending>,
    history: VecDeque<Proposal>,
    next_id: u64,
    focus: bool,
    ime_composing: bool,
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
            warned: false,
            connection: None,
            connection_error: None,
            message: String::new(),
            pending: None,
            history: VecDeque::new(),
            next_id: 1,
            focus: false,
            ime_composing: false,
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
        ("ui.assistant", "Mirai Assistant", "value?: bool; omitted toggles the right assistant panel"),
        ("ui.assistant.newConversation", "New conversation", "Clears the assistant panel's local conversation, draft, consent and errors; never changes the document or authentication"),
        ("assistant.auth", "Check Claude connection", "Checks the official Claude Code account/OAuth login; no document is sent"),
        ("assistant.suggest", "Request writing suggestions", "instruction: string; scope?: auto|selection|paragraph|section|document; auto uses selected text, or the whole document when nothing is selected; confirm_document?: bool is required for whole-body scope or include_document_context (the panel warns once instead: sending again confirms); explain_only?: bool; model?: string (Claude alias or id; omitted: Claude Code default). Sends the chosen text to Claude. Returns a job; poll assistant.status."),
        ("assistant.status", "Writing assistant status", "Returns busy, connection, message and local proposals"),
        ("assistant.cancel", "Cancel writing request", "Cancels the running local Claude Code process; cannot retract already transmitted text"),
        ("assistant.accept", "Apply writing suggestion", "id: proposal id; index?: target index (omitted applies all pending suggestions)"),
        ("assistant.discard", "Discard writing suggestion", "id: proposal id"),
    ].into_iter().map(|(id, label, params)| json!({"id":id,"label":label,"location":"Review › Mirai Assistant",
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
        "ui.assistant.newConversation" => {
            if app.assistant.pending.is_some() {
                return Some(Err("a writing request is already running".into()));
            }
            app.assistant.instruction.clear();
            app.assistant.warned = false;
            app.assistant.message.clear();
            app.assistant.connection_error = None;
            app.assistant.history.clear();
            app.assistant.focus = true;
            Ok(json!({"cleared":true}))
        }
        "assistant.auth" => {
            app.assistant.connection_error = None;
            match start_auth(app) {
                Ok(value) => Ok(value),
                Err(error) => {
                    app.assistant.connection_error = Some(error.clone());
                    app.assistant.message.clone_from(&error);
                    Err(error)
                }
            }
        }
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
                    // One sign-in check and one generation: each starts a Claude Code process.
                    client.generate_with_auth(&request, &worker_cancel).map_err(|e| e.to_string())
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
    let structure = context
        .get("targets")
        .and_then(Value::as_array)
        .map(|targets| targets.iter().map(|t| t.get("structure").and_then(Value::as_str).unwrap_or("paragraph").to_owned()).collect())
        .unwrap_or_default();
    let request = Request { instruction: sent, targets, structure, context: document_context };
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
        if app.assistant.instruction.is_empty() {
            app.assistant.instruction.clone_from(&pending.instruction);
        }
        app.assistant.message = tl!("Request cancelled. Text already sent cannot be retracted.").into();
        return;
    }
    match answer {
        Answer::Auth(Ok(auth)) => {
            app.assistant.connection = Some(auth);
            app.assistant.connection_error = None;
            app.assistant.message.clear();
        }
        Answer::Suggestion(Ok((auth, mut response))) => {
            app.assistant.connection = Some(auth);
            app.assistant.connection_error = None;
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
                trim_history(&mut app.assistant);
            }
        }
        Answer::Auth(Err(error)) => {
            if app.assistant.instruction.is_empty() {
                app.assistant.instruction.clone_from(&pending.instruction);
            }
            app.assistant.connection = None;
            app.assistant.connection_error = Some(error.clone());
            app.assistant.message = error;
        }
        Answer::Suggestion(Err(error)) => {
            if app.assistant.instruction.is_empty() {
                app.assistant.instruction.clone_from(&pending.instruction);
            }
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
                    .saturating_add(p.instruction.len())
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
        .map(|s| json!({"index":s.index,"text":s.text,"markdown":s.markdown}))
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
fn draw_diff(ui: &mut egui::Ui, original: &str, suggested: &str, markdown: bool) {
    let tokens = crate::theme::Tokens::get(ui.ctx());
    let parts = word_diff(original, suggested);
    // Markdown replaces the whole target, formatting included; its marks (#, -, **) show as typed.
    let suggested_heading = if !markdown {
        tl!("Suggested")
    } else if suggested.trim().is_empty() {
        tl!("Suggested: remove this paragraph")
    } else {
        tl!("Suggested (formatted)")
    };
    for (heading, shown) in [(crate::i18n::tc("assistant", "Original"), Part::Removed), (suggested_heading, Part::Added)] {
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

/// A button press in the panel, run after it is drawn.
type Action = Option<(&'static str, Value)>;

/// Non-modal, docked assistant panel. Opening it never starts a provider call.
pub fn show(app: &mut WordApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    if !app.assistant.open {
        return;
    }
    let mut action: Action = None;
    // Escape closes the panel unless a popup such as the scope list was open and takes it.
    let escape = !egui::Popup::is_any_open(ctx) && ctx.input(|i| i.key_pressed(egui::Key::Escape));
    let live = (app.session.document_id(), app.session.rev());
    let has_text_selection = !app.session.sel.is_collapsed();
    // Sections are paragraphs carrying a section break; avoid allocating the sections Vec each frame.
    let one_section = !app.session.doc.body.iter().any(|block| block.as_ref().as_para().and_then(|para| para.section.as_ref()).is_some());
    let ime_event_this_frame = ctx.input(|input| update_ime_composition(&mut app.assistant, &input.events));
    let tokens = crate::theme::Tokens::get(ctx);
    let frame = egui::Frame::new()
        .fill(tokens.assistant_background)
        .stroke(egui::Stroke::new(1.0, tokens.assistant_border))
        .inner_margin(egui::Margin::same(12));
    egui::Panel::right("writing-assistant-panel").resizable(true).default_size(280.0).min_size(240.0).max_size(520.0).frame(frame).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        let state = &mut app.assistant;
        let busy = state.pending.is_some();
        header(ui, state, &mut action);
        context_note(ui);
        account_access(ui, state, &mut action);
        if let Some(error) = state.connection_error.as_deref() {
            error_card(ui, error);
        } else if state.pending.is_none() && !state.message.is_empty() {
            error_card(ui, &state.message);
        }
        egui::Panel::bottom("assistant-composer-panel").frame(egui::Frame::NONE).show(ui, |ui| {
            ui.add_enabled_ui(!busy, |ui| {
                writing_options(ui, state, &mut action);
                request(ui, state, has_text_selection, ime_event_this_frame, &mut action);
            });
        });
        egui::ScrollArea::vertical().id_salt("assistant-history").auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            history(ui, state, live, &mut action);
            if let Some(pending) = &state.pending {
                if pending.context.is_some() {
                    message(ui, tl!("You"), &pending.instruction, true);
                }
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tl!("Claude is processing…"));
                    if ui.button(tl!("Cancel")).clicked() {
                        action = Some(("assistant.cancel", json!({})));
                    }
                });
            }
        });
    });
    if escape {
        let _ = command(app, "ui.assistant", &json!({"value":false}));
    }
    if let Some((id, params)) = action {
        // Sending the whole document first shows a warning; sending again (Enter or Send) goes ahead.
        if id == "assistant.suggest" && sends_whole_document(&app.assistant, has_text_selection, one_section) && !app.assistant.warned {
            app.assistant.warned = true;
            app.assistant.focus = true;
            return;
        }
        let submitted_draft = is_draft_submission(&app.assistant, id, &params);
        let result = app.execute(id, params);
        if id == "assistant.suggest" && result.is_ok() {
            app.assistant.warned = false;
            if submitted_draft {
                app.assistant.instruction.clear();
            }
        }
        if let Err(error) = result {
            app.assistant.message.clone_from(&error);
            if id == "assistant.auth" {
                app.assistant.connection_error = Some(error);
            }
        }
    }
}

fn is_draft_submission(state: &State, id: &str, params: &Value) -> bool {
    id == "assistant.suggest"
        && !state.instruction.is_empty()
        && params
            .get("instruction")
            .and_then(Value::as_str)
            .is_some_and(|sent| sent == state.instruction || sent == format!("Explain the text and suggest improvements. {}", state.instruction))
}

fn update_ime_composition(state: &mut State, events: &[egui::Event]) -> bool {
    let mut saw_ime_event = false;
    for event in events {
        if let egui::Event::Ime(ime) = event {
            saw_ime_event = true;
            match ime {
                egui::ImeEvent::Preedit { text, .. } => state.ime_composing = !text.is_empty(),
                egui::ImeEvent::Commit(_) => state.ime_composing = false,
                _ => {}
            }
        }
    }
    saw_ime_event
}

fn should_send_on_enter(modifiers: egui::Modifiers, composing: bool, ime_event: bool) -> bool {
    !composing && !ime_event && !modifiers.shift && !modifiers.alt
}

/// Header status is intentionally textual as well as coloured.
fn header(ui: &mut egui::Ui, state: &State, action: &mut Action) {
    let tokens = crate::theme::Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        if let Some(texture) = logo_texture(ui.ctx()) {
            ui.add(egui::Image::from_texture(&texture).alt_text("Mirai").max_height(64.0))
                .on_hover_text(tl!("Claude Code · consult the current document without applying changes automatically"));
        } else {
            ui.label(egui::RichText::new(tl!("Mirai")).strong().italics().size(17.0))
                .on_hover_text(tl!("Claude Code · consult the current document without applying changes automatically"));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            if ui.add(egui::Button::new("×").frame(false).small()).on_hover_text(tl!("Close")).clicked() {
                *action = Some(("ui.assistant", json!({"value":false})));
            }
        });
    });
    ui.horizontal(|ui| {
        status_badge(ui, state.connection.is_some());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let new_conversation = egui::Button::new(egui::RichText::new(tl!("New conversation")).small().color(tokens.assistant_muted)).frame(false);
            if ui.add_enabled(state.pending.is_none(), new_conversation).clicked() {
                *action = Some(("ui.assistant.newConversation", json!({})));
            }
        });
    });
}

fn status_badge(ui: &mut egui::Ui, connected: bool) {
    let tokens = crate::theme::Tokens::get(ui.ctx());
    let color = if connected { tokens.green } else { tokens.assistant_error };
    ui.horizontal(|ui| {
        let (dot, _) = ui.allocate_exact_size(egui::vec2(6.0, 6.0), egui::Sense::hover());
        ui.painter().circle_filled(dot.center(), 3.0, color);
        ui.label(egui::RichText::new(if connected { tl!("Connected") } else { tl!("Disconnected") }).small().color(tokens.assistant_muted))
            .on_hover_text(if connected { tl!("Connected with Claude account") } else { tl!("Disconnected / authentication required") });
    });
}

fn context_note(ui: &mut egui::Ui) {
    let tokens = crate::theme::Tokens::get(ui.ctx());
    egui::Frame::new().fill(tokens.assistant_surface).corner_radius(7).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("↳").strong().color(tokens.assistant_info));
            ui.label(egui::RichText::new(tl!("Reads a copy of the open document to answer. It does not modify your text.")).small());
        });
    });
}

/// Visible account access stays outside Writing options. The disclosure only shows official CLI
/// instructions; only the separate connection-check button runs `claude auth status`.
fn account_access(ui: &mut egui::Ui, state: &State, action: &mut Action) {
    let connected = state.connection.is_some();
    let disclosure = if connected { tl!("Change account") } else { tl!("Sign in") };
    ui.collapsing(disclosure, |ui| {
        if connected {
            ui.label(tl!("To change accounts, use Claude Code's official sign-out and sign-in flow in a terminal."));
            ui.horizontal_wrapped(|ui| {
                ui.monospace("claude auth logout");
                if ui.button(tl!("Copy sign-out command")).clicked() {
                    ui.ctx().copy_text("claude auth logout".into());
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.monospace("claude auth login");
                if ui.button(tl!("Copy sign-in command")).clicked() {
                    ui.ctx().copy_text("claude auth login".into());
                }
            });
        } else {
            ui.label(tl!("In a terminal, run the official Claude Code login command:"));
            ui.horizontal_wrapped(|ui| {
                ui.monospace("claude auth login");
                if ui.button(tl!("Copy command")).clicked() {
                    ui.ctx().copy_text("claude auth login".into());
                }
            });
            ui.small(tl!("WordCraft does not start login or handle your credentials."));
        }
        ui.hyperlink_to(tl!("Official authentication instructions"), "https://code.claude.com/docs/en/authentication");
    });
    ui.small(tl!("Check connection only checks the current Claude Code login; it does not sign in."));
    if ui.add_enabled(state.pending.is_none(), egui::Button::new(tl!("Check connection"))).clicked() {
        *action = Some(("assistant.auth", json!({})));
    }
}

fn error_card(ui: &mut egui::Ui, error: &str) {
    let tokens = crate::theme::Tokens::get(ui.ctx());
    egui::Frame::new()
        .fill(tokens.assistant_surface)
        .stroke(egui::Stroke::new(1.0, tokens.assistant_error))
        .corner_radius(7)
        .inner_margin(egui::Margin::symmetric(9, 8))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(error).color(tokens.text));
        });
}

fn writing_options(ui: &mut egui::Ui, state: &mut State, action: &mut Action) {
    let tokens = crate::theme::Tokens::get(ui.ctx());
    ui.collapsing(egui::RichText::new(tl!("Writing options")).small().color(tokens.assistant_muted), |ui| {
        egui::ScrollArea::vertical().id_salt("assistant-options").max_height(240.0).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(tl!("Scope"));
                craft_ui::ComboBox::from_id_salt("assistant-scope").selected_text(scope_label(&state.scope)).show_ui(ui, |ui| {
                    for scope in ["auto", "selection", "paragraph", "section", "document"] {
                        if ui.selectable_value(&mut state.scope, scope.into(), scope_label(scope)).changed() {
                            state.warned = false;
                        }
                    }
                });
            });
            ui.small(tl!("Automatic: selected text, or the whole document when nothing is selected."));
            ui.horizontal_wrapped(|ui| {
                let model_label = ui.label(tl!("Model"));
                craft_ui::ComboBox::from_id_salt("assistant-model")
                    .selected_text(if state.model.is_empty() { tl!("Claude Code default") } else { state.model.as_str() })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut state.model, String::new(), tl!("Claude Code default"));
                        for (alias, label) in [("sonnet", "Sonnet"), ("opus", "Opus"), ("haiku", "Haiku")] {
                            ui.selectable_value(&mut state.model, alias.into(), tl!(label));
                        }
                    })
                    .response
                    .labelled_by(model_label.id);
            });
            ui.add(
                egui::TextEdit::singleline(&mut state.model).hint_text(tl!("Custom model alias or id")).char_limit(64).desired_width(f32::INFINITY),
            );
            if ui.checkbox(&mut state.include_document, tl!("Include the whole document as context")).changed() {
                state.warned = false;
            }
            {
                ui.horizontal_wrapped(|ui| {
                    for (label, instruction) in [
                        ("More formal", "Rewrite in a more formal tone, preserving its language."),
                        ("More informal", "Rewrite in a more informal tone, preserving its language."),
                        ("Summarize", "Summarize concisely in the same language. Keep the response mapped to the supplied paragraph indices."),
                    ] {
                        if chip(ui, tl!(label)).clicked() {
                            state.instruction = instruction.into();
                            state.focus = true;
                        }
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(tl!("Tone"));
                    ui.add(egui::TextEdit::singleline(&mut state.tone).char_limit(128).desired_width(120.0));
                    if ui.add_enabled(!state.tone.trim().is_empty(), egui::Button::new(tl!("Rewrite tone"))).clicked() {
                        *action = Some(("assistant.suggest", request_params(state, &format!("Rewrite in this tone: {}", state.tone), false)));
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(tl!("Language"));
                    ui.add(egui::TextEdit::singleline(&mut state.language).char_limit(128).desired_width(120.0));
                    if ui.add_enabled(!state.language.trim().is_empty(), egui::Button::new(tl!("Translate"))).clicked() {
                        *action = Some(("assistant.suggest", request_params(state, &format!("Translate into {}", state.language), false)));
                    }
                });
            }
        });
    });
}

fn set_prompt(state: &mut State, prompt: &str) {
    state.instruction = prompt.to_owned();
    state.focus = true;
}

fn sparkle_tile(ui: &mut egui::Ui) {
    let tokens = crate::theme::Tokens::get(ui.ctx());
    egui::Frame::new().fill(tokens.assistant_bubble).corner_radius(9).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::hover());
        let center = rect.center();
        let r = 9.0;
        let inner = 1.7;
        let points = vec![
            center + egui::vec2(0.0, -r),
            center + egui::vec2(inner, -inner),
            center + egui::vec2(r, 0.0),
            center + egui::vec2(inner, inner),
            center + egui::vec2(0.0, r),
            center + egui::vec2(-inner, inner),
            center + egui::vec2(-r, 0.0),
            center + egui::vec2(-inner, -inner),
        ];
        ui.painter().add(egui::Shape::convex_polygon(points, tokens.assistant_pink, egui::Stroke::NONE));
        let small = center + egui::vec2(8.0, -7.0);
        let sparkle = vec![
            small + egui::vec2(0.0, -2.5),
            small + egui::vec2(0.6, -0.6),
            small + egui::vec2(2.5, 0.0),
            small + egui::vec2(0.6, 0.6),
            small + egui::vec2(0.0, 2.5),
            small + egui::vec2(-0.6, 0.6),
            small + egui::vec2(-2.5, 0.0),
            small + egui::vec2(-0.6, -0.6),
        ];
        ui.painter().add(egui::Shape::convex_polygon(sparkle, tokens.assistant_pink, egui::Stroke::NONE));
    });
}

fn welcome(ui: &mut egui::Ui, state: &mut State) {
    let tokens = crate::theme::Tokens::get(ui.ctx());
    ui.add_space(14.0);
    sparkle_tile(ui);
    ui.add_space(8.0);
    ui.label(egui::RichText::new(tl!("How can I help with your document?")).strong().size(16.0).color(tokens.text));
    ui.label(egui::RichText::new(tl!("Choose an idea to start or write your own question.")).small().color(tokens.assistant_muted));
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        for (label, prompt) in [
            ("Summarize main ideas", "Summarize the document's main ideas."),
            ("Review clarity", "Review the document's clarity and point out parts that could be expressed more clearly."),
            ("Suggest next steps", "Suggest concrete next steps based on this document."),
        ] {
            if chip(ui, tl!(label)).clicked() {
                set_prompt(state, tl!(prompt));
            }
        }
    });
}

/// What to send, always in view: the selection (or the whole document without one), only the
/// selection, or the whole document. Paragraph and section stay under Writing options.
fn scope_bar(ui: &mut egui::Ui, state: &mut State, has_text_selection: bool) {
    let tokens = crate::theme::Tokens::get(ui.ctx());
    ui.horizontal_wrapped(|ui| {
        for scope in ["auto", "selection", "document"] {
            if ui.selectable_value(&mut state.scope, scope.into(), egui::RichText::new(scope_label(scope)).small()).changed() {
                state.warned = false;
            }
        }
    });
    let sends = match state.scope.as_str() {
        "selection" => tl!("Will send the selected text"),
        "paragraph" => tl!("Will send the paragraph with the caret"),
        "section" => tl!("Will send the section with the caret"),
        "document" => tl!("Will send the whole document"),
        _ if has_text_selection => tl!("Will send the selected text"),
        _ => tl!("Will send the whole document"),
    };
    ui.label(egui::RichText::new(sends).small().color(tokens.assistant_muted));
}

/// Whether the request would send every body paragraph, or the whole document as context.
fn sends_whole_document(state: &State, has_text_selection: bool, one_section: bool) -> bool {
    state.include_document
        || match state.scope.as_str() {
            "document" => true,
            "auto" => !has_text_selection,
            "section" => one_section,
            _ => false,
        }
}

/// The compact fixed composer. Sending the whole document shows a warning first; sending again
/// confirms it for that request.
fn request(ui: &mut egui::Ui, state: &mut State, has_text_selection: bool, ime_event_this_frame: bool, action: &mut Action) {
    let tokens = crate::theme::Tokens::get(ui.ctx());
    scope_bar(ui, state, has_text_selection);
    if state.warned {
        egui::Frame::new()
            .fill(tokens.assistant_surface)
            .stroke(egui::Stroke::new(1.0, tokens.assistant_pink))
            .corner_radius(7)
            .inner_margin(egui::Margin::symmetric(9, 6))
            .show(ui, |ui| {
                ui.label(egui::RichText::new(tl!("The whole document will be sent to Claude. Press Enter or Send again to continue.")).small());
            });
    }
    let can_send = !state.instruction.trim().is_empty();
    let field_id = egui::Id::unique("assistant-instruction");
    let sends_enter = can_send
        && ui.memory(|memory| memory.has_focus(field_id))
        && ui.input_mut(|input| {
            let modifiers = input.modifiers;
            input.key_pressed(egui::Key::Enter)
                && should_send_on_enter(modifiers, state.ime_composing, ime_event_this_frame)
                && input.consume_key(modifiers, egui::Key::Enter)
        });
    if can_send && sends_enter {
        *action = Some(("assistant.suggest", request_params(state, &state.instruction, false)));
    }
    egui::Frame::new()
        .fill(tokens.assistant_surface)
        .stroke(egui::Stroke::new(1.0, tokens.assistant_border))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(9, 8))
        .show(ui, |ui| {
            let field = egui::ScrollArea::vertical()
                .id_salt("assistant-draft")
                .max_height(74.0)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut state.instruction)
                            .id(field_id)
                            .hint_text(tl!("Ask about your document…"))
                            .font(egui::FontId::proportional(14.0))
                            .char_limit(4096)
                            .frame(egui::Frame::NONE)
                            .min_size(egui::vec2(0.0, 58.0))
                            .desired_width(f32::INFINITY),
                    )
                })
                .inner;
            if std::mem::take(&mut state.focus) {
                field.request_focus();
            }
            ui.add_space(4.0);
            ui.label(egui::RichText::new(tl!("Press Enter to send · Shift+Enter for a new line")).small().color(tokens.assistant_muted));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let send = egui::Button::new(egui::RichText::new(tl!("Send")).strong().color(tokens.assistant_on_button))
                    .fill(tokens.assistant_button)
                    .corner_radius(6)
                    .min_size(egui::vec2(62.0, 30.0));
                if ui.add_enabled(can_send, send).clicked() {
                    *action = Some(("assistant.suggest", request_params(state, &state.instruction, false)));
                }
            });
        });
}

/// A rounded quick-action chip in the fork's assistant palette.
fn chip(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let tokens = crate::theme::Tokens::get(ui.ctx());
    ui.add(
        egui::Button::new(egui::RichText::new(label).small().color(tokens.assistant_accent))
            .fill(tokens.assistant_background)
            .stroke(egui::Stroke::new(1.0, tokens.assistant_accent))
            .corner_radius(13)
            .min_size(egui::vec2(0.0, 27.0)),
    )
}

/// Role labels keep the conversation understandable without depending on bubble colour.
fn message(ui: &mut egui::Ui, role: &str, text: &str, user: bool) {
    let tokens = crate::theme::Tokens::get(ui.ctx());
    egui::Frame::new()
        .fill(if user { tokens.assistant_bubble } else { tokens.assistant_surface })
        .stroke(egui::Stroke::new(1.0, tokens.assistant_border))
        .corner_radius(14)
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width((ui.available_width() - 2.0).max(0.0));
            ui.label(egui::RichText::new(role).small().strong().color(tokens.assistant_accent));
            ui.add_space(2.0);
            ui.label(egui::RichText::new(text).size(14.5));
        });
    ui.add_space(8.0);
}

/// The proposals, oldest first, each with its diffs and buttons. `live` is the open document's
/// identity and revision: a proposal read at another one can't be applied.
fn history(ui: &mut egui::Ui, state: &mut State, live: (u64, u64), action: &mut Action) {
    let busy = state.pending.is_some();
    if !state.history.iter().any(|p| !p.dismissed) && !busy {
        welcome(ui, state);
    }
    for proposal in state.history.iter().rev().filter(|p| !p.dismissed) {
        ui.push_id(proposal.id, |ui| {
            message(ui, tl!("You"), &proposal.instruction, true);
            if !proposal.response.explanation.is_empty() {
                message(ui, tl!("Claude"), &proposal.response.explanation, false);
            }
            ui.collapsing(tl!("Review changes"), |ui| {
                ui.strong(format!("{} #{}", tl!("Proposal"), proposal.id));
                let same_doc = proposal.context.get("document_id").and_then(Value::as_u64) == Some(live.0);
                let fresh = same_doc && proposal.context.get("revision").and_then(Value::as_u64) == Some(live.1);
                if !fresh {
                    ui.label(tl!("The document changed. Regenerate before applying this proposal."));
                }
                for suggestion in &proposal.response.suggestions {
                    let original = proposal.originals.get(suggestion.index).map(String::as_str).unwrap_or("");
                    draw_diff(ui, original, &suggestion.text, suggestion.markdown);
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
                    if ui.add_enabled(!busy, egui::Button::new(tl!("Regenerate"))).clicked() {
                        *action = Some(("assistant.suggest", request_params(state, &proposal.instruction, proposal.explain_only)));
                    }
                    let can_undo = same_doc && proposal.undo_revision == Some(live.1);
                    if ui.add_enabled(can_undo, egui::Button::new(tl!("Undo last application"))).clicked() {
                        *action = Some(("edit.undo", json!({})));
                    }
                });
            });
            ui.separator();
        });
    }
}

fn request_params(state: &State, instruction: &str, explain: bool) -> Value {
    let mut params = json!({"instruction":instruction,"scope":state.scope,"confirm_document":state.warned,
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
                    suggestions: vec![wordcraft_assistant::Suggestion { index: 0, text: "A short sentence.".into(), markdown: false }],
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
    fn markdown_proposal_applies_formatted_blocks_and_sends_structure() {
        let mut app = WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::from_text("Notes\nfirst, second")), Default::default());
        let context = app.session.run("assistant.context", &json!({"scope":"document","confirm_document":true})).unwrap();
        assert_eq!(context["targets"][0]["structure"], "paragraph");
        let (sender, receiver) = std::sync::mpsc::channel();
        app.assistant.pending = Some(Pending {
            cancel: Arc::new(AtomicBool::new(false)),
            receiver,
            context: Some(context),
            instruction: "Structure".into(),
            explain_only: false,
        });
        sender
            .send(Answer::Suggestion(Ok((
                AuthState::Account,
                Response {
                    suggestions: vec![
                        wordcraft_assistant::Suggestion { index: 0, text: "# Notes".into(), markdown: true },
                        wordcraft_assistant::Suggestion { index: 1, text: "- first\n- **second**".into(), markdown: true },
                    ],
                    explanation: "Heading and list.".into(),
                },
            ))))
            .unwrap();
        poll(&mut app);
        app.execute("assistant.accept", json!({"id":1})).unwrap();
        assert_eq!(app.session.doc.plain_text(wordcraft_doc::StoryRef::Body), "Notes\nfirst\nsecond");
        let heading = app.session.doc.para(wordcraft_doc::StoryRef::Body, &wordcraft_doc::Path::top(0)).and_then(|p| p.props.style.clone());
        assert_eq!(heading.as_deref(), Some("Heading1"));
        app.execute("edit.undo", json!({})).unwrap();
        assert_eq!(app.session.doc.plain_text(wordcraft_doc::StoryRef::Body), "Notes\nfirst, second");
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
                        wordcraft_assistant::Suggestion { index: 0, text: "First line.".into(), markdown: false },
                        wordcraft_assistant::Suggestion { index: 1, text: "Second line.".into(), markdown: false },
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
                        wordcraft_assistant::Suggestion { index: 0, text: "First line.".into(), markdown: false },
                        wordcraft_assistant::Suggestion { index: 1, text: "Second line.".into(), markdown: false },
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
        let context = app.session.run("assistant.context", &json!({"scope":"auto","confirm_document":true})).unwrap();
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
        harness.get_by_label("Review changes").click();
        harness.run_steps(2);
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
    fn docked_panel_keeps_composer_and_options_available_without_sending() {
        use egui_kittest::kittest::Queryable;
        let mut app = reviewed_app();
        app.assistant.model.clear();
        app.assistant.instruction = "A longer message.\n".repeat(80);
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1100.0, 1000.0)).build_ui_state(
            |ui, app: &mut WordApp| {
                app.logic(ui.ctx());
                app.ui(ui);
            },
            app,
        );
        h.run_steps(6);
        assert!(h.query_by_label("You").is_some());
        assert!(h.query_by_label("Mirai").is_some());
        assert!(h.query_by_label("Claude Code · consult the current document without applying changes automatically").is_none());
        assert!(h.query_by_label("Reads a copy of the open document to answer. It does not modify your text.").is_some());
        assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "New conversation").is_some());
        assert!(h.query_by_label("Press Enter to send · Shift+Enter for a new line").is_some());
        assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "Send").is_some());
        h.get_by_label("Writing options").click();
        h.run_steps(3);
        h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Model").click();
        h.run_steps(3);
        h.get_by_label("Haiku").click();
        h.run_steps(3);
        assert_eq!(h.state().assistant.model, "haiku");
        assert_eq!(h.state().assistant.instruction, "A longer message.\n".repeat(80));
        assert!(h.state().assistant.pending.is_none(), "model selection never starts Claude Code");
        assert_eq!(request_params(&h.state().assistant, "Improve", false)["model"], "haiku");
        assert_eq!(h.state().session.doc.plain_text(wordcraft_doc::StoryRef::Body), "A very long sentence.\nKeep this paragraph.");
    }

    #[test]
    fn welcome_chips_only_fill_and_focus_the_draft() {
        use egui_kittest::kittest::Queryable;
        let mut app = WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::from_text("Current document.")), Default::default());
        app.assistant.open = true;
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(900.0, 760.0)).build_ui_state(
            |ui, app: &mut WordApp| {
                app.logic(ui.ctx());
                app.ui(ui);
            },
            app,
        );
        h.run_steps(5);
        for label in ["How can I help with your document?", "Summarize main ideas", "Review clarity", "Suggest next steps"] {
            assert!(h.query_by_label(label).is_some(), "missing {label}");
        }
        assert!(h.query_by_label("Sign in").is_some());
        assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "Check connection").is_some());
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Review clarity").click();
        h.run_steps(4);
        assert_eq!(h.state().assistant.instruction, "Review the document's clarity and point out parts that could be expressed more clearly.");
        assert!(h.state().assistant.pending.is_none(), "a prompt chip only fills the draft");
        assert_eq!(h.state().session.doc.plain_text(wordcraft_doc::StoryRef::Body), "Current document.");
    }

    #[test]
    fn disconnected_account_access_shows_official_login_instructions_without_authenticating() {
        use egui_kittest::kittest::Queryable;
        let mut app = WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::new()), Default::default());
        app.assistant.open = true;
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(900.0, 760.0)).build_ui_state(
            |ui, app: &mut WordApp| {
                app.logic(ui.ctx());
                app.ui(ui);
            },
            app,
        );
        h.run_steps(5);
        assert!(h.query_by_label("Sign in").is_some());
        assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "Check connection").is_some());
        assert!(h.query_by_label("Writing options").is_some());
        h.get_by_label("Sign in").click();
        h.run_steps(3);
        assert!(h.query_by_label("claude auth login").is_some());
        assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "Copy command").is_some());
        assert!(h.query_by_label("Official authentication instructions").is_some());
        assert!(h.query_by_label("WordCraft does not start login or handle your credentials.").is_some());
        assert!(h.query_by_label("Check connection only checks the current Claude Code login; it does not sign in.").is_some());
        assert!(h.state().assistant.pending.is_none());
        assert!(h.state().assistant.connection.is_none());
    }

    #[test]
    fn connected_account_access_offers_official_sign_out_and_sign_in_without_running_them() {
        use egui_kittest::kittest::Queryable;
        let mut app = WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::new()), Default::default());
        app.assistant.open = true;
        app.assistant.connection = Some(AuthState::Account);
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(900.0, 760.0)).build_ui_state(
            |ui, app: &mut WordApp| {
                app.logic(ui.ctx());
                app.ui(ui);
            },
            app,
        );
        h.run_steps(5);
        assert!(h.query_by_label("Change account").is_some());
        h.get_by_label("Change account").click();
        h.run_steps(3);
        assert!(h.query_by_label("claude auth logout").is_some());
        assert!(h.query_by_label("claude auth login").is_some());
        assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "Copy sign-out command").is_some());
        assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "Copy sign-in command").is_some());
        assert!(h.query_by_label("Official authentication instructions").is_some());
        assert!(h.state().assistant.pending.is_none());
        assert_eq!(h.state().assistant.connection, Some(AuthState::Account));
    }

    #[test]
    fn new_conversation_clears_local_state_but_keeps_auth_and_document() {
        use egui_kittest::kittest::Queryable;
        let mut app = reviewed_app();
        app.assistant.connection = Some(AuthState::Account);
        app.assistant.connection_error = Some("temporary connection error".into());
        app.assistant.message = "temporary connection error".into();
        app.assistant.instruction = "Keep a draft".into();
        app.assistant.warned = true;
        let document = app.session.doc.plain_text(wordcraft_doc::StoryRef::Body);
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(900.0, 760.0)).build_ui_state(
            |ui, app: &mut WordApp| {
                app.logic(ui.ctx());
                app.ui(ui);
            },
            app,
        );
        h.run_steps(4);
        assert!(h.query_all_by_label("Check connection").next().is_some());
        h.get_by_role_and_label(egui::accesskit::Role::Button, "New conversation").click();
        h.run_steps(3);
        assert!(h.state().assistant.instruction.is_empty());
        assert!(!h.state().assistant.warned);
        assert!(h.state().assistant.history.is_empty());
        assert!(h.state().assistant.message.is_empty());
        assert!(h.state().assistant.connection_error.is_none());
        assert_eq!(h.state().assistant.connection, Some(AuthState::Account));
        assert_eq!(h.state().session.doc.plain_text(wordcraft_doc::StoryRef::Body), document);
    }

    #[test]
    fn new_conversation_is_refused_while_a_request_is_pending() {
        let mut app = reviewed_app();
        let (sender, receiver) = std::sync::mpsc::channel();
        app.assistant.pending =
            Some(Pending { cancel: Arc::new(AtomicBool::new(false)), receiver, context: None, instruction: String::new(), explain_only: false });
        assert!(app.execute("ui.assistant.newConversation", json!({})).is_err());
        assert_eq!(app.assistant.history.len(), 1);
        assert!(app.assistant.pending.is_some());
        drop(sender);
    }

    #[test]
    fn enter_send_respects_shift_and_ime_composition() {
        assert!(should_send_on_enter(egui::Modifiers::NONE, false, false));
        assert!(should_send_on_enter(egui::Modifiers::COMMAND, false, false));
        assert!(should_send_on_enter(egui::Modifiers::CTRL, false, false));
        assert!(!should_send_on_enter(egui::Modifiers::SHIFT, false, false));
        assert!(!should_send_on_enter(egui::Modifiers::NONE, true, false));
        assert!(!should_send_on_enter(egui::Modifiers::NONE, false, true));

        let mut state = State::default();
        assert!(update_ime_composition(&mut state, &[egui::Event::Ime(egui::ImeEvent::Preedit { text: "に".into(), active_range_chars: None })]));
        assert!(state.ime_composing);
        assert!(!should_send_on_enter(egui::Modifiers::NONE, state.ime_composing, false));
        assert!(update_ime_composition(&mut state, &[egui::Event::Ime(egui::ImeEvent::Commit("に".into()))]));
        assert!(!state.ime_composing);
    }

    #[test]
    fn ime_preedit_and_commit_in_one_frame_suppress_enter() {
        let mut state = State::default();
        let events = [
            egui::Event::Ime(egui::ImeEvent::Preedit { text: "a".into(), active_range_chars: None }),
            egui::Event::Ime(egui::ImeEvent::Commit("a".into())),
        ];
        assert!(update_ime_composition(&mut state, &events));
        assert!(!state.ime_composing);
        assert!(!should_send_on_enter(egui::Modifiers::NONE, state.ime_composing, true));
    }

    #[test]
    fn composer_warns_once_for_the_whole_document_and_keeps_the_custom_model() {
        let mut app = reviewed_app();
        let state = &mut app.assistant;
        state.model = "claude-custom-model".into();
        state.scope = "document".into();
        assert!(sends_whole_document(state, true, false));
        assert_eq!(request_params(state, "Improve", false)["confirm_document"], false);
        state.warned = true;
        assert_eq!(request_params(state, "Improve", false)["confirm_document"], true, "sending after the warning confirms");
        assert_eq!(request_params(state, "Improve", false)["model"], "claude-custom-model");
        state.warned = false;
        state.scope = "auto".into();
        assert!(!sends_whole_document(state, true, true), "auto with a text selection stays scoped to that selection");
        assert!(sends_whole_document(state, false, true), "auto without a selection includes the whole body");
        state.include_document = true;
        assert!(sends_whole_document(state, true, false));
        state.include_document = false;
        state.scope = "section".into();
        assert!(sends_whole_document(state, true, true), "a one-section document's section is the whole body");
        assert!(!sends_whole_document(state, true, false));
        state.scope = "selection".into();
        assert!(!sends_whole_document(state, false, true));
    }

    #[test]
    fn section_count_fast_path_matches_document_sections() {
        use wordcraft_doc::{Document, Path, StoryRef};

        let mut empty = Document::new();
        empty.body.clear();

        let mut one_marker = Document::from_text("first\nsecond\nthird");
        one_marker.para_mut(StoryRef::Body, &Path::top(0)).unwrap().section = Some(Box::default());

        let multiple_paragraphs = Document::from_text("first\nsecond\nthird");

        let mut marker_on_final_paragraph = Document::from_text("first\nlast");
        marker_on_final_paragraph.para_mut(StoryRef::Body, &Path::top(1)).unwrap().section = Some(Box::default());

        let cases = [
            ("empty body", empty, true),
            ("one section marker", one_marker, false),
            ("multiple paragraphs without markers", multiple_paragraphs, true),
            ("marker on final paragraph", marker_on_final_paragraph, false),
        ];
        for (name, doc, expected) in cases {
            let optimized = !doc.body.iter().any(|block| block.as_ref().as_para().and_then(|para| para.section.as_ref()).is_some());
            assert_eq!(optimized, doc.sections().len() <= 1, "fast path diverges for {name}");
            assert_eq!(optimized, expected, "unexpected classification for {name}");
        }
    }

    #[test]
    fn whole_document_request_warns_first_then_sends_on_the_second_enter() {
        use egui_kittest::kittest::Queryable;
        let mut app = reviewed_app();
        app.assistant.open = true;
        app.assistant.scope = "document".into();
        app.assistant.instruction = "Improve".into();
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(900.0, 760.0)).build_ui_state(
            |ui, app: &mut WordApp| {
                app.logic(ui.ctx());
                app.ui(ui);
            },
            app,
        );
        h.run_steps(3);
        assert!(h.query_by_label("I approve sending the whole document for this request").is_none(), "no consent checkbox");
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Send").click();
        h.run_steps(3);
        assert!(h.state().assistant.warned, "the first send only warns");
        assert!(h.state().assistant.pending.is_none(), "nothing was sent");
        assert_eq!(h.state().assistant.instruction, "Improve", "the draft stays");
        assert!(h.query_by_label("The whole document will be sent to Claude. Press Enter or Send again to continue.").is_some());
    }

    #[test]
    fn assistant_artwork_is_embedded_and_reuses_one_texture() {
        let image = image::load_from_memory(include_bytes!("../../../assets/assistant/wordcraft-assistant.png")).unwrap();
        assert_eq!((image.width(), image.height()), (1024, 1024));
        let ctx = egui::Context::default();
        let first = icon_texture(&ctx).unwrap();
        let second = icon_texture(&ctx).unwrap();
        assert_eq!(first.id(), second.id());
        assert_eq!(first.size(), [256, 256]);
    }

    #[test]
    fn mirai_logo_is_embedded_and_reuses_one_texture() {
        let image = egui_extras::image::load_svg_bytes_with_size(
            include_bytes!("../../../assets/assistant/mirai-gorrion.svg"),
            egui::SizeHint::Width(256),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(image.size, [256, 128], "SVG rasterization preserves the 2:1 aspect ratio");
        assert_eq!(image.pixels.first(), Some(&egui::Color32::TRANSPARENT));
        assert!(image.pixels.contains(&egui::Color32::from_rgb(0x00, 0x6B, 0x78)), "preserve the teal SVG fill");
        assert!(image.pixels.contains(&egui::Color32::from_rgb(0x75, 0x41, 0xAE)), "preserve the purple SVG fill");
        let ctx = egui::Context::default();
        let first = logo_texture(&ctx).unwrap();
        let second = logo_texture(&ctx).unwrap();
        assert_eq!(first.id(), second.id());
        assert_eq!(first.size(), [256, 128]);
    }

    #[test]
    fn failed_request_restores_the_message_without_replacing_a_new_draft() {
        for draft in ["", "A new draft"] {
            let mut app = WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::new()), Default::default());
            app.assistant.instruction = draft.into();
            let (sender, receiver) = std::sync::mpsc::channel();
            app.assistant.pending = Some(Pending {
                cancel: Arc::new(AtomicBool::new(false)),
                receiver,
                context: Some(json!({})),
                instruction: "My original message".into(),
                explain_only: false,
            });
            sender.send(Answer::Suggestion(Err("offline".into()))).unwrap();
            poll(&mut app);
            assert_eq!(app.assistant.instruction, if draft.is_empty() { "My original message" } else { draft });
            assert!(app.assistant.history.is_empty());
            assert!(app.assistant.pending.is_none());
        }
    }

    #[test]
    fn quick_actions_do_not_consume_an_unsubmitted_draft() {
        let mut app = reviewed_app();
        app.assistant.instruction = "Keep my draft".into();
        let state = &app.assistant;
        assert!(!is_draft_submission(state, "assistant.suggest", &request_params(state, "Make concise", false)));
        assert!(is_draft_submission(state, "assistant.suggest", &request_params(state, "Keep my draft", false)));
        assert!(is_draft_submission(
            state,
            "assistant.suggest",
            &request_params(state, "Explain the text and suggest improvements. Keep my draft", true),
        ));
        assert!(!is_draft_submission(state, "assistant.auth", &json!({})));
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
