//! The writing assistant's context and apply: scopes, consent, stale and forged contexts, bounds,
//! no-op refusal, repeated applies with the updated context, Track Changes and undo.

use serde_json::{Value, json};
use wordcraft_doc::para::OBJ;
use wordcraft_doc::props::CharProps;
use wordcraft_doc::styles::{Style, StyleKind};
use wordcraft_doc::{Document, Path, Pos, StoryRef};

use crate::assistant::{MAX_CONTEXT_BYTES, MAX_TARGETS};
use crate::{Selection, Session};

fn session(text: &str) -> Session {
    Session::new(Document::from_text(text))
}

fn select(s: &mut Session, a: Pos, b: Pos) {
    s.sel = Selection { anchor: a, focus: b };
}

fn ctx(s: &mut Session, scope: &str, confirm: bool) -> Result<Value, crate::CmdError> {
    s.run("assistant.context", &json!({"scope": scope, "confirm_document": confirm}))
}

fn apply_req(context: &Value, changes: Value) -> Value {
    json!({"context": context, "changes": changes})
}

fn body(s: &Session) -> String {
    s.doc.plain_text(StoryRef::Body)
}

#[test]
fn auto_without_selection_uses_the_whole_body_with_explicit_consent() {
    let mut s = session("one\ntwo\nthree");
    s.sel = Selection::caret(Pos::body(1, 1));
    assert!(ctx(&mut s, "auto", false).is_err());
    let v = ctx(&mut s, "auto", true).unwrap();
    assert_eq!(v["scope"], "document");
    assert_eq!(v["targets"].as_array().map(Vec::len), Some(3));
    assert_eq!(v["targets"][0]["original"], "one");
    assert_eq!(v["targets"][1]["original"], "two");
    assert_eq!(v["targets"][2]["original"], "three");
    assert_eq!(v["revision"], s.rev());
    assert_eq!(v["document_id"], s.document_id());
}

#[test]
fn selection_cuts_first_and_last_paragraph() {
    let mut s = session("alpha\nbeta\ngamma");
    select(&mut s, Pos::body(0, 2), Pos::body(2, 3));
    let v = ctx(&mut s, "auto", false).unwrap();
    assert_eq!(v["scope"], "selection");
    let originals: Vec<&str> = v["targets"].as_array().map(|t| t.iter().filter_map(|x| x["original"].as_str()).collect()).unwrap_or_default();
    assert_eq!(originals, ["pha", "beta", "gam"]);
    assert_eq!(v["targets"][0]["start"]["off"], 2);
    assert_eq!(v["targets"][0]["end"]["off"], 5);
    assert_eq!(v["targets"][1]["start"]["off"], 0);
    assert_eq!(v["targets"][1]["end"]["off"], 4);
    assert_eq!(v["targets"][2]["start"]["off"], 0);
    assert_eq!(v["targets"][2]["end"]["off"], 3);
}

#[test]
fn explicit_paragraph_scope_stays_at_the_caret_paragraph() {
    let mut s = session("one\ntwo\nthree");
    s.sel = Selection::caret(Pos::body(1, 1));
    let v = ctx(&mut s, "paragraph", false).unwrap();
    assert_eq!(v["scope"], "paragraph");
    assert_eq!(v["targets"].as_array().map(Vec::len), Some(1));
    assert_eq!(v["targets"][0]["original"], "two");
}

#[test]
fn selection_scope_rejects_collapsed_caret() {
    let mut s = session("one\ntwo");
    s.sel = Selection::caret(Pos::body(0, 1));
    assert!(ctx(&mut s, "selection", false).is_err());
}

#[test]
fn document_needs_explicit_consent() {
    let mut s = session("one\ntwo");
    assert!(ctx(&mut s, "document", false).is_err());
    let v = ctx(&mut s, "document", true).unwrap();
    assert_eq!(v["targets"].as_array().map(Vec::len), Some(2));
}

#[test]
fn whole_document_coverage_needs_confirmation() {
    // A single paragraph is the whole document, whatever the scope says.
    let mut s = session("one");
    assert!(ctx(&mut s, "paragraph", false).is_err());
    assert!(ctx(&mut s, "auto", false).is_err());
    assert!(ctx(&mut s, "paragraph", true).is_ok());
    // One section holding every paragraph is the whole document too.
    let mut s = session("one\ntwo");
    assert!(ctx(&mut s, "section", false).is_err());
    assert!(ctx(&mut s, "section", true).is_ok());
    // A selection covering the whole body is refused without consent as well.
    let mut s = session("one\ntwo");
    select(&mut s, Pos::body(0, 0), Pos::body(1, 3));
    assert!(ctx(&mut s, "auto", false).is_err());
}

