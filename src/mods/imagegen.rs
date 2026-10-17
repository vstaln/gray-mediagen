//! Ported wholesale from gray-imagegen/src/main.rs.
//! gray-imagegen — image generation across five env-keyed providers.
//!
//! Five providers — openrouter / openai / xai / fal / deepinfra — behind one
//! `image_gen` tool: {prompt, provider?:"auto", model?,
//! size?:"1024x1024", out?:"./image-<ts>.png"}. With `provider:"auto"` the
//! first configured backend wins; every key is read from the environment and a
//! missing key is a clear tool error naming the variable.
//!
//! HTTP is shelled out to `curl`. The reply carries `images` media (protocol
//! 1.3) so the model sees the result; files over 8 MiB are saved but not
//! inlined.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{Value, json};

const MEDIA_CAP: u64 = 8 * 1024 * 1024;

pub fn manifest_imagegen() -> Value {
    json!({
        "name": "imagegen",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": "1.3",
        "tools": [{
            "name": "image_gen",
            "description": "Generate an image from a text prompt and save it to disk. Providers (auto-probed by env key): openrouter (OPENROUTER_API_KEY), openai (OPENAI_API_KEY), xai (XAI_API_KEY), fal (FAL_KEY), deepinfra (DEEPINFRA_API_KEY). Pass provider+model to pin one. Returns the saved path and inlines the image.",
            "parameters": {
                "type": "object",
                "properties": {
                    "prompt":   {"type": "string", "description": "What to draw"},
                    "provider": {"type": "string", "description": "auto (default) | openrouter | openai | xai | fal | deepinfra"},
                    "model":    {"type": "string", "description": "Provider-specific model id override"},
                    "size":     {"type": "string", "description": "WxH, default 1024x1024 (fal maps this to a preset)"},
                    "out":      {"type": "string", "description": "Output file path, default ./image-<ts>.png"}
                },
                "required": ["prompt"]
            }
        }],
        "commands": ["/imagegen"],
    })
}

// ---------- helpers ----------------------------------------------------------

const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().fold(0u32, |acc, &b| (acc << 8) | b as u32) << (8 * (3 - chunk.len()));
        for i in 0..4 {
            if i > chunk.len() {
                out.push('=');
            } else {
                out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            }
        }
    }
    out
}

fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    let table = |c: u8| -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    };
    let clean: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    for quad in clean.chunks(4) {
        let mut acc: u32 = 0;
        let mut pad = 0;
        for (i, &c) in quad.iter().enumerate() {
            acc <<= 6;
            if c == b'=' {
                pad += 1;
            } else if pad > 0 {
                return Err("bad base64".into());
            } else {
                acc |= table(c).ok_or("bad base64")? as u32;
            }
            let _ = i;
        }
        acc <<= 6 * (4 - quad.len());
        let bytes = acc.to_be_bytes();
        let n = quad.len().saturating_sub(1).saturating_sub(pad).min(3);
        out.extend_from_slice(&bytes[1..=n]);
    }
    Ok(out)
}

/// POST JSON with a bearer token; returns (status, body text).
fn post_json(url: &str, token: &str, body: &Value, auth_kind: &str) -> Result<(u16, String), String> {
    let auth = if auth_kind == "fal" {
        format!("Authorization: Key {token}")
    } else {
        format!("Authorization: Bearer {token}")
    };
    let out = std::process::Command::new("curl")
        .args([
            "-sS", "--max-time", "120", "-w", "\n%{http_code}",
            "-X", "POST", url,
            "-H", &auth,
            "-H", "Content-Type: application/json",
            "-d",
        ])
        .arg(body.to_string())
        .output()
        .map_err(|e| format!("couldn't run curl: {e}"))?;
    split_status(out)
}

fn get(url: &str, token: &str, auth_kind: &str) -> Result<(u16, String), String> {
    let mut args: Vec<String> = vec!["-sS".into(), "--max-time".into(), "30".into(), "-w".into(), "\n%{http_code}".into()];
    if !token.is_empty() {
        args.push("-H".into());
        args.push(if auth_kind == "fal" {
            format!("Authorization: Key {token}")
        } else {
            format!("Authorization: Bearer {token}")
        });
    }
    args.push(url.into());
    let out = std::process::Command::new("curl")
        .args(&args)
        .output()
        .map_err(|e| format!("couldn't run curl: {e}"))?;
    split_status(out)
}

