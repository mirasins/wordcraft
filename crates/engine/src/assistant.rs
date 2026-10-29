//! The writing assistant's engine side: a bounded, revision-checked context of paragraph slices
//! and the validated, undoable application of replacements made from it. No network access.
//!
//! A context can be forged, so `apply` checks every target against the document (not only the
//! changed ones) before it changes anything.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use wordcraft_doc::edit::Fragment;
use wordcraft_doc::para::OBJ;
use wordcraft_doc::props::PropChange;
use wordcraft_doc::props::{CharProps, ParaProps};
use wordcraft_doc::styles::{Style, StyleKind};
use wordcraft_doc::{Block, Paragraph, Path, Pos, Revision, RevisionKind, StoryRef};
use wordcraft_formats::model::{Kind, para_kind};

use crate::{CmdError, Selection, Session};

/// Most text the context carries, in bytes of the targets' originals.
pub const MAX_CONTEXT_BYTES: usize = 65_536;
/// Most targets (paragraph slices) one context or one apply may hold.
pub const MAX_TARGETS: usize = 256;
/// The author of the assistant's suggestions, as tracked changes show it.
pub const SUGGESTION_AUTHOR: &str = "Mirai";

/// One paragraph slice the assistant may rewrite.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub start: Pos,
    pub end: Pos,
    pub original: String,
    /// Set by a tracked apply: the paragraph now holds the tracked deletion, so this target's text
    /// is no longer checked and it can't be applied again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub applied: bool,
    /// What the paragraph is (`paragraph`, `heading 2`, `bulleted list item, level 1`…), so the
    /// assistant knows the structure it rewrites. Informational: `apply` doesn't check it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structure: Option<String>,
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

/// One replacement for the target at `index` of a context: plain text (one paragraph, keeping
/// the target's formatting), or Markdown (`markdown: true`) that replaces the whole target with
/// formatted text and possibly several blocks; empty Markdown removes the target.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub index: usize,
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub markdown: bool,
}

/// What `assistant.apply` receives.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyRequest {
    pub context: Context,
    pub changes: Vec<Change>,
    /// Write the changes as suggestions: tracked changes by [`SUGGESTION_AUTHOR`], with
    /// revisions of their own per change, whatever the Track Changes setting.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub suggest: bool,
}

/// Parameters of `assistant.context`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextParams {
    pub scope: String,
    #[serde(default)]
    pub confirm_document: bool,
}

/// What `apply` did: how many paragraphs changed and the context for the next apply (at the
/// revision the command leaves). With Track Changes the applied targets are marked `applied`
/// instead of updated (their paragraphs now hold tracked deletions); the others stay applicable.
#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    pub count: usize,
    pub context: Context,
    /// For suggestions: each change's index and the revisions only it uses, to accept or reject
    /// it alone (`assistant.resolve`).
    pub revisions: Vec<(usize, Vec<u32>)>,
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
    /// A whole paragraph of a document or section scope: empty paragraphs, and paragraphs the
    /// assistant can't rewrite (objects, fields, tracked deletions, hidden text), are left out
    /// instead of refusing the whole request. Their text is never sent.
    fn add_rewritable(&mut self, s: &Session, story: StoryRef, path: &Path) -> Result<(), CmdError> {
        let p = s.doc.para(story, path).ok_or_else(|| fail(format!("no paragraph at {path}")))?;
        if p.text.trim().is_empty() || p.text.contains(OBJ) || hides_text(s, p, path.cell().is_some()) {
            return Ok(());
        }
        let len = p.len();
        self.add(s, story, path, 0, len)
    }

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
        let structure = Some(describe(s, p, path.cell().is_some()));
        self.targets.push(Target {
            start: Pos::new(story, path.clone(), from),
            end: Pos::new(story, path.clone(), to),
            original,
            applied: false,
            structure,
        });
        Ok(())
    }
}