#[test]
fn unknown_scope_and_bad_params_are_errors() {
    let mut s = session("one\ntwo");
    assert!(ctx(&mut s, "everything", true).is_err());
    assert!(s.run("assistant.context", &json!({"scope": 3})).is_err());
    assert!(s.run("assistant.context", &json!({"scope": "paragraph", "bogus": 1})).is_err());
}

#[test]
fn unicode_offsets_stay_on_boundaries() {
    let mut s = session("héllo wörld");
    select(&mut s, Pos::body(0, 1), Pos::body(0, 3));
    let v = ctx(&mut s, "selection", false).unwrap();
    assert_eq!(v["targets"][0]["original"], "é");
}

#[test]
fn too_many_paragraphs_is_refused_not_truncated() {
    let text = vec!["x"; MAX_TARGETS + 1].join("\n");
    let mut s = session(&text);
    assert!(ctx(&mut s, "document", true).is_err());
    assert!(ctx(&mut s, "auto", true).is_err());
}

#[test]
fn oversized_paragraph_is_refused_before_copying() {
    let mut s = session(&"a".repeat(MAX_CONTEXT_BYTES + 1));
    assert!(ctx(&mut s, "paragraph", true).is_err());
}

#[test]
fn header_paragraph_context_works() {
    let mut s = session("body");
    let part = 7;
    let para = wordcraft_doc::Paragraph::with_text("head", Default::default());
    s.doc.parts.insert(
        part,
        wordcraft_doc::Part { kind: wordcraft_doc::PartKind::Header, blocks: vec![wordcraft_doc::para_block(para)], body: Default::default() },
    );
    s.sel = Selection::caret(Pos { story: StoryRef::Part(part), path: Path::top(0), off: 0 });
    let v = ctx(&mut s, "paragraph", false).unwrap();
    assert_eq!(v["targets"][0]["original"], "head");
}

#[test]
fn apply_changes_several_paragraphs_and_undoes_in_one_step() {
    let mut s = session("hello world\nsecond");
    let c = ctx(&mut s, "document", true).unwrap();
    let req = apply_req(&c, json!([{"index": 0, "text": "hello there world"}, {"index": 1, "text": "2nd"}]));
    let v = s.run("assistant.apply", &req).unwrap();
    assert_eq!(v["applied"], 2);
    assert_eq!(body(&s), "hello there world\n2nd");
    s.run("edit.undo", &json!({})).unwrap();
    assert_eq!(body(&s), "hello world\nsecond");
}

#[test]
fn stale_revision_is_rejected_and_nothing_changes() {
    let mut s = session("one\ntwo");
    let c = ctx(&mut s, "document", true).unwrap();
    s.run("text.insert", &json!({"text": "x"})).unwrap();
    let before = body(&s);
    assert!(s.run("assistant.apply", &apply_req(&c, json!([{"index": 0, "text": "ONE"}]))).is_err());
    assert_eq!(body(&s), before);
}

#[test]
fn forged_original_is_rejected() {
    let mut s = session("one\ntwo");
    let mut c = ctx(&mut s, "document", true).unwrap();
    c["targets"][0]["original"] = json!("zzz");
    assert!(s.run("assistant.apply", &apply_req(&c, json!([{"index": 1, "text": "TWO"}]))).is_err());
    assert_eq!(body(&s), "one\ntwo");
}

#[test]
fn overlapping_and_duplicate_targets_are_refused() {
    let mut s = session("abcdef");
    let mut c = ctx(&mut s, "paragraph", true).unwrap();
    let whole = c["targets"][0].clone();
    // Overlapping ranges in one paragraph.
    c["targets"] = json!([
        {"start": {"story": "body", "path": [0], "off": 0}, "end": {"story": "body", "path": [0], "off": 4}, "original": "abcd"},
        {"start": {"story": "body", "path": [0], "off": 2}, "end": {"story": "body", "path": [0], "off": 6}, "original": "cdef"},
    ]);
    assert!(s.run("assistant.apply", &apply_req(&c, json!([{"index": 0, "text": "x"}]))).is_err());
    // The same range twice under different indices.
    c["targets"] = json!([whole.clone(), whole]);
    assert!(s.run("assistant.apply", &apply_req(&c, json!([{"index": 0, "text": "x"}, {"index": 1, "text": "y"}]))).is_err());
    assert_eq!(body(&s), "abcdef");
}