fn split_status(out: std::process::Output) -> Result<(u16, String), String> {
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let (body, status) = text.rsplit_once('\n').unwrap_or((&text, "0"));
    let status: u16 = status.trim().parse().unwrap_or(0);
    if status == 0 && !out.status.success() {
        return Err(format!("curl failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok((status, body.to_string()))
}

fn env_key(name: &str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{name} is not set"))
}

fn api_err(provider: &str, status: u16, body: &str) -> String {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.pointer("/error"))
                .or_else(|| v.pointer("/detail"))
                .and_then(|d| d.as_str().map(str::to_string).or_else(|| Some(d.to_string())))
        })
        .unwrap_or_else(|| body.chars().take(300).collect());
    format!("{provider} → HTTP {status}: {detail}")
}

// ---------- image extraction -------------------------------------------------

enum ImageBytes {
    B64(String),
    Url(String),
}

/// OpenAI-style `{data:[{b64_json|url}]}`.
fn openai_extract(v: &Value) -> Result<ImageBytes, String> {
    let item = v.pointer("/data/0").ok_or("response had no data[0]")?;
    if let Some(b) = item.get("b64_json").and_then(Value::as_str) {
        return Ok(ImageBytes::B64(b.to_string()));
    }
    if let Some(u) = item.get("url").and_then(Value::as_str) {
        return Ok(ImageBytes::Url(u.to_string()));
    }
    Err("data[0] had neither b64_json nor url".into())
}

// ---------- providers --------------------------------------------------------

fn gen_openai_style(
    provider: &str,
    url: &str,
    token: &str,
    model: &str,
    prompt: &str,
    size: Option<&str>,
    extra: &[(&str, Value)],
) -> Result<ImageBytes, String> {
    let mut body = json!({"model": model, "prompt": prompt, "n": 1});
    if let Some(s) = size {
        body["size"] = json!(s);
    }
    for (k, v) in extra {
        body[*k] = v.clone();
    }
    let (status, text) = post_json(url, token, &body, "bearer")?;
    let v: Value = serde_json::from_str(&text).unwrap_or(json!({}));
    if !(200..300).contains(&status) {
        return Err(api_err(provider, status, &text));
    }
    openai_extract(&v)
}

fn gen_fal(token: &str, model: &str, prompt: &str, size: &str) -> Result<ImageBytes, String> {
    let preset = fal_size_preset(size);
    let body = json!({
        "prompt": prompt,
        "image_size": preset,
        "num_inference_steps": 4,
        "output_format": "png",
        "enable_safety_checker": false,
    });
    let submit_url = format!("https://queue.fal.run/{model}");
    let (status, text) = post_json(&submit_url, token, &body, "fal")?;
    let v: Value = serde_json::from_str(&text).unwrap_or(json!({}));
    if !(200..300).contains(&status) {
        return Err(api_err("fal", status, &text));
    }
    // Some fal endpoints answer synchronously on submit.
    if let Some(u) = v.pointer("/images/0/url").and_then(Value::as_str) {
        return Ok(ImageBytes::Url(u.to_string()));
    }
    let status_url = v.get("status_url").and_then(Value::as_str).unwrap_or("").to_string();
    let response_url = v.get("response_url").and_then(Value::as_str).unwrap_or("").to_string();
    if status_url.is_empty() || response_url.is_empty() {
        return Err(format!("fal submit gave no queue urls: {}", text.chars().take(200).collect::<String>()));
    }
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_secs(2));
        let (_, stext) = get(&status_url, token, "fal")?;
        let sv: Value = serde_json::from_str(&stext).unwrap_or(json!({}));
        match sv.get("status").and_then(Value::as_str).unwrap_or("") {
            "COMPLETED" => {
                let (rs, rtext) = get(&response_url, token, "fal")?;
                let rv: Value = serde_json::from_str(&rtext).unwrap_or(json!({}));
                if !(200..300).contains(&rs) {
                    return Err(api_err("fal", rs, &rtext));
                }
                if let Some(u) = rv.pointer("/images/0/url").and_then(Value::as_str) {
                    return Ok(ImageBytes::Url(u.to_string()));
                }
                return Err(format!("fal result had no images[0].url: {}", rtext.chars().take(200).collect::<String>()));
            }
            "FAILED" | "CANCELLED" => {
                return Err(format!("fal job failed: {}", stext.chars().take(200).collect::<String>()));
            }
            _ => continue,
        }
    }
    Err("fal job still running after ~20s — try again or a faster model".into())
}