/// What a paragraph is, in the words the assistant's structure list uses.
fn describe(s: &Session, p: &Paragraph, in_table: bool) -> String {
    let (kind, list) = para_kind(&s.doc, p);
    let mut out = match kind {
        Kind::Heading(n) => format!("heading {n}"),
        Kind::Title => "title".to_string(),
        Kind::Quote => "quote".to_string(),
        Kind::Code => "code".to_string(),
        Kind::Rule | Kind::Normal => "paragraph".to_string(),
    };
    if let Some(l) = list {
        let which = if l.ordered { "numbered" } else { "bulleted" };
        out = if out == "paragraph" {
            format!("{which} list item, level {}", l.level + 1)
        } else {
            format!("{out}, {which} list, level {}", l.level + 1)
        };
    }
    if in_table {
        out.push_str(", in a table cell");
    }
    out
}

/// Whether the paragraph's kind is structure Markdown expresses (heading, title, quote, code or a
/// list), so a plain Markdown paragraph in its place means plain body text.
fn is_structural(s: &Session, p: &Paragraph) -> bool {
    let (kind, list) = para_kind(&s.doc, p);
    list.is_some() || !matches!(kind, Kind::Normal | Kind::Rule)
}

/// Build the context for `cp`. Scopes: `auto` (the selection, else the whole body),
/// `selection`, `paragraph`, `section` (the body section holding the caret) and `document`.
/// Whole-body coverage by any scope needs `confirm_document`. Errors instead of truncating.
pub fn context(s: &Session, cp: &ContextParams) -> Result<Context, CmdError> {
    let scope: &str = match cp.scope.as_str() {
        "auto" if s.sel.is_collapsed() => "document",
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
            if from == 0 && to.saturating_add(1) >= s.doc.body.len() && !cp.confirm_document {
                return Err(refuse("this section is the whole document; set \"confirm_document\": true to send it"));
            }
            for path in s.doc.para_paths(StoryRef::Body) {
                let i = path.0.first().copied().unwrap_or(0) as usize;
                if (from..=to).contains(&i) {
                    col.add_rewritable(s, StoryRef::Body, &path)?;
                }
            }
        }
        _ => {
            for path in s.doc.para_paths(StoryRef::Body) {
                col.add_rewritable(s, StoryRef::Body, &path)?;
            }
        }
    }
    // An empty document (or section) still gets the caret's paragraph, so the assistant can write
    // into it.
    if col.targets.is_empty() && matches!(scope, "section" | "document") {
        let path = s.sel.focus.path.clone();
        let len = para_len(s, story, &path)?;
        col.add(s, story, &path, 0, len)?;
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
        if t.applied {
            continue;
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
/// shift another target's offsets. Applied targets are never edited again, so they don't count:
/// a removed paragraph's target may share its path with the next one's.
fn one_target_per_paragraph(ctx: &Context) -> Result<(), CmdError> {
    let mut seen: HashSet<(StoryRef, &Path)> = HashSet::with_capacity(ctx.targets.len());
    for t in ctx.targets.iter().filter(|t| !t.applied) {
        if !seen.insert((t.start.story, &t.start.path)) {
            return Err(refuse(format!("paragraph {} appears in two targets; a context holds one target per paragraph", t.start.path)));
        }
    }
    Ok(())
}

/// One replacement inside a paragraph: `middle` goes in place of `from..to`; `full` is the
/// target's text afterwards. A Markdown edit puts `middle`, read as Markdown, in place of the
/// whole target.
struct Edit {
    idx: usize,
    from: Pos,
    to: Pos,
    middle: String,
    full: String,
    markdown: bool,
}

/// The minimal edits for `changes`, in document order. Refuses no-op changes, duplicated indices,
/// plain text that isn't one paragraph, and control characters or objects in any text.
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
        if ch.markdown {
            if ch.text.chars().any(|c| c == OBJ || (c.is_control() && !matches!(c, '\t' | '\n' | '\r'))) {
                return Err(refuse(format!("change {}: Markdown text can't hold control characters or objects", ch.index)));
            }
            if t.applied {
                return Err(refuse(format!("change {}: the target was already applied", ch.index)));
            }
            out.push(Edit { idx: ch.index, from: t.start.clone(), to: t.end.clone(), middle: ch.text.clone(), full: String::new(), markdown: true });
            continue;
        }
        if ch.text.chars().any(|c| matches!(c, OBJ | '\u{2028}' | '\u{2029}') || (c.is_control() && c != '\t')) {
            return Err(refuse(format!(
                "change {}: text must be one paragraph of plain text (no line breaks, control characters or objects)",
                ch.index
            )));
        }
        if t.applied {
            return Err(refuse(format!("change {}: the target was already applied with Track Changes", ch.index)));
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
            markdown: false,
        });
    }
    if out.is_empty() {
        return Err(refuse("nothing to change"));
    }
    out.sort_by(|a, b| a.from.cmp(&b.from));
    Ok(out)
}