#[test]
fn forged_context_with_object_paragraph_is_refused() {
    let mut s = session("abc");
    let c = ctx(&mut s, "paragraph", true).unwrap();
    // Bypass the context: put an object character into the paragraph text.
    s.doc.para_mut(StoryRef::Body, &Path::top(0)).unwrap().text.push(OBJ);
    let forged = json!({
        "document_id": s.document_id(),
        "revision": s.rev(),
        "scope": "paragraph",
        "targets": [{"start": {"story": "body", "path": [0], "off": 0}, "end": {"story": "body", "path": [0], "off": 3}, "original": "abc"}],
    });
    let _ = c;
    assert!(s.run("assistant.apply", &apply_req(&forged, json!([{"index": 0, "text": "x"}]))).is_err());
}

#[test]
fn paragraph_with_object_cannot_be_read() {
    let mut s = session("abc");
    s.doc.para_mut(StoryRef::Body, &Path::top(0)).unwrap().text.push(OBJ);
    assert!(ctx(&mut s, "paragraph", true).is_err());
}

#[test]
fn bad_indices_line_breaks_and_control_chars_are_rejected() {
    let mut s = session("one\ntwo");
    let c = ctx(&mut s, "document", true).unwrap();
    let cases = [
        json!([{"index": 0, "text": "a"}, {"index": 0, "text": "b"}]),
        json!([{"index": 9, "text": "a"}]),
        json!([{"index": 0, "text": "a\nb"}]),
        json!([{"index": 0, "text": "a\u{7}b"}]),
        json!([{"index": 0, "text": "a\u{2029}b"}]),
    ];
    for changes in cases {
        assert!(s.run("assistant.apply", &apply_req(&c, changes)).is_err());
    }
    assert_eq!(body(&s), "one\ntwo");
}

#[test]
fn noop_apply_errors_and_leaves_no_undo_step() {
    let mut s = session("one\ntwo");
    let c = ctx(&mut s, "document", true).unwrap();
    let rev = s.rev();
    assert!(s.run("assistant.apply", &apply_req(&c, json!([{"index": 0, "text": "one"}]))).is_err());
    assert!(s.run("assistant.apply", &apply_req(&c, json!([]))).is_err());
    assert_eq!(s.rev(), rev);
    assert!(!s.undo());
}

#[test]
fn malformed_apply_params_are_errors_not_panics() {
    let mut s = session("one");
    assert!(s.run("assistant.apply", &json!({"changes": 1})).is_err());
    assert!(s.run("assistant.apply", &json!({"context": {}, "changes": []})).is_err());
    assert!(s.run("assistant.apply", &json!(null)).is_err());
}

#[test]
fn repeated_apply_with_updated_context() {
    let mut s = session("one\ntwo");
    let c = ctx(&mut s, "document", true).unwrap();
    let first = s.run("assistant.apply", &apply_req(&c, json!([{"index": 0, "text": "ONE"}]))).unwrap();
    let next = first["context"].clone();
    assert_eq!(next["revision"], s.rev());
    assert_eq!(next["targets"][0]["original"], "ONE");
    let second = s.run("assistant.apply", &apply_req(&next, json!([{"index": 1, "text": "TWO"}]))).unwrap();
    assert_eq!(second["applied"], 1);
    assert_eq!(second["context"]["revision"], s.rev());
    assert_eq!(body(&s), "ONE\nTWO");
    // The old context is stale now.
    assert!(s.run("assistant.apply", &apply_req(&c, json!([{"index": 1, "text": "x"}]))).is_err());
}

#[test]
fn track_changes_keeps_the_other_targets_applicable() {
    let mut s = session("one\ntwo");
    s.doc.settings.track_changes = true;
    let c = ctx(&mut s, "document", true).unwrap();
    let v = s.run("assistant.apply", &apply_req(&c, json!([{"index": 0, "text": "ONE"}]))).unwrap();
    assert_eq!(v["applied"], 1);
    let next = v["context"].clone();
    assert_eq!(next["revision"], json!(s.rev()));
    assert!(s.run("assistant.apply", &apply_req(&next, json!([{"index": 0, "text": "uno"}]))).is_err(), "an applied target is stale");
    let v = s.run("assistant.apply", &apply_req(&next, json!([{"index": 1, "text": "TWO"}]))).unwrap();
    assert_eq!(v["applied"], 1);
}

