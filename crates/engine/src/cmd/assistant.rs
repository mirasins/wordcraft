//! Writing assistant commands: `assistant.context` reads a bounded, revision-checked context and
//! `assistant.apply` applies validated replacements (plain text or Markdown) from it as one undo step.

use serde_json::{Value, json};

use crate::assistant::{self, ApplyRequest, ContextParams};
use crate::{CmdError, CmdResult, CommandSpec, Session};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("assistant.context", "Mirai Assistant Context", "Review › Mirai Assistant", context)
            .params(r#"{"scope": "auto" | "selection" | "paragraph" | "section" | "document", "confirm_document"?: bool} (auto uses selected text, or the whole body when there is no text selection; whole-body contexts require confirm_document: true)"#)
            .pure(),
        CommandSpec::new("assistant.apply", "Apply Mirai Assistant Changes", "Review › Mirai Assistant", apply)
            .params(r#"{"context": Context, "changes": [{"index": number, "text": string, "markdown"?: bool}], "suggest"?: bool} (plain text is one paragraph and keeps the target's formatting; markdown: true replaces the whole target with formatted blocks, and empty Markdown removes it; suggest: true writes them as tracked changes by Mirai and returns each change's revisions)"#),
        CommandSpec::new("assistant.resolve", "Resolve Mirai Assistant Suggestion", "Review › Mirai Assistant", resolve)
            .params(r#"{"revisions": [number], "accept": bool} (accepts or rejects only the tracked changes made under these revisions)"#),
    ]
}

/// Most revisions one `assistant.resolve` takes.
const MAX_RESOLVE_REVISIONS: usize = 4096;

fn resolve(s: &mut Session, v: &Value) -> CmdResult {
    let accept = v.get("accept").and_then(Value::as_bool).ok_or_else(|| CmdError::Params("`accept` must be a boolean".into()))?;
    let list = v.get("revisions").and_then(Value::as_array).ok_or_else(|| CmdError::Params("`revisions` must be an array".into()))?;
    if list.len() > MAX_RESOLVE_REVISIONS {
        return Err(CmdError::Params(format!("more than {MAX_RESOLVE_REVISIONS} revisions")));
    }
    let revs = list
        .iter()
        .map(|r| r.as_u64().and_then(|n| u32::try_from(n).ok()).ok_or_else(|| CmdError::Params("revisions are nonnegative integers".into())))
        .collect::<Result<std::collections::BTreeSet<u32>, _>>()?;
    let resolved = super::review::resolve_revisions(s, &revs, accept)?;
    Ok(json!({"resolved": resolved}))
}

fn context(s: &mut Session, v: &Value) -> CmdResult {
    let params: ContextParams = serde_json::from_value(v.clone()).map_err(|e| CmdError::Params(format!("bad parameters: {e}")))?;
    let ctx = assistant::context(s, &params)?;
    serde_json::to_value(ctx).map_err(|e| CmdError::Failed(e.to_string()))
}

fn apply(s: &mut Session, v: &Value) -> CmdResult {
    let req: ApplyRequest = serde_json::from_value(v.clone()).map_err(|e| CmdError::Params(format!("bad parameters: {e}")))?;
    let applied = assistant::apply(s, &req)?;
    let context = serde_json::to_value(applied.context).map_err(|e| CmdError::Failed(e.to_string()))?;
    let revisions: Vec<Value> = applied.revisions.iter().map(|(index, revs)| json!({"index": index, "revisions": revs})).collect();
    Ok(json!({"applied": applied.count, "documentId": s.document_id(), "context": context, "revisions": revisions}))
}