/// Blocks a Markdown edit added (`delta` > 0) or removed in one container of a story: the later
/// blocks of that container move by `delta`.
struct Shift {
    story: StoryRef,
    parent: Vec<u32>,
    at: usize,
    delta: i64,
}

impl Shift {
    /// How far a path read before the edit moves.
    fn of(&self, story: StoryRef, path: &Path) -> i64 {
        let n = self.parent.len();
        let inside = story == self.story && path.0.get(..n) == Some(self.parent.as_slice());
        match path.0.get(n) {
            Some(&i) if inside && i as usize > self.at => self.delta,
            _ => 0,
        }
    }
}

/// `path` with the index at `depth_index` moved by `delta`.
fn moved(path: &Path, depth_index: usize, delta: i64) -> Path {
    let mut p = path.clone();
    if let Some(i) = p.0.get_mut(depth_index) {
        *i = u32::try_from(i64::from(*i).saturating_add(delta).max(0)).unwrap_or(u32::MAX);
    }
    p
}

/// Character formatting new Markdown text starts from: the target's own at its start, without
/// the emphasis, character style, link and revision marks Markdown states for itself.
fn base_char_props(dest: &Paragraph, off: usize) -> CharProps {
    let mut base = dest.props_at(off).clone();
    base.style = None;
    base.bold = None;
    base.italic = None;
    base.underline = None;
    base.strike = None;
    base.link = None;
    base.ins = None;
    base.del = None;
    base.fmt_change = None;
    base
}

/// Fit a Markdown fragment to the target it replaces. Plain paragraphs keep the target's fonts and
/// colours, with the Markdown's emphasis over them; replacing a whole body paragraph they keep its
/// paragraph formatting too. Headings, lists, quotes and code keep the formatting of their styles.
fn blend(frag: &mut Fragment, dest: &Paragraph, dest_structural: bool, off: usize, whole: bool) {
    let base = base_char_props(dest, off);
    for b in &mut frag.blocks {
        let Block::Para(p) = b else { continue };
        let plain = p.props.style.is_none() && p.props.numbering.is_none();
        if plain || !whole {
            for r in &mut p.runs {
                let mut props = base.clone();
                props.overlay(&r.props);
                r.props = props;
            }
        }
        if whole && plain && !dest_structural {
            let mut props: ParaProps = dest.props.clone();
            props.fmt_change = None;
            props.num_change = None;
            p.props = props;
        }
    }
}