fn fal_size_preset(size: &str) -> &'static str {
    let (w, h) = size
        .split_once('x')
        .map(|(a, b)| (a.parse::<u32>().unwrap_or(1024), b.parse::<u32>().unwrap_or(1024)))
        .unwrap_or((1024, 1024));
    if w == h {
        "square_hd"
    } else if w > h {
        "landscape_16_9"
    } else {
        "portrait_16_9"
    }
}

fn providers() -> Vec<(&'static str, &'static str, &'static str)> {
    // (name, env key, default model)
    vec![
        ("openrouter", "OPENROUTER_API_KEY", "google/gemini-3.1-flash-image"),
        ("openai", "OPENAI_API_KEY", "gpt-image-1"),
        ("xai", "XAI_API_KEY", "grok-imagine-image"),
        ("fal", "FAL_KEY", "fal-ai/flux-2/klein/9b"),
        ("deepinfra", "DEEPINFRA_API_KEY", "black-forest-labs/FLUX-1-schnell"),
    ]
}

fn generate(provider: &str, token: &str, model: &str, prompt: &str, size: &str) -> Result<ImageBytes, String> {
    match provider {
        "openrouter" => gen_openai_style(
            provider, "https://openrouter.ai/api/v1/images/generations",
            token, model, prompt, Some(size), &[],
        ),
        "openai" => gen_openai_style(
            provider, "https://api.openai.com/v1/images/generations",
            token, model, prompt, Some(size), &[],
        ),
        "xai" => gen_openai_style(
            provider, "https://api.x.ai/v1/images/generations",
            token, model, prompt, None, &[("response_format", json!("b64_json"))],
        ),
        "fal" => gen_fal(token, model, prompt, size),
        "deepinfra" => gen_openai_style(
            provider, "https://api.deepinfra.com/v1/openai/images/generations",
            token, model, prompt, Some(size), &[],
        ),
        other => Err(format!("unknown provider '{other}' — openrouter|openai|xai|fal|deepinfra")),
    }
}

// ---------- output -----------------------------------------------------------

fn out_path(arg: Option<&str>, session: &Value) -> PathBuf {
    let mut name = arg.unwrap_or("").trim().to_string();
    if name.is_empty() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        name = format!("image-{ts}.png");
    }
    let path = if let Some(rest) = name.strip_prefix("~/") {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(rest))
            .unwrap_or_else(|| PathBuf::from(&name))
    } else {
        PathBuf::from(&name)
    };
    if path.is_relative() {
        if let Some(cwd) = session.pointer("/cwd").and_then(Value::as_str) {
            return PathBuf::from(cwd).join(path);
        }
    }
    path
}

fn mime_for(bytes: &[u8], path: &std::path::Path) -> &'static str {
    if bytes.starts_with(b"\x89PNG") { return "image/png"; }
    if bytes.starts_with(b"\xff\xd8\xff") { return "image/jpeg"; }
    if bytes.starts_with(b"RIFF") { return "image/webp"; }
    if bytes.starts_with(b"GIF8") { return "image/gif"; }
    match path.extension().and_then(|e| e.to_str()) {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        _ => "image/png",
    }
}

fn save_image(img: &ImageBytes, path: &std::path::Path) -> Result<u64, String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    match img {
        ImageBytes::B64(b) => {
            let bytes = b64_decode(b)?;
            std::fs::write(path, &bytes).map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
            Ok(bytes.len() as u64)
        }
        ImageBytes::Url(u) => {
            let status = std::process::Command::new("curl")
                .args(["-sSL", "--max-time", "120", "-o"])
                .arg(path)
                .arg(u)
                .status()
                .map_err(|e| format!("couldn't run curl: {e}"))?;
            if !status.success() {
                return Err(format!("image download failed for {u}"));
            }
            Ok(std::fs::metadata(path).map(|m| m.len()).unwrap_or(0))
        }
    }
}

