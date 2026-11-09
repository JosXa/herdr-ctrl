//! `herdr-ctrl mcp`: one MCP tool that runs any herdr-ctrl command, over stdio.

use anyhow::Result;
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::process::Command;

const ABOUT: &str = "Drive a Herdr window. `args` are the herdr-ctrl command line after the program name, such as [\"snapshot\", \"-i\"], [\"click\", \"@e12\"] or [\"spawn\", \"--build\", \"/path/to/herdr-app\"]. Run [\"skill\"] once for the full guide. Results are JSON; a screenshot also returns the image.";

pub fn serve() -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = message.get("id").cloned() else {
            continue;
        };
        let method = message["method"].as_str().unwrap_or_default();
        let reply = match method {
            "initialize" => json!({
                "protocolVersion": message["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "herdr-ctrl", "version": env!("CARGO_PKG_VERSION")},
            }),
            "ping" => json!({}),
            "tools/list" => json!({"tools": [{
                "name": "herdr_ctrl",
                "description": ABOUT,
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "args": {"type": "array", "items": {"type": "string"}},
                        "on": {"type": "string", "description": "live, or a hidden instance's name"},
                    },
                    "required": ["args"],
                },
            }]}),
            "tools/call" => call(&message["params"]["arguments"])?,
            _ => {
                writeln!(
                    stdout,
                    "{}",
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("unknown method {method}")}})
                )?;
                stdout.flush()?;
                continue;
            }
        };
        writeln!(stdout, "{}", json!({"jsonrpc": "2.0", "id": id, "result": reply}))?;
        stdout.flush()?;
    }
    Ok(())
}

fn call(arguments: &Value) -> Result<Value> {
    let args: Vec<String> = arguments["args"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
        .unwrap_or_default();
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("--json");
    if let Some(on) = arguments["on"].as_str() {
        command.args(["--on", on]);
    }
    let output = command.args(&args).output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let mut content = vec![json!({"type": "text", "text": if stderr.is_empty() { stdout.clone() } else { format!("{stdout}{stderr}") }})];
    if output.status.success()
        && args.first().is_some_and(|a| a == "screenshot")
        && let Ok(info) = serde_json::from_str::<Value>(&stdout)
        && let Some(path) = info["path"].as_str()
        && let Ok(bytes) = std::fs::read(path)
    {
        content.push(json!({"type": "image", "data": base64(&bytes), "mimeType": "image/png"}));
    }
    Ok(json!({"content": content, "isError": !output.status.success()}))
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_pads_like_the_standard() {
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foo"), "Zm9v");
        assert_eq!(super::base64(b"foobar"), "Zm9vYmFy");
    }
}