fn first_para(s: &mut Session) -> &mut wordcraft_doc::Paragraph {
    s.doc.para_mut(StoryRef::Body, &Path::top(0)).unwrap()
}

#[test]
fn tracked_deletion_is_never_sent() {
    let mut s = session("one two");
    first_para(&mut s).format(4, 7, &|c| c.del = Some(1)).unwrap();
    assert!(ctx(&mut s, "paragraph", true).is_err());
}

#[test]
fn hidden_text_is_never_sent() {
    let mut s = session("one two");
    first_para(&mut s).format(4, 7, &|c| c.hidden = Some(true)).unwrap();
    assert!(ctx(&mut s, "paragraph", true).is_err());
}

#[test]
fn deleted_paragraph_mark_is_never_sent() {
    let mut s = session("one");
    first_para(&mut s).mark.del = Some(1);
    assert!(ctx(&mut s, "paragraph", true).is_err());
}

#[test]
fn forged_context_over_a_deletion_is_refused() {
    let mut s = session("one two");
    let c = ctx(&mut s, "paragraph", true).unwrap();
    // The deletion appears after the context was read, without a revision bump.
    first_para(&mut s).format(4, 7, &|c| c.del = Some(1)).unwrap();
    assert!(s.run("assistant.apply", &apply_req(&c, json!([{"index": 0, "text": "x"}]))).is_err());
    assert_eq!(body(&s), "one two");
}

#[test]
fn tracked_insertion_still_applies_to_clean_text() {
    let mut s = session("one");
    s.doc.settings.track_changes = true;
    let c = ctx(&mut s, "paragraph", true).unwrap();
    assert!(s.run("assistant.apply", &apply_req(&c, json!([{"index": 0, "text": "ONE"}]))).is_ok());
}

#[test]
fn line_separators_are_refused() {
    let mut s = session("one\ntwo");
    let c = ctx(&mut s, "document", true).unwrap();
    for text in ["a\u{2028}b", "a\u{2029}b"] {
        assert!(s.run("assistant.apply", &apply_req(&c, json!([{"index": 0, "text": text}]))).is_err());
    }
    assert_eq!(body(&s), "one\ntwo");
}

#[test]
fn more_changes_than_targets_is_refused_before_planning() {
    let mut s = session("one\ntwo");
    let c = ctx(&mut s, "document", true).unwrap();
    let changes: Vec<Value> = (0..1000).map(|_| json!({"index": 0, "text": "x"})).collect();
    assert!(s.run("assistant.apply", &apply_req(&c, Value::Array(changes))).is_err());
    assert_eq!(body(&s), "one\ntwo");
}

#[test]
fn one_target_per_paragraph_is_enforced() {
    let mut s = session("abcdef");
    let mut c = ctx(&mut s, "paragraph", true).unwrap();
    c["targets"] = json!([
        {"start": {"story": "body", "path": [0], "off": 0}, "end": {"story": "body", "path": [0], "off": 2}, "original": "ab"},
        {"start": {"story": "body", "path": [0], "off": 4}, "end": {"story": "body", "path": [0], "off": 6}, "original": "ef"},
    ]);
    assert!(s.run("assistant.apply", &apply_req(&c, json!([{"index": 1, "text": "EF"}]))).is_err());
    assert_eq!(body(&s), "abcdef");
}

#[test]
fn first_selected_character_format_is_kept() {
    let mut s = session("one two");
    first_para(&mut s).format(0, 3, &|c| c.bold = Some(true)).unwrap();
    let c = ctx(&mut s, "paragraph", true).unwrap();
    s.run("assistant.apply", &apply_req(&c, json!([{"index": 0, "text": "ONE two"}]))).unwrap();
    assert_eq!(body(&s), "ONE two");
    let p = s.doc.para_at(&Pos::body(0, 0)).unwrap();
    assert_eq!(p.props_of_char(0).bold, Some(true));
    assert_ne!(p.props_of_char(4).bold, Some(true), "the unchanged suffix keeps its own format");
}

