//! The writing assistant's engine side: a bounded, revision-checked context of paragraph slices
//! and the validated, undoable application of replacements made from it. No network access.
//!
//! A context can be forged, so `apply` checks every target against the document (not only the
//! changed ones) before it changes anything.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use wordcraft_doc::para::OBJ;
use wordcraft_doc::props::CharProps;
use wordcraft_doc::styles::{Style, StyleKind};
use wordcraft_doc::{Path, Pos, StoryRef};

use crate::{CmdError, Selection, Session};

/// Most text the context carries, in bytes of the targets' originals.
pub const MAX_CONTEXT_BYTES: usize = 65_536;
/// Most targets (paragraph slices) one context or one apply may hold.
pub const MAX_TARGETS: usize = 256;

/// One paragraph slice the assistant may rewrite.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub start: Pos,
    pub end: Pos,
    pub original: String,
}

/// What the assistant was given: the document identity and revision it was read at, the
/// effective scope and the targets.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub document_id: u64,
    pub revision: u64,
    pub scope: String,
    pub targets: Vec<Target>,
}

/// One replacement for the target at `index` of a context.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub index: usize,
    pub text: String,
}

/// What `assistant.apply` receives.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyRequest {
    pub context: Context,
    pub changes: Vec<Change>,
}

/// Parameters of `assistant.context`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextParams {
    pub scope: String,
    #[serde(default)]
    pub confirm_document: bool,
}

/// What `apply` did: how many paragraphs changed and, outside Track Changes, the context for the
/// next apply (at the revision the command leaves). With Track Changes the context is `None`:
/// the caller reads the document again.
#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    pub count: usize,
    pub context: Option<Context>,
}

fn refuse(msg: impl Into<String>) -> CmdError {
    CmdError::Params(msg.into())
}

fn fail(msg: impl Into<String>) -> CmdError {
    CmdError::Failed(msg.into())
}

fn para_len(s: &Session, story: StoryRef, path: &Path) -> Result<usize, CmdError> {
    s.doc.para(story, path).map(|p| p.len()).ok_or_else(|| fail(format!("no paragraph at {path}")))
}

/// Whether a table style may hide text in some cell: its own formatting, or the formatting of any
/// of its conditional parts (header row, bands, first and last columns, total row).
fn table_style_may_hide(st: &Style) -> bool {
    let hid = |c: &CharProps| c.hidden == Some(true);
    if hid(&st.chr) {
        return true;
    }
    let Some(t) = &st.table else { return false };
    [&t.header_chr, &t.band_chr, &t.col_band_chr, &t.first_col_chr, &t.last_col_chr, &t.total_chr].into_iter().any(hid)
        || t.regions.values().any(|f| hid(&f.chr))
}

/// A paragraph whose text may not all be visible: a tracked deletion, a deleted paragraph mark,
/// or text hidden by its own, its paragraph style's or its character style's formatting (through
/// the document defaults). Inside a table, any table style that may hide text counts too.
fn hides_text(s: &Session, p: &wordcraft_doc::Paragraph, in_table: bool) -> bool {
    if p.mark.del.is_some() {
        return true;
    }
    if in_table && s.doc.styles.styles.iter().any(|st| st.kind == StyleKind::Table && table_style_may_hide(st)) {
        return true;
    }
    let style = p.props.style.as_deref();
    p.runs.iter().any(|r| r.props.del.is_some() || s.doc.styles.resolve_char(style, &r.props).hidden)
}

fn hidden_msg(path: &impl std::fmt::Display) -> CmdError {
    fail(format!("paragraph {path} has tracked deletions or hidden text; review or accept the revisions and show the text first"))
}

/// Targets collected so far, with their total bytes. Every budget is checked before the text is
/// copied, so a huge paragraph is refused without allocating for it.
struct Collector {
    targets: Vec<Target>,
    bytes: usize,
}

