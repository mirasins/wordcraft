//! Writing assistant commands: `assistant.context` reads a bounded, revision-checked context and
//! `assistant.apply` applies validated replacements from it as one undo step.

use serde_json::{Value, json};

use crate::assistant::{self, ApplyRequest, ContextParams};
use crate::{CmdError, CmdResult, CommandSpec, Session};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("assistant.context", "Writing Assistant Context", "Review › Writing Assistant", context)
            .params(r#"{"scope": "auto" | "selection" | "paragraph" | "section" | "document", "confirm_document"?: bool}"#)
            .pure(),
        CommandSpec::new("assistant.apply", "Apply Writing Assistant Changes", "Review › Writing Assistant", apply)
            .params(r#"{"context": Context, "changes": [{"index": number, "text": string}]}"#),
    ]
}

fn context(s: &mut Session, v: &Value) -> CmdResult {
    let params: ContextParams = serde_json::from_value(v.clone()).map_err(|e| CmdError::Params(format!("bad parameters: {e}")))?;
    let ctx = assistant::context(s, &params)?;
    serde_json::to_value(ctx).map_err(|e| CmdError::Failed(e.to_string()))
}

fn apply(s: &mut Session, v: &Value) -> CmdResult {
    let req: ApplyRequest = serde_json::from_value(v.clone()).map_err(|e| CmdError::Params(format!("bad parameters: {e}")))?;
    let applied = assistant::apply(s, &req)?;
    let mut out = json!({"applied": applied.count, "documentId": s.document_id()});
    if let Some(ctx) = applied.context {
        out["context"] = serde_json::to_value(ctx).map_err(|e| CmdError::Failed(e.to_string()))?;
    }
    Ok(out)
}