/// Remove the target of an empty Markdown change: a whole paragraph goes with its paragraph mark
/// (joined to the paragraph before it, else removed outright), keeping a section break it holds.
fn remove_target(s: &mut Session, e: &Edit, dest: &Paragraph, whole: bool, blocks: usize) -> Result<(), CmdError> {
    let story = e.from.story;
    let path = &e.from.path;
    let track = s.doc.settings.track_changes;
    let prev = path.last().checked_sub(1).map(|i| path.with_last(i)).filter(|p| s.doc.para(story, p).is_some());
    if whole && dest.section.is_none() && blocks > 1 {
        if let Some(prev) = prev {
            let len = para_len(s, story, &prev)?;
            let a = Pos::new(story, prev, len);
            crate::cmd::controls::check_edit(s, &a, &e.to)?;
            s.sel = Selection { anchor: a, focus: e.to.clone() };
            crate::cmd::delete_selection(s)?;
            return Ok(());
        }
        if !track {
            s.doc.remove_block(story, path).map_err(|err| fail(err.to_string()))?;
            s.sel = Selection::caret(Pos::new(story, path.clone(), 0));
            return Ok(());
        }
    }
    s.sel = Selection { anchor: e.from.clone(), focus: e.to.clone() };
    crate::cmd::delete_selection(s)?;
    Ok(())
}

/// Apply one Markdown edit; returns how the blocks after it moved.
fn apply_markdown(s: &mut Session, e: &Edit) -> Result<Option<Shift>, CmdError> {
    let story = e.from.story;
    let path = e.from.path.clone();
    let count = |s: &Session| s.doc.container(story, &path).map_or(0, |b| b.len());
    let before = count(s);
    let dest = s.doc.para(story, &path).ok_or_else(|| fail(format!("no paragraph at {path}")))?.clone();
    let whole = e.from.off == 0 && e.to.off == dest.len();
    let dest_structural = is_structural(s, &dest);
    if e.middle.trim().is_empty() {
        remove_target(s, e, &dest, whole, before)?;
    } else {
        let mut frag = crate::cmd::paste::markdown_fragment(s, &e.middle);
        blend(&mut frag, &dest, dest_structural, e.from.off, whole);
        let first = match frag.blocks.first() {
            Some(Block::Para(p)) => Some(p.props.clone()),
            _ => None,
        };
        s.pending_move = None;
        s.sel = Selection { anchor: e.from.clone(), focus: e.to.clone() };
        let at = crate::cmd::delete_selection(s)?;
        // A tracked deletion stays in the paragraph: the new text goes after it, so the original
        // keeps its paragraph (and its formatting) when the change is rejected.
        let at = if s.doc.settings.track_changes {
            let len = para_len(s, story, &path)?;
            Pos { off: e.to.off.min(len), ..e.to.clone() }
        } else {
            at
        };
        s.sel = Selection::caret(at);
        crate::cmd::paste::insert(s, frag)?;
        // Pasting keeps the paragraph formatting where the first block lands; a whole target
        // takes the Markdown's (a heading, a list item, or plain text again).
        if whole && let Some(props) = first {
            // A tracked style change (a heading, a list) is rejected with the text. It is measured
            // against the target as it was: pasting may already have given it the new style.
            let old = &dest.props;
            let change = if s.doc.settings.track_changes && old.fmt_change.is_none() && old.formatting() != props.formatting() {
                Some(PropChange::boxed(crate::cmd::new_revision(s, RevisionKind::Format), old.formatting()))
            } else {
                None
            };
            if let Ok(p) = s.doc.para_mut(story, &path) {
                p.props = props;
                if let Some(change) = change {
                    p.props.fmt_change = change;
                }
            }
        }
    }
    let delta = i64::try_from(count(s)).unwrap_or(i64::MAX).saturating_sub(i64::try_from(before).unwrap_or(i64::MAX));
    Ok((delta != 0).then(|| Shift { story, parent: path.parent().to_vec(), at: path.last(), delta }))
}

/// New insertion, deletion and formatting revisions by the session's author, so the next change
/// uses its own (`new_revision` reuses the latest matching one).
fn fresh_revisions(s: &mut Session) {
    let date = crate::cmd::now_iso();
    for kind in [RevisionKind::Insert, RevisionKind::Delete, RevisionKind::Format] {
        s.doc.revisions.push(Revision::new(kind, s.author.clone(), date.clone()));
    }
}