impl Collector {
    fn add(&mut self, s: &Session, story: StoryRef, path: &Path, from: usize, to: usize) -> Result<(), CmdError> {
        if self.targets.len() >= MAX_TARGETS {
            return Err(refuse(format!("more than {MAX_TARGETS} paragraphs; select a smaller part of the document")));
        }
        let p = s.doc.para(story, path).ok_or_else(|| fail(format!("no paragraph at {path}")))?;
        if p.len() > MAX_CONTEXT_BYTES {
            return Err(refuse(format!("paragraph {path} is longer than {MAX_CONTEXT_BYTES} bytes")));
        }
        if p.text.contains(OBJ) {
            return Err(fail(format!("paragraph {path} holds an object or field; the assistant can't rewrite it")));
        }
        if hides_text(s, p, path.cell().is_some()) {
            return Err(hidden_msg(path));
        }
        if from > to || to > p.len() {
            return Err(fail(format!("paragraph {path}: range out of bounds")));
        }
        let len = to - from;
        if self.bytes.saturating_add(len) > MAX_CONTEXT_BYTES {
            return Err(refuse(format!("more than {MAX_CONTEXT_BYTES} bytes of text; select a smaller part of the document")));
        }
        let original = p.text.get(from..to).ok_or_else(|| fail(format!("paragraph {path}: offsets aren't on character boundaries")))?.to_string();
        self.bytes += len;
        self.targets.push(Target { start: Pos::new(story, path.clone(), from), end: Pos::new(story, path.clone(), to), original });
        Ok(())
    }
}

/// Build the context for `cp`. Scopes: `auto` (the selection, else the caret's paragraph),
/// `selection`, `paragraph`, `section` (the body section holding the caret) and `document`.
/// Whole-body coverage by any scope needs `confirm_document`. Errors instead of truncating.
pub fn context(s: &Session, cp: &ContextParams) -> Result<Context, CmdError> {
    let scope: &str = match cp.scope.as_str() {
        "auto" if s.sel.is_collapsed() => "paragraph",
        "auto" => "selection",
        "selection" | "paragraph" | "section" | "document" => cp.scope.as_str(),
        other => return Err(refuse(format!("unknown `scope` {other:?}: auto, selection, paragraph, section or document"))),
    };
    if scope == "document" && !cp.confirm_document {
        return Err(refuse("the whole document needs \"confirm_document\": true"));
    }
    if s.column_segments().is_some() || s.cell_range().is_some() {
        return Err(fail("the assistant can't use a table cell or column selection"));
    }
    let story = s.sel.focus.story;
    let mut col = Collector { targets: Vec::new(), bytes: 0 };
    match scope {
        "selection" => {
            let (a, b) = s.sel.ordered();
            if a == b {
                return Err(fail("nothing is selected"));
            }
            selection(s, &a, &b, &mut col)?;
        }
        "paragraph" => {
            let path = s.sel.focus.path.clone();
            let len = para_len(s, story, &path)?;
            col.add(s, story, &path, 0, len)?;
        }
        "section" => {
            if story != StoryRef::Body {
                return Err(fail("sections are in the body only"));
            }
            let top = s.sel.focus.path.0.first().copied().unwrap_or(0) as usize;
            let (from, to) = section_range(s, top);
            for path in s.doc.para_paths(StoryRef::Body) {
                let i = path.0.first().copied().unwrap_or(0) as usize;
                if (from..=to).contains(&i) {
                    let len = para_len(s, StoryRef::Body, &path)?;
                    col.add(s, StoryRef::Body, &path, 0, len)?;
                }
            }
        }
        _ => {
            for path in s.doc.para_paths(StoryRef::Body) {
                let len = para_len(s, StoryRef::Body, &path)?;
                col.add(s, StoryRef::Body, &path, 0, len)?;
            }
        }
    }
    if covers_body(s, &col.targets) && !cp.confirm_document {
        return Err(refuse("these paragraphs are the whole document; set \"confirm_document\": true to send it"));
    }
    Ok(Context { document_id: s.document_id(), revision: s.rev(), scope: scope.to_string(), targets: col.targets })
}

/// One target per paragraph of the selection `a..b`, each cut to the selected part. Refused when
/// the selection crosses table cells.
fn selection(s: &Session, a: &Pos, b: &Pos, col: &mut Collector) -> Result<(), CmdError> {
    if a.story != b.story {
        return Err(fail("the selection spans more than one story"));
    }
    let paths = s.doc.para_paths_in(a, b);
    if paths.len() > 1 && paths.iter().any(|p| p.cell().is_some()) {
        return Err(fail("the selection crosses table cells"));
    }
    for path in &paths {
        let len = para_len(s, a.story, path)?;
        let from = if *path == a.path { a.off } else { 0 };
        let to = if *path == b.path { b.off } else { len };
        col.add(s, a.story, path, from, to)?;
    }
    Ok(())
}

