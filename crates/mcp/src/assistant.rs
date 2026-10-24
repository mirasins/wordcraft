//! `assistant.suggest` for the headless backend: a synchronous local run with no window and no
//! status job. The context comes from the engine (`assistant.context`); the model is reached only
//! through `wordcraft_assistant` (Claude Code account, no key fallback). Limits and the prompt for
//! explain-only requests match the desktop app's (`crates/ui-egui/src/assistant.rs`).

use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;

use serde_json::{Value, json};
use wordcraft_assistant::{Client, MAX_EXPLANATION_BYTES, MAX_INSTRUCTION_BYTES, MAX_TEXT_BYTES, Provider, Request, Response};
use wordcraft_engine::Session;

/// The scopes `assistant.context` understands.
pub const SCOPES: [&str; 5] = ["auto", "selection", "paragraph", "section", "document"];
/// Prepended to explain-only instructions (the desktop app sends the same words).
pub const EXPLAIN_PREFIX: &str = "Explain or suggest improvements only. Return no replacement suggestions.";

/// Validated `assistant.suggest` parameters.
#[derive(Debug, PartialEq)]
pub struct SuggestParams {
    pub instruction: String,
    pub scope: String,
    pub confirm_document: bool,
    pub include_document_context: bool,
    pub explain_only: bool,
    /// Claude model alias or id for `--model`; `None` keeps Claude Code's default.
    pub model: Option<String>,
}

/// Checks the parameters with the desktop app's rules: an absent field takes its default, but a
/// present one must have the right type (`null` included). Nothing is read or sent before this passes.
pub fn parse_suggest(p: &Value) -> Result<SuggestParams, String> {
    let instruction = match p.get("instruction") {
        Some(Value::String(s)) => s.clone(),
        Some(_) => return Err("`instruction` must be a string".into()),
        None => return Err("missing `instruction`".into()),
    };
    if instruction.trim().is_empty() || instruction.len() > MAX_INSTRUCTION_BYTES {
        return Err(format!("give a nonempty `instruction` of at most {MAX_INSTRUCTION_BYTES} bytes"));
    }
    let scope = match p.get("scope") {
        None => "auto".to_string(),
        Some(Value::String(s)) if SCOPES.contains(&s.as_str()) => s.clone(),
        Some(_) => return Err(format!("`scope` must be one of {}", SCOPES.join(", "))),
    };
    let flag = |k: &str| match p.get(k) {
        None => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("`{k}` must be a boolean")),
    };
    let confirm_document = flag("confirm_document")?;
    let include_document_context = flag("include_document_context")?;
    let explain_only = flag("explain_only")?;
    if include_document_context && !confirm_document {
        return Err("confirm_document: true is required to send the whole document as context".into());
    }
    let model = match p.get("model") {
        None => None,
        Some(Value::String(m)) => {
            wordcraft_assistant::model_name(Some(m)).map_err(|_| "`model` must be a Claude model alias or id such as sonnet or opus".to_string())?
        }
        Some(_) => return Err("`model` must be a string".into()),
    };
    Ok(SuggestParams { instruction, scope, confirm_document, include_document_context, explain_only, model })
}

/// The `original` text of every target in an `assistant.context` snapshot.
fn originals(snapshot: &Value) -> Result<Vec<String>, String> {
    snapshot
        .get("targets")
        .and_then(Value::as_array)
        .ok_or("invalid assistant context")?
        .iter()
        .map(|t| t.get("original").and_then(Value::as_str).map(str::to_string).ok_or_else(|| "invalid assistant target".to_string()))
        .collect()
}

fn context(session: &mut Session, scope: &str, confirm_document: bool) -> Result<Value, String> {
    session.run("assistant.context", &json!({"scope": scope, "confirm_document": confirm_document})).map_err(|e| e.to_string())
}