/// Apply `req` as one undoable step (the caller's `Session::run` checkpoint). Nothing changes
/// unless every target validates and every change plans. Suggestions are tracked changes by
/// [`SUGGESTION_AUTHOR`]; the Track Changes setting and the author are restored afterwards.
pub fn apply(s: &mut Session, req: &ApplyRequest) -> Result<Applied, CmdError> {
    if !req.suggest {
        return apply_edits(s, req);
    }
    let track = std::mem::replace(&mut s.doc.settings.track_changes, true);
    let author = std::mem::replace(&mut s.author, SUGGESTION_AUTHOR.to_string());
    let out = apply_edits(s, req);
    s.doc.settings.track_changes = track;
    s.author = author;
    out
}

fn apply_edits(s: &mut Session, req: &ApplyRequest) -> Result<Applied, CmdError> {
    check_context(s, &req.context)?;
    let edits = plan(&req.context, &req.changes)?;
    let track = s.doc.settings.track_changes;
    let mut shifts = Vec::new();
    let mut revisions = Vec::new();
    // Last paragraph first, so earlier offsets and paths stay valid. `paste_text` takes its
    // formatting from the first replaced character, so the selection is the replaced text.
    for e in edits.iter().rev() {
        crate::cmd::controls::check_edit(s, &e.from, &e.to)?;
        let first_rev = s.doc.revisions.len();
        if req.suggest {
            fresh_revisions(s);
        }
        let own = |s: &Session| (first_rev..s.doc.revisions.len()).filter_map(|i| u32::try_from(i).ok()).collect::<Vec<u32>>();
        if e.markdown {
            shifts.extend(apply_markdown(s, e)?);
            if req.suggest {
                revisions.push((e.idx, own(s)));
            }
            continue;
        }
        let before = para_len(s, e.from.story, &e.from.path)?;
        s.sel = Selection { anchor: e.from.clone(), focus: e.to.clone() };
        if s.doc.settings.track_changes && e.from.off < e.to.off {
            // As a suggestion reads: the struck-through original first, then the new text.
            crate::cmd::delete_selection(s)?;
            let len = para_len(s, e.from.story, &e.from.path)?;
            s.sel = Selection::caret(Pos { off: e.to.off.min(len), ..e.to.clone() });
        }
        if !e.middle.is_empty() || s.sel.anchor != s.sel.focus {
            crate::cmd::paste_text(s, &e.middle)?;
        }
        if !track {
            let after = para_len(s, e.from.story, &e.from.path)?;
            let landed = s.doc.para_at(&e.from).and_then(|p| p.text.get(e.from.off..e.from.off + e.middle.len())).is_some_and(|t| t == e.middle);
            let removed = e.to.off.saturating_sub(e.from.off);
            if !landed || after + removed != before + e.middle.len() {
                return Err(fail("the replacement didn't land as planned; nothing was changed"));
            }
        }
        if req.suggest {
            revisions.push((e.idx, own(s)));
        }
    }
    revisions.reverse();
    let count = edits.len();
    let mut updated = req.context.clone();
    for e in &edits {
        if let Some(t) = updated.targets.get_mut(e.idx) {
            if track || e.markdown {
                // The deleted text stays in the paragraph, or the target became other blocks:
                // the offsets no longer describe it.
                t.applied = true;
                continue;
            }
            t.end = Pos { off: t.start.off.saturating_add(e.full.len()), ..t.start.clone() };
            t.original = e.full.clone();
        }
    }
    // Blocks added or removed by Markdown edits move the targets after them. Every shift is
    // measured against the paths as they were read.
    for t in &mut updated.targets {
        let read = t.start.path.clone();
        for sh in &shifts {
            let delta = sh.of(t.start.story, &read);
            if delta != 0 {
                t.start.path = moved(&t.start.path, sh.parent.len(), delta);
                t.end.path = moved(&t.end.path, sh.parent.len(), delta);
            }
        }
    }
    updated.revision = s.rev().saturating_add(1);
    Ok(Applied { count, context: updated, revisions })
}