/// The top-level body block range of the section containing body block `top`.
fn section_range(s: &Session, top: usize) -> (usize, usize) {
    let mut from = 0;
    for (end, _) in s.doc.sections() {
        if top <= end {
            return (from, end);
        }
        from = end.saturating_add(1);
    }
    (0, s.doc.body.len().saturating_sub(1))
}

/// Whether `targets` hold every paragraph of the body, each whole.
fn covers_body(s: &Session, targets: &[Target]) -> bool {
    let all = s.doc.para_paths(StoryRef::Body);
    if all.is_empty() || all.len() > targets.len() {
        return false;
    }
    let full: HashSet<&Path> = targets
        .iter()
        .filter(|t| {
            t.start.story == StoryRef::Body
                && t.start.path == t.end.path
                && t.start.off == 0
                && s.doc.para_at(&t.start).is_some_and(|p| p.len() == t.end.off)
        })
        .map(|t| &t.start.path)
        .collect();
    all.iter().all(|p| full.contains(p))
}

/// Byte lengths of the longest common char-aligned prefix and suffix of `a` and `b` (the suffix
/// never overlaps the prefix).
fn common_affixes(a: &str, b: &str) -> (usize, usize) {
    let pre: usize = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).map(|(x, _)| x.len_utf8()).sum();
    let (ra, rb) = (&a[pre..], &b[pre..]);
    let suf: usize = ra.chars().rev().zip(rb.chars().rev()).take_while(|(x, y)| x == y).map(|(x, _)| x.len_utf8()).sum();
    (pre, suf)
}

/// Refuse a context whose targets don't match the document: identity, revision, bounds, every
/// target's original, paragraphs holding objects or fields, and overlapping ranges.
fn check_context(s: &Session, ctx: &Context) -> Result<(), CmdError> {
    if ctx.document_id != s.document_id() || ctx.revision != s.rev() {
        return Err(fail("the document changed since the context was read; read it again"));
    }
    if ctx.targets.len() > MAX_TARGETS {
        return Err(refuse(format!("{} paragraphs; the limit is {MAX_TARGETS}", ctx.targets.len())));
    }
    let bytes = ctx.targets.iter().fold(0usize, |n, t| n.saturating_add(t.original.len()));
    if bytes > MAX_CONTEXT_BYTES {
        return Err(refuse(format!("{bytes} bytes of text; the limit is {MAX_CONTEXT_BYTES}")));
    }
    for (i, t) in ctx.targets.iter().enumerate() {
        if t.start.story != t.end.story || t.start.path != t.end.path || t.start.off > t.end.off {
            return Err(refuse(format!("target {i}: bad range")));
        }
        let p = s.doc.para(t.start.story, &t.start.path).ok_or_else(|| fail(format!("target {i}: no such paragraph")))?;
        if p.text.contains(OBJ) {
            return Err(fail(format!("target {i}: paragraph holds an object or field; the assistant can't rewrite it")));
        }
        if hides_text(s, p, t.start.path.cell().is_some()) {
            return Err(hidden_msg(&t.start.path));
        }
        let current = p.text.get(t.start.off..t.end.off).ok_or_else(|| refuse(format!("target {i}: offsets aren't on character boundaries")))?;
        if current != t.original {
            return Err(fail(format!("target {i}: its text changed since the context was read")));
        }
    }
    one_target_per_paragraph(ctx)
}

/// A context holds one target per paragraph (the generator's invariant). Replacements then never
/// shift another target's offsets, and the updated context needs no rebasing.
fn one_target_per_paragraph(ctx: &Context) -> Result<(), CmdError> {
    let mut seen: HashSet<(StoryRef, &Path)> = HashSet::with_capacity(ctx.targets.len());
    for t in &ctx.targets {
        if !seen.insert((t.start.story, &t.start.path)) {
            return Err(refuse(format!("paragraph {} appears in two targets; a context holds one target per paragraph", t.start.path)));
        }
    }
    Ok(())
}

/// One replacement inside a paragraph: `middle` goes in place of `from..to`; `full` is the
/// target's text afterwards.
struct Edit {
    idx: usize,
    from: Pos,
    to: Pos,
    middle: String,
    full: String,
}

