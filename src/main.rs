//! gray-mediagen — merged media generation sidecar.
//!
//! One sidecar for gray-imagegen and gray-videogen. Each source lives
//! verbatim in `mods/` with its own providers and slash command — only the
//! wire plumbing is shared.
//!
//! Routing:
//! - `tool/call`   → `image_gen` → imagegen, `video_gen` → videogen.
//! - `command/run` → `/imagegen`, `/videogen` routed to their owners;
//!   `/mediagen` prints an overview.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

#[allow(dead_code)]
#[path = "mods/imagegen.rs"]
mod imagegen;
#[allow(dead_code)]
#[path = "mods/videogen.rs"]
mod videogen;

fn manifest() -> Value {
    let mut tools = imagegen::manifest_imagegen()["tools"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    tools.extend(
        videogen::manifest_videogen()["tools"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
    );
    json!({
        "name": "mediagen",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": "2.0",
        "tools": tools,
        "commands": ["/mediagen", "/imagegen", "/videogen"],
    })
}

/// `/mediagen` — merged-plugin overview.
fn overview() -> String {
    format!(
        "gray-mediagen {} — one sidecar, two generators:\n\
         \x20 image_gen  text→image (openrouter/openai/xai/fal/deepinfra, auto-probed by env key)\n\
         \x20 video_gen  text→video (fal/deepinfra/xai; long jobs persist for status/fetch)\n\
         \x20 /imagegen, /videogen  per-module status",
        env!("CARGO_PKG_VERSION")
    )
}

fn handle(req: &Value) -> (Option<Value>, bool) {
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let Some(id) = id else {
        return (None, method == "plugin/shutdown");
    };
    match method {
        "plugin/manifest" => (Some(json!({ "id": id, "result": manifest() })), false),
        "plugin/tools" => (
            Some(json!({ "id": id, "result": { "tools": manifest()["tools"] } })),
            false,
        ),
        "tool/call" => match params.get("name").and_then(Value::as_str).unwrap_or("") {
            "image_gen" => imagegen::handle_imagegen(req),
            "video_gen" => videogen::handle_videogen(req),
            name => (
                Some(json!({
                    "id": id,
                    "result": { "content": format!("unknown tool {name}"), "is_error": true }
                })),
                false,
            ),
        },
        "command/run" => match params.get("name").and_then(Value::as_str).unwrap_or("") {
            "/imagegen" => imagegen::handle_imagegen(req),
            "/videogen" => videogen::handle_videogen(req),
            "/mediagen" => (
                Some(json!({ "id": id, "result": { "text": overview() } })),
                false,
            ),
            name => (
                Some(json!({
                    "id": id,
                    "result": { "text": format!("unknown command {name} — try /mediagen") }
                })),
                false,
            ),
        },
        "plugin/shutdown" => (Some(json!({ "id": id, "result": {} })), true),
        _ => {
            let error = json!({ "code": -32601, "message": "method not found" });
            (Some(json!({ "id": id, "error": error })), false)
        }
    }
}

fn main() -> std::io::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("manifest") {
        println!("{}", manifest());
        return Ok(());
    }
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let (reply, exit) = handle(&req);
        if let Some(reply) = reply {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
        if exit {
            break;
        }
    }
    Ok(())
}
