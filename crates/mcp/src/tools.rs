//! MCP tools: names, schemas and implementations on top of a [`Backend`].

use serde_json::{Value, json};

use crate::backend::Backend;

/// An image tool result from a reply carrying base64 PNG in `png`.
fn png_result(v: &Value, info: &Value) -> ToolResult {
    match v.get("png").and_then(Value::as_str) {
        Some(png) if !png.is_empty() => ToolResult::image(png.to_string(), info),
        _ => ToolResult::error("the reply carried no picture"),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult {
    pub content: Vec<Value>,
    pub is_error: bool,
}

impl ToolResult {
    pub fn text(t: impl Into<String>) -> Self {
        Self { content: vec![json!({"type": "text", "text": t.into()})], is_error: false }
    }
    pub fn json(v: &Value) -> Self {
        Self::text(serde_json::to_string_pretty(v).unwrap_or_default())
    }
    pub fn error(t: impl Into<String>) -> Self {
        Self { content: vec![json!({"type": "text", "text": t.into()})], is_error: true }
    }
    pub fn image(png_base64: String, info: &Value) -> Self {
        Self {
            content: vec![json!({"type": "image", "data": png_base64, "mimeType": "image/png"}), json!({"type": "text", "text": info.to_string()})],
            is_error: false,
        }
    }
    pub fn to_value(&self) -> Value {
        json!({"content": self.content, "isError": self.is_error})
    }
}

fn s(desc: &str) -> Value {
    json!({"type": "string", "description": desc})
}
fn n(desc: &str) -> Value {
    json!({"type": "number", "description": desc})
}
fn obj(props: Value, req: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": req})
}
fn tool(name: &str, title: &str, desc: &str, schema: Value, ro: bool) -> Value {
    tool_world(name, title, desc, schema, ro, false)
}
/// `open`: the tool sends data outside the app (a suggestion goes to the writing assistant's provider).
fn tool_world(name: &str, title: &str, desc: &str, schema: Value, ro: bool, open: bool) -> Value {
    json!({"name": name, "title": title, "description": desc, "inputSchema": schema, "annotations": {"title": title, "readOnlyHint": ro, "openWorldHint": open}})
}

pub fn tool_definitions() -> Value {
    json!([
        tool(
            "list_commands",
            "List commands",
            "Every WordCraft command: id, label, ribbon location, shortcut and params. Filter with `query`.",
            obj(json!({"query": s("Case-insensitive filter on id/label/location")}), &[]),
            true
        ),
        tool(
            "execute",
            "Run a command",
            "Run one command by id with params, e.g. {command:\"format.bold\"} or {command:\"insert.table\", params:{rows:3, cols:4}}.",
            obj(json!({"command": s("Command id"), "params": {"type": "object", "description": "Command parameters"}}), &["command"]),
            false
        ),
        tool(
            "batch",
            "Run several commands",
            "Run commands in order; stops at the first error unless keepGoing.",
            obj(
                json!({"commands": {"type": "array", "items": {"type": "object", "properties": {"command": {"type": "string"}, "params": {"type": "object"}}, "required": ["command"]}}, "keepGoing": {"type": "boolean"}}),
                &["commands"]
            ),
            false
        ),
        tool(
            "new_document",
            "New document",
            "Start a document: blank, sample, letter, resume or report.",
            obj(json!({"template": s("blank|sample|letter|resume|report")}), &[]),
            false
        ),
        tool(
            "open_document",
            "Open",
            "Open a .docx, .doc, .odt, .rtf, .md, .tex, .html, .txt or .json file, or any other text file (.m3u, .log, .ini…) as plain text. Give `password` for a protected file; without it the call fails asking for one.",
            obj(json!({"path": s("File path"), "password": s("Password, if the file is protected")}), &["path"]),
            false
        ),
        tool(
            "save_document",
            "Save",
            "Save (format from the extension: docx, pdf, odt, rtf, html, md, tex, txt, png).",
            obj(json!({"path": s("File path; omit to save in place")}), &[]),
            false
        ),
        tool(
            "type_text",
            "Type text",
            "Type at the caret (replacing the selection). Use \\n... for line breaks inside text; use execute text.newParagraph for new paragraphs, or `paragraphs: true` to split lines into paragraphs.",
            obj(json!({"text": s("Text"), "paragraphs": {"type": "boolean"}}), &["text"]),
            false
        ),
        tool(
            "select_text",
            "Select text",
            "Select the n-th occurrence of some text.",
            obj(json!({"text": s("Text to find"), "occurrence": n("1-based")}), &["text"]),
            false
        ),
        tool("get_text", "Document text", "Body text with tracked deletions omitted.", obj(json!({}), &[]), true),
        tool(
            "inspect_document",
            "Inspect document",
            "Structure: blocks with text, styles, formatting runs, lists, tables, sections, parts, selection, pages.",
            obj(json!({}), &[]),
            true
        ),
        tool(
            "render_page",
            "Render page",
            "Render a page to PNG to look at the result.",
            obj(json!({"page": n("1-based page"), "scale": n("Pixels per point (default 1)")}), &[]),
            true
        ),
        tool("parity", "Feature parity", "WordCraft's command coverage of the word-processor feature catalog.", obj(json!({}), &[]), true),
        tool("screenshot", "Screenshot app", "Screenshot of the whole WordCraft window (desktop app only).", obj(json!({}), &[]), true),
        tool(
            "click",
            "Click",
            "Click at window coordinates (desktop app only).",
            obj(json!({"x": n("x"), "y": n("y"), "count": n("1-3 clicks"), "shift": {"type": "boolean"}, "cmd": {"type": "boolean"}}), &["x", "y"]),
            false
        ),
        tool(
            "key",
            "Press key",
            "Press a key with modifiers (desktop app only), e.g. {key:\"B\", cmd:true}.",
            obj(json!({"key": s("Key name"), "shift": {"type": "boolean"}, "alt": {"type": "boolean"}, "cmd": {"type": "boolean"}}), &["key"]),
            false
        ),
        tool(
            "ui_inspect",
            "Inspect UI",
            "UI state: tab, panes, dialog, page rectangles on screen, caret (desktop app only).",
            obj(json!({}), &[]),
            true
        ),
        tool(
            "assistant_context",
            "Writing assistant context",
            "What the writing assistant would work on: document id, revision, scope and target ranges with their original text. Local only; nothing is sent to a provider. `auto` uses selected text, or the whole body with no selection. Any whole-body scope needs confirm_document: true.",
            obj(
                json!({"scope": {"type": "string", "enum": ["auto", "selection", "paragraph", "section", "document"], "description": "Default auto: selected text, or the whole body when no text is selected"}, "confirm_document": {"type": "boolean", "description": "Explicit consent required whenever the scope covers the whole body"}}),
                &[]
            ),
            true
        ),
        tool_world(
            "assistant_suggest",
            "Writing assistant suggest",
            "Ask the writing assistant (your Claude Code account) for rewrites of the targets. `auto` uses selected text, or the whole body when nothing is selected. Sends the instruction and target text to the provider; any whole-body scope and include_document_context require explicit confirm_document: true. Does not change the document: apply with assistant_apply. explain_only returns an explanation with no suggestions.",
            obj(
                json!({
                    "instruction": s("What to do with the text"),
                    "scope": {"type": "string", "enum": ["auto", "selection", "paragraph", "section", "document"], "description": "Default auto: selected text, or the whole body when no text is selected"},
                    "confirm_document": {"type": "boolean", "description": "Explicit consent required whenever the scope covers the whole body"},
                    "include_document_context": {"type": "boolean", "description": "Send the whole document as context (needs confirm_document)"},
                    "explain_only": {"type": "boolean", "description": "Explain without suggesting rewrites"},
                    "model": {"type": "string", "description": "Claude model alias or id (sonnet, opus, haiku, claude-opus-5-5…); omit for Claude Code's default"}
                }),
                &["instruction"]
            ),
            true,
            true
        ),
        tool(
            "assistant_apply",
            "Writing assistant apply",
            "Apply suggestions from assistant_suggest as one undoable edit. Pass the context from assistant_context or assistant_suggest and changes [{index, text}].",
            obj(
                json!({
                    "context": {"type": "object", "description": "The context snapshot the suggestions were made for"},
                    "changes": {"type": "array", "items": {"type": "object", "properties": {"index": {"type": "integer", "minimum": 0}, "text": {"type": "string"}}, "required": ["index", "text"]}}
                }),
                &["context", "changes"]
            ),
            false
        ),
    ])
}

fn exec(b: &mut dyn Backend, id: &str, params: Value) -> Result<Value, String> {
    b.call("engine.execute", json!({"command": id, "params": params}))
}

/// The named keys of `a` that are present, as a new object: nothing else is forwarded.
fn pick(a: &Value, keys: &[&str]) -> Value {
    Value::Object(keys.iter().filter_map(|k| a.get(*k).map(|v| ((*k).to_string(), v.clone()))).collect())
}

fn wrap(r: Result<Value, String>) -> ToolResult {
    match r {
        Ok(v) => ToolResult::json(&v),
        Err(e) => ToolResult::error(e),
    }
}

pub fn call_tool(b: &mut dyn Backend, name: &str, a: &Value) -> ToolResult {
    let st = |k: &str| a.get(k).and_then(Value::as_str);
    match name {
        "list_commands" => {
            let q = st("query").unwrap_or("").to_lowercase();
            match b.call("engine.commands", json!({})) {
                Ok(Value::Array(v)) => {
                    let f: Vec<Value> = v
                        .into_iter()
                        .filter(|c| {
                            q.is_empty()
                                || ["id", "label", "location"]
                                    .iter()
                                    .any(|k| c.get(*k).and_then(Value::as_str).is_some_and(|x| x.to_lowercase().contains(&q)))
                        })
                        .collect();
                    ToolResult::json(&Value::Array(f))
                }
                Ok(v) => ToolResult::json(&v),
                Err(e) => ToolResult::error(e),
            }
        }
        "execute" => match st("command") {
            Some(c) => wrap(exec(b, c, a.get("params").cloned().unwrap_or(json!({})))),
            None => ToolResult::error("missing `command`"),
        },
        "batch" => {
            let keep = a.get("keepGoing").and_then(Value::as_bool).unwrap_or(false);
            let mut out = Vec::new();
            for c in a.get("commands").and_then(Value::as_array).cloned().unwrap_or_default() {
                let id = c.get("command").and_then(Value::as_str).unwrap_or("");
                let r = exec(b, id, c.get("params").cloned().unwrap_or(json!({})));
                let failed = r.is_err();
                out.push(match r {
                    Ok(v) => json!({"command": id, "ok": true, "result": v}),
                    Err(e) => json!({"command": id, "ok": false, "error": e}),
                });
                if failed && !keep {
                    return ToolResult { content: vec![json!({"type": "text", "text": Value::Array(out).to_string()})], is_error: true };
                }
            }
            ToolResult::json(&Value::Array(out))
        }
        "new_document" => wrap(exec(b, "file.new", json!({"template": st("template").unwrap_or("blank")}))),
        "open_document" => wrap(exec(b, "file.open", json!({"path": st("path").unwrap_or(""), "password": st("password")}))),
        "save_document" => {
            let p = st("path");
            let ext = p.and_then(|x| x.rsplit('.').next()).unwrap_or("").to_ascii_lowercase();
            if ext == "png" {
                wrap(exec(b, "file.exportPng", json!({"path": p})))
            } else {
                wrap(exec(
                    b,
                    "file.save",
                    match p {
                        Some(p) => json!({"path": p}),
                        None => json!({}),
                    },
                ))
            }
        }
        "type_text" => {
            let t = st("text").unwrap_or("");
            if a.get("paragraphs").and_then(Value::as_bool).unwrap_or(false) {
                let mut last = Ok(Value::Null);
                for (i, line) in t.split('\n').enumerate() {
                    if i > 0 {
                        last = exec(b, "text.newParagraph", json!({}));
                        if last.is_err() {
                            break;
                        }
                    }
                    if !line.is_empty() {
                        last = exec(b, "text.insert", json!({"text": line, "raw": true}));
                    }
                    if last.is_err() {
                        break;
                    }
                }
                wrap(last)
            } else {
                wrap(exec(b, "text.insert", json!({"text": t, "raw": true})))
            }
        }
        "select_text" => {
            wrap(exec(b, "select.text", json!({"text": st("text").unwrap_or(""), "occurrence": a.get("occurrence").cloned().unwrap_or(json!(1))})))
        }
        "get_text" => wrap(exec(b, "document.text", json!({}))),
        "inspect_document" => wrap(b.call("document.inspect", json!({}))),
        "parity" => wrap(b.call("ui.parity", json!({}))),
        // Pictures travel inside the reply (`png`), headless or through the app: no file with a
        // predictable name in the shared temporary folder.
        "render_page" => {
            let params = json!({"page": a.get("page").cloned().unwrap_or(json!(1)), "scale": a.get("scale").cloned().unwrap_or(json!(1.0))});
            match b.call("ui.render", params) {
                Ok(v) => png_result(&v, &json!({"width": v["width"], "height": v["height"], "pages": v["pages"]})),
                Err(e) => ToolResult::error(e),
            }
        }
        "screenshot" => match b.call("ui.screenshot", json!({})) {
            Ok(v) => png_result(&v, &json!({"width": v["width"], "height": v["height"]})),
            Err(e) => ToolResult::error(e),
        },
        "click" => wrap(b.call("ui.click", a.clone())),
        "key" => wrap(b.call("ui.key", a.clone())),
        "ui_inspect" => wrap(b.call("ui.inspect", json!({}))),
        "assistant_context" => wrap(exec(b, "assistant.context", pick(a, &["scope", "confirm_document"]))),
        "assistant_suggest" => wrap(exec(
            b,
            "assistant.suggest",
            pick(a, &["instruction", "scope", "confirm_document", "include_document_context", "explain_only", "model"]),
        )),
        "assistant_apply" => wrap(exec(b, "assistant.apply", pick(a, &["context", "changes"]))),
        other => ToolResult::error(format!("unknown tool `{other}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A backend that keeps what it was asked for, so a test can read the forwarded params.
    struct Recording {
        seen: Option<(String, Value)>,
    }

    impl Backend for Recording {
        fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
            self.seen = Some((method.to_string(), params));
            Ok(json!({"encrypted": true}))
        }
        fn has_ui(&self) -> bool {
            false
        }
        fn describe(&self) -> String {
            "recording".into()
        }
    }

    /// What the backend was asked for: `file.open` runs through `engine.execute`, so the
    /// command and its parameters are nested one level down.
    fn forwarded(args: Value) -> Value {
        let mut b = Recording { seen: None };
        let r = call_tool(&mut b, "open_document", &args);
        assert!(!r.is_error, "open_document failed: {:?}", r);
        let (method, mut call) = b.seen.expect("the backend was called");
        assert_eq!(method, "engine.execute");
        assert_eq!(call["command"], json!("file.open"));
        call["params"].take()
    }

    #[test]
    fn open_document_forwards_the_password_to_file_open() {
        let p = forwarded(json!({"path": "/tmp/a.docx", "password": "s3cret"}));
        assert_eq!(p["path"], json!("/tmp/a.docx"));
        assert_eq!(p["password"], json!("s3cret"));
    }

    /// Without a password the parameter stays null rather than an empty string: `file.open` reads
    /// an empty one as "no password given", so a protected file would come back unprotected.
    #[test]
    fn open_document_without_a_password_passes_null() {
        assert_eq!(forwarded(json!({"path": "/tmp/a.docx"}))["password"], Value::Null);
    }

    /// The password has to be in the schema, or a client has no way to know it can be sent.
    #[test]
    fn the_schema_advertises_the_password_as_optional() {
        let tools = tool_definitions();
        let open = tools.as_array().expect("the tools are a list").iter().find(|t| t["name"] == "open_document").expect("open_document is offered");
        let schema = &open["inputSchema"];
        assert!(schema["properties"]["password"].is_object(), "the schema offers no password");
        assert_eq!(schema["required"], json!(["path"]), "the password stays optional");
    }

    fn definition(name: &str) -> Value {
        tool_definitions().as_array().expect("list").iter().find(|t| t["name"] == name).cloned().unwrap_or(Value::Null)
    }

    #[test]
    fn assistant_tools_have_the_right_annotations() {
        assert_eq!(definition("assistant_context")["annotations"]["openWorldHint"], json!(false));
        assert_eq!(definition("assistant_context")["annotations"]["readOnlyHint"], json!(true));
        let suggest = definition("assistant_suggest");
        assert_eq!(suggest["annotations"]["openWorldHint"], json!(true), "suggestions go to the provider");
        assert_eq!(suggest["annotations"]["readOnlyHint"], json!(true), "suggest does not change the document");
        let apply = definition("assistant_apply");
        assert_eq!(apply["annotations"]["readOnlyHint"], json!(false));
        assert_eq!(apply["annotations"]["openWorldHint"], json!(false));
    }

    #[test]
    fn assistant_suggest_schema_carries_the_consent_parameters() {
        let schema = &definition("assistant_suggest")["inputSchema"];
        assert_eq!(schema["required"], json!(["instruction"]));
        for k in ["confirm_document", "include_document_context", "explain_only"] {
            assert_eq!(schema["properties"][k]["type"], json!("boolean"), "{k}");
        }
        assert_eq!(schema["properties"]["scope"]["enum"], json!(["auto", "selection", "paragraph", "section", "document"]));
        assert!(schema["properties"]["scope"]["description"].as_str().is_some_and(|s| s.contains("whole body when no text is selected")));
        assert!(schema["properties"]["confirm_document"]["description"].as_str().is_some_and(|s| s.contains("whole body")));
        assert_eq!(definition("assistant_apply")["inputSchema"]["required"], json!(["context", "changes"]));
    }

    #[test]
    fn assistant_tools_forward_only_their_known_keys() {
        let mut b = Recording { seen: None };
        let r = call_tool(&mut b, "assistant_suggest", &json!({"instruction": "Shorten", "explain_only": true, "scope": "paragraph", "junk": 1}));
        assert!(!r.is_error, "{:?}", r);
        let (method, call) = b.seen.take().expect("called");
        assert_eq!(method, "engine.execute");
        assert_eq!(call["command"], json!("assistant.suggest"));
        assert_eq!(call["params"], json!({"instruction": "Shorten", "explain_only": true, "scope": "paragraph"}));
    }

    #[test]
    fn assistant_apply_forwards_context_and_changes() {
        let mut b = Recording { seen: None };
        let args = json!({"context": {"document_id": "d", "revision": 3}, "changes": [{"index": 0, "text": "x"}], "x": 1});
        call_tool(&mut b, "assistant_apply", &args);
        let (_, call) = b.seen.take().expect("called");
        assert_eq!(call["command"], json!("assistant.apply"));
        assert_eq!(call["params"], json!({"context": {"document_id": "d", "revision": 3}, "changes": [{"index": 0, "text": "x"}]}));
    }
}