// ---------- tool -------------------------------------------------------------

fn call_tool(name: &str, args: &Value, session: &Value) -> Value {
    if name != "image_gen" {
        return json!({ "content": format!("unknown tool: {name}"), "is_error": true });
    }
    let prompt = args.get("prompt").and_then(Value::as_str).unwrap_or("").trim();
    if prompt.is_empty() {
        return json!({ "content": "image_gen needs a prompt", "is_error": true });
    }
    let size = args.get("size").and_then(Value::as_str).unwrap_or("1024x1024");
    let model_arg = args.get("model").and_then(Value::as_str).unwrap_or("");
    let requested = args.get("provider").and_then(Value::as_str).unwrap_or("auto");

    let mut tried: Vec<String> = Vec::new();
    let targets: Vec<(&str, &str, &str)> = if requested == "auto" || requested.is_empty() {
        providers()
            .into_iter()
            .filter(|(_, env, _)| std::env::var(env).map(|v| !v.trim().is_empty()).unwrap_or(false))
            .collect()
    } else {
        match providers().into_iter().find(|(n, _, _)| *n == requested) {
            Some(p) => vec![p],
            None => return json!({
                "content": format!("unknown provider '{requested}' — openrouter|openai|xai|fal|deepinfra"),
                "is_error": true,
            }),
        }
    };
    if targets.is_empty() {
        return json!({
            "content": if requested == "auto" {
                "no image provider configured — set one of OPENROUTER_API_KEY, OPENAI_API_KEY, XAI_API_KEY, FAL_KEY, DEEPINFRA_API_KEY".to_string()
            } else {
                format!("provider '{requested}' selected but its key is not set")
            },
            "is_error": true,
        });
    }

    for (name, env, default_model) in targets {
        let token = match env_key(env) {
            Ok(t) => t,
            Err(e) => return json!({ "content": e, "is_error": true }),
        };
        let model = if model_arg.is_empty() {
            std::env::var(format!("{}_IMAGE_MODEL", name.to_uppercase()))
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| default_model.to_string())
        } else {
            model_arg.to_string()
        };
        match generate(name, &token, &model, prompt, size) {
            Ok(img) => {
                let path = out_path(args.get("out").and_then(Value::as_str), session);
                return match save_image(&img, &path) {
                    Ok(bytes) => {
                        let mut result = json!({
                            "content": format!("saved {} ({} bytes, {name}/{model})", path.display(), bytes)
                        });
                        if bytes > 0 && bytes <= MEDIA_CAP {
                            if let Ok(data) = std::fs::read(&path) {
                                result["images"] = json!([{
                                    "mime": mime_for(&data, &path),
                                    "data_base64": b64_encode(&data),
                                }]);
                            }
                        }
                        result
                    }
                    Err(e) => json!({ "content": e, "is_error": true }),
                };
            }
            Err(e) => {
                tried.push(format!("{name}: {e}"));
                if requested != "auto" && !requested.is_empty() {
                    return json!({ "content": tried.join("\n"), "is_error": true });
                }
            }
        }
    }
    json!({ "content": format!("all providers failed:\n{}", tried.join("\n")), "is_error": true })
}

fn run_command(argv: &[&str]) -> String {
    let configured: Vec<&str> = providers()
        .iter()
        .filter(|(_, env, _)| std::env::var(env).map(|v| !v.trim().is_empty()).unwrap_or(false))
        .map(|(n, _, _)| *n)
        .collect();
    match argv.first().copied() {
        _ => format!(
            "gray-imagegen {} — image_gen tool. Providers probed in order: openrouter → openai → xai → fal → deepinfra. Configured: {}",
            env!("CARGO_PKG_VERSION"),
            if configured.is_empty() { "none".into() } else { configured.join(", ") }
        ),
    }
}