#[test]
fn unchanged_prefix_keeps_its_format() {
    let mut s = session("abc");
    first_para(&mut s).format(0, 3, &|c| c.bold = Some(true)).unwrap();
    let c = ctx(&mut s, "paragraph", true).unwrap();
    s.run("assistant.apply", &apply_req(&c, json!([{"index": 0, "text": "abXc"}]))).unwrap();
    assert_eq!(body(&s), "abXc");
    let p = s.doc.para_at(&Pos::body(0, 0)).unwrap();
    assert_eq!(p.props_of_char(0).bold, Some(true));
    assert_eq!(p.props_of_char(1).bold, Some(true));
}

#[test]
fn section_scope_stays_inside_its_section() {
    let mut s = session("a\nb\nc");
    // A section break after the first paragraph: sections are [a] and [b, c].
    s.doc.para_mut(StoryRef::Body, &Path::top(0)).unwrap().section = Some(Box::default());
    s.sel = Selection::caret(Pos::body(0, 0));
    let first = ctx(&mut s, "section", false).unwrap();
    assert_eq!(first["targets"].as_array().map(Vec::len), Some(1));
    assert_eq!(first["targets"][0]["original"], "a");
    s.sel = Selection::caret(Pos::body(2, 0));
    let second = ctx(&mut s, "section", false).unwrap();
    let originals: Vec<&str> = second["targets"].as_array().map(|t| t.iter().filter_map(|x| x["original"].as_str()).collect()).unwrap_or_default();
    assert_eq!(originals, ["b", "c"]);
}

#[test]
fn malformed_context_is_refused() {
    let mut s = session("one\ntwo");
    let base = ctx(&mut s, "document", true).unwrap();
    let mut missing_end = base.clone();
    if let Some(o) = missing_end["targets"][0].as_object_mut() {
        o.remove("end");
    }
    assert!(s.run("assistant.apply", &apply_req(&missing_end, json!([{"index": 0, "text": "x"}]))).is_err());
    let mut extra = base.clone();
    extra["extra"] = json!(1);
    assert!(s.run("assistant.apply", &apply_req(&extra, json!([{"index": 0, "text": "x"}]))).is_err());
    let mut bad_story = base.clone();
    bad_story["targets"][0]["start"]["story"] = json!("nowhere");
    assert!(s.run("assistant.apply", &apply_req(&bad_story, json!([{"index": 0, "text": "x"}]))).is_err());
    assert_eq!(body(&s), "one\ntwo");
}

fn hidden_style(id: &str, kind: StyleKind, hidden: bool) -> Style {
    Style { id: id.into(), name: id.into(), kind, chr: CharProps { hidden: hidden.then_some(true), ..Default::default() }, ..Default::default() }
}

#[test]
fn text_hidden_by_paragraph_style_is_refused() {
    let mut s = session("one");
    s.doc.styles.styles.push(hidden_style("HidPara", StyleKind::Paragraph, true));
    first_para(&mut s).props.style = Some("HidPara".into());
    assert!(ctx(&mut s, "paragraph", true).is_err());
}

#[test]
fn text_hidden_by_character_style_is_refused() {
    let mut s = session("one two");
    s.doc.styles.styles.push(hidden_style("HidChar", StyleKind::Character, true));
    first_para(&mut s).format(4, 7, &|c| c.style = Some("HidChar".into())).unwrap();
    assert!(ctx(&mut s, "paragraph", true).is_err());
}

#[test]
fn table_paragraph_is_refused_when_a_table_style_hides_text() {
    let mut s = session("one");
    let table = wordcraft_doc::Table::new(1, 1, 200.0);
    s.doc.body.push(std::sync::Arc::new(wordcraft_doc::Block::Table(table)));
    s.sel = Selection::caret(Pos { story: StoryRef::Body, path: Path(vec![1, 0, 0, 0]), off: 0 });
    assert!(ctx(&mut s, "paragraph", false).is_ok());
    s.doc.styles.styles.push(hidden_style("HidTable", StyleKind::Table, true));
    assert!(ctx(&mut s, "paragraph", false).is_err());
}

#[test]
fn locked_document_refuses_apply() {
    let mut s = session("one\ntwo");
    let c = ctx(&mut s, "document", true).unwrap();
    s.doc.settings.protection = Some("readOnly".into());
    assert!(s.run("assistant.apply", &apply_req(&c, json!([{"index": 0, "text": "x"}]))).is_err());
    assert_eq!(body(&s), "one\ntwo");
}