/// Checks a model answer against the request: indices in range and unique, bounded strings, and
/// single-paragraph replacement text (what `assistant.apply` accepts).
fn check_response(r: &Response, targets: usize) -> Result<(), String> {
    if r.explanation.len() > MAX_EXPLANATION_BYTES {
        return Err("the assistant's explanation is too long".into());
    }
    let mut seen = BTreeSet::new();
    for s in &r.suggestions {
        if s.index >= targets || !seen.insert(s.index) {
            return Err(format!("the assistant suggested an unknown or repeated target index {}", s.index));
        }
        if s.text.len() > MAX_TEXT_BYTES || s.text.contains(['\r', '\n', '\u{2029}', '\u{000B}']) {
            return Err(format!("the assistant's text for target {} is not one bounded paragraph", s.index));
        }
    }
    Ok(())
}

/// Runs one suggestion: context from the engine, optional whole-document context (joined by a
/// newline, as in the desktop app), then `generate` with the model's answer as JSON. Returns
/// `{context, suggestions, explanation}`. Does not change the document.
pub fn suggest(session: &mut Session, params: &Value, generate: impl FnOnce(&Request) -> Result<Value, String>) -> Result<Value, String> {
    let p = parse_suggest(params)?;
    let snapshot = context(session, &p.scope, p.confirm_document)?;
    let targets = originals(&snapshot)?;
    let document = if p.include_document_context {
        let whole = context(session, "document", true)?;
        Some(originals(&whole)?.join("\n"))
    } else {
        None
    };
    let instruction = if p.explain_only { format!("{EXPLAIN_PREFIX} {}", p.instruction) } else { p.instruction };
    let request = Request { instruction, targets, context: document };
    request.validate().map_err(|e| e.to_string())?;
    let reply = generate(&request)?;
    let response: Response = serde_json::from_value(reply).map_err(|_| "the assistant returned an invalid answer".to_string())?;
    check_response(&response, request.targets.len())?;
    // A suggestion that repeats its original would make `assistant.apply` refuse the whole batch
    // ("the text is unchanged"); drop it, as the desktop window does.
    let mut suggestions = if p.explain_only { Vec::new() } else { response.suggestions };
    suggestions.retain(|s| request.targets.get(s.index).is_some_and(|original| *original != s.text));
    Ok(json!({"context": snapshot, "suggestions": suggestions, "explanation": response.explanation}))
}

/// Production generator: the local Claude Code account only, with `model` (`None`: Claude Code's
/// default). Errors are messages without tokens.
pub fn generate_locally(request: &Request, model: Option<&str>) -> Result<Value, String> {
    let client = Client::discover().and_then(|c| c.with_model(model)).map_err(|e| e.to_string())?;
    let cancel = AtomicBool::new(false);
    client.auth(&cancel).map_err(|e| e.to_string())?;
    let reply = client.generate(request, &cancel).map_err(|e| e.to_string())?;
    serde_json::to_value(&reply).map_err(|e| e.to_string())
}