/// The minimal edits for `changes`, in document order. Refuses no-op changes, duplicated indices,
/// and text that isn't one paragraph of plain text.
fn plan(ctx: &Context, changes: &[Change]) -> Result<Vec<Edit>, CmdError> {
    let total = changes.iter().fold(0usize, |n, c| n.saturating_add(c.text.len()));
    if total > MAX_CONTEXT_BYTES {
        return Err(refuse(format!("{total} bytes of new text; the limit is {MAX_CONTEXT_BYTES}")));
    }
    // Bounded before any allocation: a request can't hold more changes than it has targets.
    if changes.len() > ctx.targets.len() {
        return Err(refuse(format!("{} changes for {} targets", changes.len(), ctx.targets.len())));
    }
    let mut seen = vec![false; ctx.targets.len()];
    let mut out = Vec::with_capacity(changes.len());
    for ch in changes {
        let t = ctx.targets.get(ch.index).ok_or_else(|| refuse(format!("change index {} is out of range", ch.index)))?;
        let slot = seen.get_mut(ch.index).ok_or_else(|| refuse(format!("change index {} is out of range", ch.index)))?;
        if std::mem::replace(slot, true) {
            return Err(refuse(format!("change index {} appears twice", ch.index)));
        }
        if ch.text.chars().any(|c| matches!(c, OBJ | '\u{2028}' | '\u{2029}') || (c.is_control() && c != '\t')) {
            return Err(refuse(format!(
                "change {}: text must be one paragraph of plain text (no line breaks, control characters or objects)",
                ch.index
            )));
        }
        if ch.text == t.original {
            return Err(refuse(format!("change {}: the text is unchanged", ch.index)));
        }
        let (pre, suf) = common_affixes(&t.original, &ch.text);
        let bad = || fail(format!("change {}: bad range", ch.index));
        let from = t.start.off.checked_add(pre).ok_or_else(bad)?;
        let to = t.end.off.checked_sub(suf).ok_or_else(bad)?;
        let new_end = ch.text.len().checked_sub(suf).ok_or_else(bad)?;
        let middle = ch.text.get(pre..new_end).ok_or_else(bad)?.to_string();
        out.push(Edit {
            idx: ch.index,
            from: Pos { off: from, ..t.start.clone() },
            to: Pos { off: to, ..t.end.clone() },
            middle,
            full: ch.text.clone(),
        });
    }
    if out.is_empty() {
        return Err(refuse("nothing to change"));
    }
    out.sort_by(|a, b| a.from.cmp(&b.from));
    Ok(out)
}

/// Apply `req` as one undoable step (the caller's `Session::run` checkpoint). Nothing changes
/// unless every target validates and every change plans.
pub fn apply(s: &mut Session, req: &ApplyRequest) -> Result<Applied, CmdError> {
    check_context(s, &req.context)?;
    let edits = plan(&req.context, &req.changes)?;
    let track = s.doc.settings.track_changes;
    // Last paragraph first, so earlier offsets in each paragraph stay valid. `paste_text` takes
    // its formatting from the first replaced character, so the selection is the replaced text.
    for e in edits.iter().rev() {
        crate::cmd::controls::check_edit(s, &e.from, &e.to)?;
        let before = para_len(s, e.from.story, &e.from.path)?;
        s.sel = Selection { anchor: e.from.clone(), focus: e.to.clone() };
        crate::cmd::paste_text(s, &e.middle)?;
        if !track {
            let after = para_len(s, e.from.story, &e.from.path)?;
            let landed = s.doc.para_at(&e.from).and_then(|p| p.text.get(e.from.off..e.from.off + e.middle.len())).is_some_and(|t| t == e.middle);
            let removed = e.to.off.saturating_sub(e.from.off);
            if !landed || after + removed != before + e.middle.len() {
                return Err(fail("the replacement didn't land as planned; nothing was changed"));
            }
        }
    }
    let count = edits.len();
    if track {
        return Ok(Applied { count, context: None });
    }
    let mut updated = req.context.clone();
    for e in &edits {
        if let Some(t) = updated.targets.get_mut(e.idx) {
            t.end = Pos { off: t.start.off.saturating_add(e.full.len()), ..t.start.clone() };
            t.original = e.full.clone();
        }
    }
    updated.revision = s.rev().saturating_add(1);
    Ok(Applied { count, context: Some(updated) })
}