fn style_of(s: &Session, i: usize) -> Option<String> {
    s.doc.para(StoryRef::Body, &Path::top(i)).and_then(|p| p.props.style.clone())
}

#[test]
fn document_scope_leaves_out_empty_and_object_paragraphs() {
    let mut s = session("one\n\nthree\nfour");
    s.doc.para_mut(StoryRef::Body, &Path::top(3)).unwrap().text.push(OBJ);
    let v = ctx(&mut s, "document", true).unwrap();
    let originals: Vec<&str> = v["targets"].as_array().unwrap().iter().filter_map(|t| t["original"].as_str()).collect();
    assert_eq!(originals, ["one", "three"]);
}

#[test]
fn empty_document_gets_the_caret_paragraph() {
    let mut s = session("");
    let v = ctx(&mut s, "document", true).unwrap();
    assert_eq!(v["targets"].as_array().map(Vec::len), Some(1));
    let c = v.clone();
    s.run("assistant.apply", &apply_req(&c, json!([{"index": 0, "text": "# Plan\n\nFirst step.", "markdown": true}]))).unwrap();
    assert_eq!(body(&s), "Plan\nFirst step.");
    assert_eq!(style_of(&s, 0).as_deref(), Some("Heading1"));
}

#[test]
fn targets_describe_their_structure() {
    let mut s = session("Title\nBody");
    s.sel = Selection::caret(Pos::body(0, 0));
    s.run("para.style", &json!({"style": "Heading 2"})).unwrap();
    let v = ctx(&mut s, "document", true).unwrap();
    assert_eq!(v["targets"][0]["structure"], "heading 2");
    assert_eq!(v["targets"][1]["structure"], "paragraph");
}

#[test]
fn markdown_turns_a_paragraph_into_a_heading_and_a_list_in_one_undo_step() {
    let mut s = session("Intro\nTail");
    let c = ctx(&mut s, "document", true).unwrap();
    let r = s.run("assistant.apply", &apply_req(&c, json!([{"index": 0, "text": "# Overview\n\n- **first**\n- second", "markdown": true}]))).unwrap();
    assert_eq!(body(&s), "Overview\nfirst\nsecond\nTail");
    assert_eq!(style_of(&s, 0).as_deref(), Some("Heading1"));
    let item = s.doc.para(StoryRef::Body, &Path::top(1)).unwrap();
    assert!(item.props.numbering.is_some(), "a list item");
    assert_eq!(item.props_at(0).bold, Some(true));
    assert_eq!(style_of(&s, 3), None, "the next paragraph keeps its formatting");
    // The second target moved by the two blocks the first one added, and can still be applied.
    assert_eq!(r["context"]["targets"][1]["start"]["path"], json!([3]));
    assert_eq!(r["context"]["targets"][0]["applied"], true);
    s.run("assistant.apply", &apply_req(&r["context"], json!([{"index": 1, "text": "End"}]))).unwrap();
    assert_eq!(body(&s), "Overview\nfirst\nsecond\nEnd");
    s.run("edit.undo", &json!({})).unwrap();
    s.run("edit.undo", &json!({})).unwrap();
    assert_eq!(body(&s), "Intro\nTail");
    assert_eq!(style_of(&s, 0), None);
}

#[test]
fn markdown_emphasis_keeps_the_paragraph_and_its_fonts() {
    let mut s = session("Quiet words here");
    first_para(&mut s).format(0, 16, &|c| c.font = Some("Georgia".into())).unwrap();
    s.sel = Selection::caret(Pos::body(0, 0));
    s.run("para.style", &json!({"style": "Heading 2"})).unwrap();
    let c = ctx(&mut s, "paragraph", true).unwrap();
    s.run("assistant.apply", &apply_req(&c, json!([{"index": 0, "text": "## Quiet **loud** here", "markdown": true}]))).unwrap();
    assert_eq!(body(&s), "Quiet loud here");
    assert_eq!(style_of(&s, 0).as_deref(), Some("Heading2"));
    let p = s.doc.para(StoryRef::Body, &Path::top(0)).unwrap();
    assert_eq!(p.props_at(7).bold, Some(true));
    assert_eq!(p.props_at(1).bold, None);
}