pub fn handle_imagegen(req: &Value) -> (Option<Value>, bool) {
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let Some(id) = id else {
        return (None, method == "plugin/shutdown");
    };
    let result = match method {
        "plugin/manifest" => manifest_imagegen(),
        "plugin/tools" => json!({ "tools": manifest_imagegen()["tools"] }),
        "tool/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("args").cloned().unwrap_or(Value::Null);
            let session = params.get("session").cloned().unwrap_or(Value::Null);
            call_tool(name, &args, &session)
        }
        "command/run" => {
            let argv: Vec<&str> = params
                .get("argv")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            json!({ "text": run_command(&argv) })
        }
        "plugin/shutdown" => return (Some(json!({ "id": id, "result": {} })), true),
        _ => {
            let error = json!({ "code": -32601, "message": "method not found" });
            return (Some(json!({ "id": id, "error": error })), false);
        }
    };
    (Some(json!({ "id": id, "result": result })), false)
}

fn main_imagegen() -> std::io::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("manifest") {
        println!("{}", manifest_imagegen());
        return Ok(());
    }
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        let (reply, exit) = handle_imagegen(&req);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn call(method: &str, params: Value) -> Value {
        handle_imagegen(&json!({ "id": 1, "method": method, "params": params })).0.unwrap()
    }

    #[test]
    fn manifest_shape() {
        let m = call("plugin/manifest", Value::Null)["result"].clone();
        assert_eq!(m["name"], "imagegen");
        assert_eq!(m["protocol"], "1.3");
        assert_eq!(m["tools"][0]["name"], "image_gen");
    }

    #[test]
    fn plugin_tools_lists_the_tool() {
        let r = call("plugin/tools", json!({}));
        assert_eq!(r["result"]["tools"][0]["name"], "image_gen");
    }

    #[test]
    fn b64_roundtrip() {
        let data = b"hello world, this is a test of base64!";
        assert_eq!(b64_decode(&b64_encode(data)).unwrap(), data);
        assert_eq!(b64_decode("aGVsbG8=").unwrap(), b"hello");
    }

    #[test]
    fn fal_size_mapping() {
        assert_eq!(fal_size_preset("1024x1024"), "square_hd");
        assert_eq!(fal_size_preset("1920x1080"), "landscape_16_9");
        assert_eq!(fal_size_preset("768x1344"), "portrait_16_9");
        assert_eq!(fal_size_preset("bogus"), "square_hd");
    }

    #[test]
    fn no_provider_is_clear_error() {
        for k in ["OPENROUTER_API_KEY", "OPENAI_API_KEY", "XAI_API_KEY", "FAL_KEY", "DEEPINFRA_API_KEY"] {
            unsafe { std::env::remove_var(k) };
        }
        let r = call(
            "tool/call",
            json!({ "name": "image_gen", "args": {"prompt": "a cat"}, "session": {} }),
        );
        assert_eq!(r["result"]["is_error"], true);
        assert!(r["result"]["content"].as_str().unwrap().contains("OPENROUTER_API_KEY"));
    }

    #[test]
    fn explicit_provider_needs_its_key() {
        unsafe { std::env::remove_var("FAL_KEY") };
        let r = call(
            "tool/call",
            json!({ "name": "image_gen", "args": {"prompt": "x", "provider": "fal"}, "session": {} }),
        );
        assert!(r["result"]["content"].as_str().unwrap().contains("FAL_KEY"));
    }

    #[test]
    fn empty_prompt_and_bad_provider() {
        let r = call("tool/call", json!({ "name": "image_gen", "args": {"prompt": " "}, "session": {} }));
        assert_eq!(r["result"]["is_error"], true);
        let r = call("tool/call", json!({ "name": "image_gen", "args": {"prompt": "x", "provider": "zzz"}, "session": {} }));
        assert!(r["result"]["content"].as_str().unwrap().contains("unknown provider"));
    }

    #[test]
    fn mime_sniffing() {
        assert_eq!(mime_for(b"\x89PNGabc", std::path::Path::new("x.png")), "image/png");
        assert_eq!(mime_for(b"\xff\xd8\xffxx", std::path::Path::new("x.bin")), "image/jpeg");
        assert_eq!(mime_for(b"???", std::path::Path::new("x.webp")), "image/webp");
    }

    #[test]
    fn shutdown_replies_then_exits() {
        let (reply, exit) = handle_imagegen(&json!({ "id": 2, "method": "plugin/shutdown" }));
        assert!(reply.is_some() && exit);
    }
}