/// Headless metadata for `engine.commands`: `assistant.suggest` runs outside the registry.
pub fn command_metadata() -> Value {
    json!({
        "id": "assistant.suggest",
        "label": "Request writing suggestions",
        "location": "Review › Mirai Assistant",
        "shortcut": null,
        "params": "instruction: string; scope?: auto|selection|paragraph|section|document; confirm_document?: bool; include_document_context?: bool; explain_only?: bool; model?: string (Claude alias or id, e.g. sonnet, opus). Sends the chosen text to Claude; synchronous in the headless backend.",
        "mutates": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Backend, Headless};
    use wordcraft_doc::Document;

    fn headless(text: &str) -> Headless {
        Headless { session: Session::new(Document::from_text(text)), ..Default::default() }
    }

    fn run(h: &mut Headless, command: &str, params: Value) -> Value {
        h.call("engine.execute", json!({"command": command, "params": params})).unwrap()
    }

    fn text_of(h: &mut Headless) -> String {
        run(h, "document.text", json!({}))["text"].as_str().unwrap().to_string()
    }

    /// A stand-in provider: checks the request it receives and answers with `reply`.
    fn provider(reply: Value) -> impl FnOnce(&Request) -> Result<Value, String> {
        move |_| Ok(reply)
    }

    fn explain_reply() -> Value {
        json!({"suggestions": [{"index": 0, "text": "Fast."}], "explanation": "Short and clear."})
    }

    #[test]
    fn unchanged_suggestions_are_dropped_so_apply_succeeds() {
        let mut h = headless("Alpha is slow.\nBeta is fine.");
        let reply = json!({"suggestions": [{"index": 0, "text": "Alpha is fast."}, {"index": 1, "text": "Beta is fine."}], "explanation": ""});
        let out =
            suggest(&mut h.session, &json!({"instruction": "Improve", "scope": "document", "confirm_document": true}), provider(reply)).unwrap();
        assert_eq!(out["suggestions"], json!([{"index": 0, "text": "Alpha is fast."}]));
        let applied = h
            .call("engine.execute", json!({"command": "assistant.apply", "params": {"context": out["context"], "changes": out["suggestions"]}}))
            .unwrap();
        assert_eq!(applied["applied"], 1);
        assert_eq!(text_of(&mut h), "Alpha is fast.\nBeta is fine.");
    }

    #[test]
    fn parse_defaults_and_strict_types() {
        let ok = parse_suggest(&json!({"instruction": "  Shorten  "})).unwrap();
        assert_eq!(
            ok,
            SuggestParams {
                instruction: "  Shorten  ".into(),
                scope: "auto".into(),
                confirm_document: false,
                include_document_context: false,
                explain_only: false,
                model: None
            }
        );
        assert_eq!(parse_suggest(&json!({"instruction": "x", "model": "opus"})).unwrap().model.as_deref(), Some("opus"));
        assert_eq!(parse_suggest(&json!({"instruction": "x", "model": " "})).unwrap().model, None);
        for bad in [
            json!({}),
            json!({"instruction": 3}),
            json!({"instruction": "x", "model": 3}),
            json!({"instruction": "x", "model": "--tools"}),
            json!({"instruction": "x", "model": "opus; rm"}),
            json!({"instruction": null}),
            json!({"instruction": "  "}),
            json!({"instruction": "x", "scope": null}),
            json!({"instruction": "x", "scope": "page"}),
            json!({"instruction": "x", "explain_only": null}),
            json!({"instruction": "x", "explain_only": "yes"}),
            json!({"instruction": "x", "confirm_document": 1}),
            json!({"instruction": "x", "include_document_context": null, "confirm_document": true}),
        ] {
            assert!(parse_suggest(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn instruction_limit_is_bytes_not_chars() {
        assert!(parse_suggest(&json!({"instruction": "x".repeat(MAX_INSTRUCTION_BYTES)})).is_ok());
        assert!(parse_suggest(&json!({"instruction": "x".repeat(MAX_INSTRUCTION_BYTES + 1)})).is_err());
        // 2100 two-byte characters: 2100 chars but 4200 bytes.
        assert!(parse_suggest(&json!({"instruction": "é".repeat(2100)})).is_err());
    }

    #[test]
    fn document_context_needs_confirmation() {
        let e = parse_suggest(&json!({"instruction": "x", "include_document_context": true})).unwrap_err();
        assert!(e.contains("confirm_document"), "{e}");
    }

    /// The whole path with the signed-in Claude Code account: engine context, one synthetic
    /// request, apply, undo, redo. Opt-in (`--ignored` and `WORDCRAFT_ASSISTANT_LIVE=1`).
    #[test]
    #[ignore = "sends one synthetic request with the signed-in Claude Code account"]
    fn live_suggest_apply_undo_redo() {
        if std::env::var_os("WORDCRAFT_ASSISTANT_LIVE").is_none() {
            eprintln!("skipped: set WORDCRAFT_ASSISTANT_LIVE=1");
            return;
        }
        let model = std::env::var("WORDCRAFT_ASSISTANT_MODEL").ok();
        let mut h = headless("Ths sentense has two erors.\nKeep this one.");
        let out = suggest(&mut h.session, &json!({"instruction": "Correct spelling only.", "scope": "paragraph"}), |req| {
            generate_locally(req, model.as_deref())
        })
        .unwrap();
        eprintln!("live: {}", out["suggestions"]);
        let applied = h
            .call("engine.execute", json!({"command": "assistant.apply", "params": {"context": out["context"], "changes": out["suggestions"]}}))
            .unwrap();
        assert_eq!(applied["applied"], 1);
        let fixed = text_of(&mut h);
        assert!(fixed.starts_with("This sentence has two errors."), "{fixed}");
        assert!(fixed.ends_with("\nKeep this one."), "{fixed}");
        run(&mut h, "edit.undo", json!({}));
        assert_eq!(text_of(&mut h), "Ths sentense has two erors.\nKeep this one.");
        run(&mut h, "edit.redo", json!({}));
        assert_eq!(text_of(&mut h), fixed);
    }

    #[test]
    fn full_flow_select_snapshot_suggest_apply_undo_redo_and_docx() {
        let mut h = headless("Alpha is slow.\nBeta is fine.");
        run(&mut h, "select.text", json!({"text": "Alpha"}));
        let mut seen = None;
        let out = suggest(&mut h.session, &json!({"instruction": "Shorten", "scope": "paragraph"}), |req| {
            seen = Some((req.targets.clone(), req.context.clone(), req.instruction.clone()));
            Ok(explain_reply())
        })
        .unwrap();
        let (targets, context, instruction) = seen.unwrap();
        assert_eq!(targets, ["Alpha is slow."]);
        assert!(context.is_none());
        assert_eq!(instruction, "Shorten");
        assert_eq!(out["suggestions"], json!([{"index": 0, "text": "Fast."}]));
        assert_eq!(out["explanation"], "Short and clear.");
        assert_eq!(text_of(&mut h), "Alpha is slow.\nBeta is fine.", "suggesting changes nothing");

        let context = out["context"].clone();
        let applied =
            h.call("engine.execute", json!({"command": "assistant.apply", "params": {"context": context, "changes": out["suggestions"]}})).unwrap();
        assert_eq!(applied["applied"], 1);
        assert_eq!(text_of(&mut h), "Fast.\nBeta is fine.");

        run(&mut h, "edit.undo", json!({}));
        assert_eq!(text_of(&mut h), "Alpha is slow.\nBeta is fine.", "one undo reverts the whole apply");
        run(&mut h, "edit.redo", json!({}));
        assert_eq!(text_of(&mut h), "Fast.\nBeta is fine.");

        let path = std::env::temp_dir().join(format!("wordcraft-mcp-assistant-{}.docx", std::process::id()));
        let path_text = path.display().to_string();
        run(&mut h, "file.save", json!({"path": path_text}));
        run(&mut h, "file.open", json!({"path": path_text}));
        let roundtrip = text_of(&mut h);
        let _ = std::fs::remove_file(&path);
        assert_eq!(roundtrip, "Fast.\nBeta is fine.");
    }

    #[test]
    fn explain_only_changes_the_prompt_and_drops_suggestions() {
        let mut h = headless("Alpha is slow.\nBeta is fine.");
        run(&mut h, "select.text", json!({"text": "Alpha"}));
        let out = suggest(&mut h.session, &json!({"instruction": "Why?", "scope": "paragraph", "explain_only": true}), |req| {
            assert!(req.instruction.starts_with(EXPLAIN_PREFIX), "{}", req.instruction);
            assert!(req.instruction.ends_with("Why?"));
            Ok(explain_reply())
        })
        .unwrap();
        assert_eq!(out["suggestions"], json!([]));
        assert_eq!(out["explanation"], "Short and clear.");
    }

    #[test]
    fn document_context_is_joined_with_newlines_like_the_app() {
        let mut h = headless("One.\nTwo.");
        run(&mut h, "select.text", json!({"text": "One"}));
        let mut ctx = None;
        suggest(
            &mut h.session,
            &json!({"instruction": "Check", "scope": "paragraph", "include_document_context": true, "confirm_document": true}),
            |req| {
                ctx = req.context.clone();
                Ok(json!({"suggestions": [], "explanation": "ok"}))
            },
        )
        .unwrap();
        assert_eq!(ctx.as_deref(), Some("One.\nTwo."));
    }

    #[test]
    fn invalid_model_answers_are_refused() {
        let cases = [
            json!({"suggestions": [], "explanation": 3}),
            json!({"suggestions": []}),
            json!({"explanation": "x"}),
            json!({"suggestions": [{"index": 1, "text": "x"}], "explanation": ""}),
            json!({"suggestions": [{"index": 0, "text": "a"}, {"index": 0, "text": "b"}], "explanation": ""}),
            json!({"suggestions": [{"index": 0, "text": "two\nlines"}], "explanation": ""}),
            json!({"suggestions": [], "explanation": "x".repeat(MAX_EXPLANATION_BYTES + 1)}),
        ];
        for reply in cases {
            let mut h = headless("Alpha.\nBeta.");
            run(&mut h, "select.text", json!({"text": "Alpha"}));
            let r = suggest(&mut h.session, &json!({"instruction": "x", "scope": "paragraph"}), provider(reply.clone()));
            assert!(r.is_err(), "accepted {reply}");
        }
    }

    #[test]
    fn generator_is_not_called_for_refused_requests() {
        let mut h = headless("Alpha.\nBeta.");
        run(&mut h, "select.text", json!({"text": "Alpha"}));
        let mut called = false;
        let r = suggest(&mut h.session, &json!({"instruction": "x", "scope": "page"}), |_| {
            called = true;
            Ok(json!({}))
        });
        assert!(r.is_err() && !called);
        let mut called = false;
        let big = "x".repeat(MAX_TEXT_BYTES);
        let r = suggest(&mut h.session, &json!({"instruction": big, "scope": "paragraph"}), |_| {
            called = true;
            Ok(json!({}))
        });
        assert!(r.is_err() && !called, "instruction and text share the request limit");
    }

    #[test]
    fn whole_body_auto_on_a_blank_document_needs_consent() {
        let mut h = Headless::default();
        let mut called = false;
        let r = suggest(&mut h.session, &json!({"instruction": "x"}), |_| {
            called = true;
            Ok(json!({}))
        });
        assert!(r.is_err() && !called, "auto on a blank document covers the whole body");
    }

    #[test]
    fn document_scope_needs_consent_before_the_provider() {
        let mut h = headless("Alpha.\nBeta.");
        let mut called = false;
        let r = suggest(&mut h.session, &json!({"instruction": "x", "scope": "document"}), |_| {
            called = true;
            Ok(json!({}))
        });
        assert!(r.is_err() && !called);
    }

    #[test]
    fn headless_intercepts_suggest_on_both_routes_and_refuses_bad_params_locally() {
        let mut h = headless("Alpha.\nBeta.");
        for call in [
            h.call("engine.execute", json!({"command": "assistant.suggest", "params": {"instruction": "x", "include_document_context": true}})),
            h.call("assistant.suggest", json!({"instruction": "x", "include_document_context": true})),
        ] {
            assert!(call.unwrap_err().contains("confirm_document"));
        }
    }

    #[test]
    fn suggest_metadata_is_listed_for_headless_commands() {
        let mut h = headless("");
        let list = h.call("engine.commands", json!({})).unwrap();
        assert!(list.as_array().unwrap().iter().any(|c| c["id"] == "assistant.suggest"));
    }
}