#[test]
fn plain_markdown_keeps_body_paragraph_formatting_and_fonts() {
    let mut s = session("Plain words");
    first_para(&mut s).format(0, 11, &|c| c.font = Some("Georgia".into())).unwrap();
    first_para(&mut s).props.align = Some(wordcraft_doc::Align::Center);
    let c = ctx(&mut s, "paragraph", true).unwrap();
    s.run("assistant.apply", &apply_req(&c, json!([{"index": 0, "text": "Plain *new* words", "markdown": true}]))).unwrap();
    let p = s.doc.para(StoryRef::Body, &Path::top(0)).unwrap();
    assert_eq!(p.text, "Plain new words");
    assert_eq!(p.props.align, Some(wordcraft_doc::Align::Center));
    assert_eq!(p.props_at(0).font.as_deref(), Some("Georgia"));
    assert_eq!(p.props_at(7).italic, Some(true));
}

#[test]
fn markdown_demotes_a_heading_to_body_text() {
    let mut s = session("Loud");
    s.run("para.style", &json!({"style": "Heading 1"})).unwrap();
    let c = ctx(&mut s, "paragraph", true).unwrap();
    s.run("assistant.apply", &apply_req(&c, json!([{"index": 0, "text": "Quiet", "markdown": true}]))).unwrap();
    assert_eq!(style_of(&s, 0), None);
}

#[test]
fn empty_markdown_merges_and_removes_paragraphs() {
    let mut s = session("one\ntwo\nthree\nfour");
    let c = ctx(&mut s, "document", true).unwrap();
    let r = s
        .run(
            "assistant.apply",
            &apply_req(&c, json!([{"index": 0, "text": "one and two", "markdown": true}, {"index": 1, "text": "", "markdown": true}])),
        )
        .unwrap();
    assert_eq!(body(&s), "one and two\nthree\nfour");
    assert_eq!(r["context"]["targets"][2]["start"]["path"], json!([1]));
    s.run("assistant.apply", &apply_req(&r["context"], json!([{"index": 2, "text": "", "markdown": true}, {"index": 3, "text": "4"}]))).unwrap();
    assert_eq!(body(&s), "one and two\n4");
}

#[test]
fn empty_markdown_removes_the_first_paragraph() {
    let mut s = session("one\ntwo");
    let c = ctx(&mut s, "document", true).unwrap();
    s.run("assistant.apply", &apply_req(&c, json!([{"index": 0, "text": "  ", "markdown": true}]))).unwrap();
    assert_eq!(body(&s), "two");
}

#[test]
fn markdown_in_a_selection_inserts_formatted_text_inline() {
    let mut s = session("Say hello now");
    select(&mut s, Pos::body(0, 4), Pos::body(0, 9));
    let c = ctx(&mut s, "selection", false).unwrap();
    s.run("assistant.apply", &apply_req(&c, json!([{"index": 0, "text": "**hi there**", "markdown": true}]))).unwrap();
    let p = s.doc.para(StoryRef::Body, &Path::top(0)).unwrap();
    assert_eq!(p.text, "Say hi there now");
    assert_eq!(p.props_at(5).bold, Some(true));
    assert_eq!(p.props_at(13).bold, None);
}

#[test]
fn markdown_with_track_changes_is_tracked() {
    let mut s = session("Intro\nTail");
    s.doc.settings.track_changes = true;
    let c = ctx(&mut s, "document", true).unwrap();
    let r = s.run("assistant.apply", &apply_req(&c, json!([{"index": 0, "text": "# New\n\nMore", "markdown": true}]))).unwrap();
    assert!(!s.doc.revisions.is_empty());
    assert_eq!(r["context"]["targets"][0]["applied"], true);
    assert!(s.run("assistant.apply", &apply_req(&r["context"], json!([{"index": 0, "text": "again", "markdown": true}]))).is_err());
}

#[test]
fn markdown_refuses_control_characters_and_objects() {
    let mut s = session("one");
    let c = ctx(&mut s, "paragraph", true).unwrap();
    for bad in ["a\u{0007}b", "a\u{FFFC}b"] {
        assert!(s.run("assistant.apply", &apply_req(&c, json!([{"index": 0, "text": bad, "markdown": true}]))).is_err(), "{bad:?}");
    }
    assert_eq!(body(&s), "one");
}
