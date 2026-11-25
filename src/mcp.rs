//! Pure Rust MCP (Model Context Protocol) stdio server for HarmonyOS NEXT devices.
//! Communicates over stdin/stdout using JSON-RPC 2.0 (newline-delimited).
//! All tracing output goes to stderr so stdout stays clean for the protocol.

use crate::device::{execute_socket_micro_swipe, DeviceManager, TouchSocket};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{info, warn};

// ── tiny pure-Rust Base64 encoder (zero external crates) ──────────────────

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[((n >> 18) & 0x3F) as usize] as char);
        out.push(B64[((n >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 { B64[((n >> 6) & 0x3F) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[(n & 0x3F) as usize] as char } else { '=' });
    }
    out
}

// ── MCP tool definitions ──────────────────────────────────────────────────

fn tool_def(name: &str, desc: &str, props: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": desc,
        "inputSchema": {
            "type": "object",
            "properties": props,
            "required": required,
        }
    })
}

fn all_tools() -> Value {
    json!([
        tool_def("list_devices", "List connected HarmonyOS NEXT devices via HDC", json!({}), &[]),
        tool_def("get_device_info", "Get device model, OS version, brand, and screen resolution", json!({}), &[]),
        tool_def("screenshot", "Capture a screenshot and return as base64 PNG image", json!({}), &[]),
        tool_def("uitree", "Dump the current UI accessibility tree with element bounds, text, and center coordinates. Optionally filter by text content.", json!({
            "filter": {"type": "string", "description": "Optional text to filter UI elements by content match (case-insensitive)"}
        }), &[]),
        tool_def("tap", "Tap at screen coordinates (device pixels: 1128x2444)", json!({
            "x": {"type": "integer", "description": "X coordinate (0-1128)"},
            "y": {"type": "integer", "description": "Y coordinate (0-2444)"}
        }), &["x", "y"]),
        tool_def("double_tap", "Double tap at screen coordinates", json!({
            "x": {"type": "integer", "description": "X coordinate"},
            "y": {"type": "integer", "description": "Y coordinate"}
        }), &["x", "y"]),
        tool_def("long_press", "Long press at screen coordinates", json!({
            "x": {"type": "integer", "description": "X coordinate"},
            "y": {"type": "integer", "description": "Y coordinate"},
            "duration_ms": {"type": "integer", "description": "Hold duration in milliseconds (default: 1500)"}
        }), &["x", "y"]),
        tool_def("swipe", "Swipe from one point to another on screen", json!({
            "x1": {"type": "integer", "description": "Start X"},
            "y1": {"type": "integer", "description": "Start Y"},
            "x2": {"type": "integer", "description": "End X"},
            "y2": {"type": "integer", "description": "End Y"},
            "speed": {"type": "integer", "description": "Swipe speed/velocity (default: 2000)"}
        }), &["x1", "y1", "x2", "y2"]),
        tool_def("scroll", "Scroll the screen in a direction from a center point", json!({
            "x": {"type": "integer", "description": "Center X coordinate for scroll gesture"},
            "y": {"type": "integer", "description": "Center Y coordinate for scroll gesture"},
            "direction": {"type": "string", "enum": ["up", "down", "left", "right"], "description": "Scroll direction (up/down/left/right)"}
        }), &["x", "y", "direction"]),
        tool_def("press_button", "Press a hardware or system button", json!({
            "button": {"type": "string", "enum": ["back", "home", "volume_up", "volume_down", "power"], "description": "Button name"}
        }), &["button"]),
        tool_def("type_text", "Type text into the currently focused input field on the device", json!({
            "text": {"type": "string", "description": "Text to type"}
        }), &["text"]),
        tool_def("list_apps", "List all installed application bundle names", json!({}), &[]),
        tool_def("launch_app", "Launch an application by its bundle name", json!({
            "bundle_name": {"type": "string", "description": "Application bundle name (e.g. com.example.myapp)"}
        }), &["bundle_name"]),
        tool_def("terminate_app", "Force stop a running application", json!({
            "bundle_name": {"type": "string", "description": "Application bundle name to terminate"}
        }), &["bundle_name"]),
        tool_def("shell", "Run an arbitrary shell command on the HarmonyOS device via HDC", json!({
            "command": {"type": "string", "description": "Shell command to execute on device"}
        }), &["command"]),
    ])
}

// ── MCP stdio server main loop ────────────────────────────────────────────

pub async fn run_stdio_mcp(
    target: String,
    hdc_path: String,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("HOS-Device MCP server starting (target={})", target);

    let mgr = DeviceManager::new(target, hdc_path, 50051);
    let mut touch: Option<TouchSocket> = None;

    let stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut lines = stdin.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                warn!("Invalid JSON on stdin: {}", e);
                continue;
            }
        };

        // Notifications (no "id" field) → never respond
        let id = match req.get("id") {
            Some(id) => id.clone(),
            None => continue,
        };

        let method = req["method"].as_str().unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(json!({}));

        let response = match method {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": {
                        "name": "hos-device",
                        "version": "0.1.0"
                    }
                }
            }),
            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": all_tools() }
            }),
            "tools/call" => {
                let tool_name = params["name"].as_str().unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let result = dispatch_tool(tool_name, &args, &mgr, &mut touch).await;
                match result {
                    Ok(content) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "content": content }
                    }),
                    Err(e) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{ "type": "text", "text": format!("Error: {}", e) }],
                            "isError": true
                        }
                    }),
                }
            }
            "ping" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {}
            }),
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("Method not found: {}", method) }
            }),
        };

        let mut out = serde_json::to_string(&response)?;
        out.push('\n');
        stdout.write_all(out.as_bytes()).await?;
        stdout.flush().await?;
    }

    info!("MCP stdin closed, shutting down");
    Ok(())
}

// ── Tool dispatch ─────────────────────────────────────────────────────────

async fn dispatch_tool(
    name: &str,
    args: &Value,
    mgr: &DeviceManager,
    touch: &mut Option<TouchSocket>,
) -> Result<Vec<Value>, String> {
    match name {
        "list_devices" => tool_list_devices(mgr),
        "get_device_info" => tool_get_device_info(mgr),
        "screenshot" => tool_screenshot(mgr),
        "uitree" => tool_uitree(mgr, args),
        "tap" => tool_tap(mgr, args, touch).await,
        "double_tap" => tool_double_tap(mgr, args, touch).await,
        "long_press" => tool_long_press(mgr, args, touch).await,
        "swipe" => tool_swipe(mgr, args, touch).await,
        "scroll" => tool_scroll(mgr, args, touch).await,
        "press_button" => tool_press_button(mgr, args, touch).await,
        "type_text" => tool_type_text(mgr, args),
        "list_apps" => tool_list_apps(mgr),
        "launch_app" => tool_launch_app(mgr, args),
        "terminate_app" => tool_terminate_app(mgr, args),
        "shell" => tool_shell(mgr, args),
        _ => Err(format!("Unknown tool: {}", name)),
    }
}

fn text_content(text: &str) -> Vec<Value> {
    vec![json!({"type": "text", "text": text})]
}

// ── Tool implementations ──────────────────────────────────────────────────

fn tool_list_devices(mgr: &DeviceManager) -> Result<Vec<Value>, String> {
    let out = mgr.exec_hdc(&["list", "targets", "-v"])?;
    Ok(text_content(&out))
}

fn tool_get_device_info(mgr: &DeviceManager) -> Result<Vec<Value>, String> {
    let model = mgr.shell("param get const.product.model").unwrap_or_default();
    let version = mgr
        .shell("param get const.product.software.version")
        .unwrap_or_default();
    let brand = mgr
        .shell("param get const.product.brand")
        .unwrap_or_default();
    let name = mgr
        .shell("param get const.product.name")
        .unwrap_or_default();
    let info = format!(
        "Model: {}\nBrand: {}\nName: {}\nSoftware Version: {}\nResolution: {}x{}",
        model.trim(),
        brand.trim(),
        name.trim(),
        version.trim(),
        mgr.width,
        mgr.height
    );
    Ok(text_content(&info))
}

fn tool_screenshot(mgr: &DeviceManager) -> Result<Vec<Value>, String> {
    let remote = "/data/local/tmp/mcp_cap.png";
    let local = "/tmp/hos_mcp_cap.png";

    mgr.shell(&format!("uitest screenCap -p {}", remote))?;
    mgr.exec_hdc(&["file", "recv", remote, local])?;

    let data =
        std::fs::read(local).map_err(|e| format!("Failed to read screenshot: {}", e))?;
    let b64 = base64_encode(&data);

    Ok(vec![
        json!({"type": "image", "data": b64, "mimeType": "image/png"}),
        json!({"type": "text", "text": format!("Screenshot captured: {}x{} ({} bytes)", mgr.width, mgr.height, data.len())}),
    ])
}

fn tool_uitree(mgr: &DeviceManager, args: &Value) -> Result<Vec<Value>, String> {
    let remote = "/data/local/tmp/mcp_tree.json";
    let local = "/tmp/hos_mcp_tree.json";

    mgr.shell(&format!("uitest dumpLayout -p {}", remote))?;
    mgr.exec_hdc(&["file", "recv", remote, local])?;

    let raw =
        std::fs::read_to_string(local).map_err(|e| format!("Failed to read UI tree: {}", e))?;

    let filter = args
        .get("filter")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if filter.is_empty() {
        let summary = summarize_ui_tree(&raw);
        Ok(text_content(&summary))
    } else {
        let filtered = filter_ui_tree(&raw, filter);
        Ok(text_content(&filtered))
    }
}

// ── UI tree helpers ───────────────────────────────────────────────────────

fn summarize_ui_tree(raw: &str) -> String {
    let tree: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return raw.to_string(),
    };

    let mut elements = Vec::new();
    collect_elements(&tree, &mut elements);

    let mut out = format!("UI Tree: {} interactive elements\n\n", elements.len());
    for (i, el) in elements.iter().enumerate() {
        out.push_str(&format!("{}. {}\n", i + 1, el));
    }
    out
}

fn collect_elements(node: &Value, out: &mut Vec<String>) {
    if let Some(attrs) = node.as_object() {
        let text = attrs
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let desc = attrs
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let typ = attrs
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let bounds = attrs
            .get("bounds")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let clickable = attrs
            .get("clickable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let label = if !text.is_empty() {
            text
        } else if !desc.is_empty() {
            desc
        } else {
            ""
        };

        if !label.is_empty() || clickable {
            let center = parse_bounds_center(bounds);
            let click_tag = if clickable { " [clickable]" } else { "" };
            out.push(format!(
                "[{}] \"{}\"{} @ ({}) bounds={}",
                typ, label, click_tag, center, bounds
            ));
        }

        if let Some(children) = attrs.get("children").and_then(|v| v.as_array()) {
            for child in children {
                collect_elements(child, out);
            }
        }
    }
}

fn parse_bounds_center(bounds: &str) -> String {
    // Format: "[x1,y1][x2,y2]"
    let nums: Vec<i32> = bounds
        .replace('[', " ")
        .replace(']', " ")
        .replace(',', " ")
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
        .collect();
    if nums.len() == 4 {
        format!("{},{}", (nums[0] + nums[2]) / 2, (nums[1] + nums[3]) / 2)
    } else {
        "?,?".to_string()
    }
}

fn filter_ui_tree(raw: &str, filter: &str) -> String {
    let tree: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return "Failed to parse UI tree JSON".to_string(),
    };

    let mut matches = Vec::new();
    filter_elements(&tree, filter, &mut matches);

    if matches.is_empty() {
        format!("No elements matching \"{}\"", filter)
    } else {
        let mut out = format!("{} elements matching \"{}\":\n\n", matches.len(), filter);
        for (i, m) in matches.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", i + 1, m));
        }
        out
    }
}

fn filter_elements(node: &Value, filter: &str, out: &mut Vec<String>) {
    if let Some(attrs) = node.as_object() {
        let text = attrs
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let desc = attrs
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let typ = attrs
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let bounds = attrs
            .get("bounds")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let filter_lower = filter.to_lowercase();

        if text.to_lowercase().contains(&filter_lower)
            || desc.to_lowercase().contains(&filter_lower)
        {
            let center = parse_bounds_center(bounds);
            let label = if !text.is_empty() { text } else { desc };
            out.push(format!(
                "[{}] \"{}\" @ ({}) bounds={}",
                typ, label, center, bounds
            ));
        }

        if let Some(children) = attrs.get("children").and_then(|v| v.as_array()) {
            for child in children {
                filter_elements(child, filter, out);
            }
        }
    }
}

// ── Touch/Input tools ─────────────────────────────────────────────────────

async fn ensure_touch(mgr: &DeviceManager, touch: &mut Option<TouchSocket>) -> bool {
    if touch.is_some() {
        return true;
    }
    match TouchSocket::connect(mgr.touch_port).await {
        Ok(s) => {
            *touch = Some(s);
            true
        }
        Err(e) => {
            warn!(
                "TouchSocket connect failed (will use shell fallback): {}",
                e
            );
            false
        }
    }
}

async fn tool_tap(
    mgr: &DeviceManager,
    args: &Value,
    touch: &mut Option<TouchSocket>,
) -> Result<Vec<Value>, String> {
    let x = args["x"].as_u64().ok_or("missing x")? as u32;
    let y = args["y"].as_u64().ok_or("missing y")? as u32;

    if ensure_touch(mgr, touch).await {
        let s = touch.as_mut().unwrap();
        if s.ensure_driver().await.is_ok() {
            if s.gesture("touchDown", x, y).await.is_ok() {
                tokio::time::sleep(Duration::from_millis(12)).await;
                let _ = s.gesture("touchUp", x, y).await;
                return Ok(text_content(&format!("Tapped at ({}, {})", x, y)));
            }
        }
        *touch = None;
    }

    // Shell fallback
    mgr.shell(&format!("uitest uiInput click {} {}", x, y))?;
    Ok(text_content(&format!("Tapped at ({}, {}) [shell]", x, y)))
}

async fn tool_double_tap(
    mgr: &DeviceManager,
    args: &Value,
    touch: &mut Option<TouchSocket>,
) -> Result<Vec<Value>, String> {
    let x = args["x"].as_u64().ok_or("missing x")? as u32;
    let y = args["y"].as_u64().ok_or("missing y")? as u32;

    if ensure_touch(mgr, touch).await {
        let s = touch.as_mut().unwrap();
        if s.ensure_driver().await.is_ok() {
            let ok = s.gesture("touchDown", x, y).await.is_ok();
            if ok {
                tokio::time::sleep(Duration::from_millis(10)).await;
                let _ = s.gesture("touchUp", x, y).await;
                tokio::time::sleep(Duration::from_millis(60)).await;
                let _ = s.gesture("touchDown", x, y).await;
                tokio::time::sleep(Duration::from_millis(10)).await;
                let _ = s.gesture("touchUp", x, y).await;
                return Ok(text_content(&format!("Double-tapped at ({}, {})", x, y)));
            }
        }
        *touch = None;
    }

    mgr.shell(&format!("uitest uiInput doubleClick {} {}", x, y))?;
    Ok(text_content(&format!(
        "Double-tapped at ({}, {}) [shell]",
        x, y
    )))
}

async fn tool_long_press(
    mgr: &DeviceManager,
    args: &Value,
    touch: &mut Option<TouchSocket>,
) -> Result<Vec<Value>, String> {
    let x = args["x"].as_u64().ok_or("missing x")? as u32;
    let y = args["y"].as_u64().ok_or("missing y")? as u32;
    let dur = args
        .get("duration_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(1500);

    if ensure_touch(mgr, touch).await {
        let s = touch.as_mut().unwrap();
        if s.ensure_driver().await.is_ok() {
            if s.gesture("touchDown", x, y).await.is_ok() {
                tokio::time::sleep(Duration::from_millis(dur)).await;
                let _ = s.gesture("touchUp", x, y).await;
                return Ok(text_content(&format!(
                    "Long-pressed at ({}, {}) for {}ms",
                    x, y, dur
                )));
            }
        }
        *touch = None;
    }

    mgr.shell(&format!("uitest uiInput longClick {} {}", x, y))?;
    Ok(text_content(&format!(
        "Long-pressed at ({}, {}) [shell]",
        x, y
    )))
}

async fn tool_swipe(
    mgr: &DeviceManager,
    args: &Value,
    touch: &mut Option<TouchSocket>,
) -> Result<Vec<Value>, String> {
    let x1 = args["x1"].as_u64().ok_or("missing x1")? as u32;
    let y1 = args["y1"].as_u64().ok_or("missing y1")? as u32;
    let x2 = args["x2"].as_u64().ok_or("missing x2")? as u32;
    let y2 = args["y2"].as_u64().ok_or("missing y2")? as u32;
    let speed = args.get("speed").and_then(|v| v.as_u64()).unwrap_or(2000);

    if ensure_touch(mgr, touch).await {
        let s = touch.as_mut().unwrap();
        if s.ensure_driver().await.is_ok() {
            if execute_socket_micro_swipe(s, x1, y1, x2, y2, 8, 10).await.is_ok() {
                return Ok(text_content(&format!(
                    "Swiped ({},{}) -> ({},{}) speed={}",
                    x1, y1, x2, y2, speed
                )));
            }
        }
        *touch = None;
    }

    let vel = speed.clamp(300, 35000);
    mgr.shell(&format!(
        "uitest uiInput swipe {} {} {} {} {}",
        x1, y1, x2, y2, vel
    ))?;
    Ok(text_content(&format!(
        "Swiped ({},{}) -> ({},{}) speed={} [shell]",
        x1, y1, x2, y2, speed
    )))
}

async fn tool_scroll(
    mgr: &DeviceManager,
    args: &Value,
    touch: &mut Option<TouchSocket>,
) -> Result<Vec<Value>, String> {
    let x = args["x"].as_u64().ok_or("missing x")? as u32;
    let y = args["y"].as_u64().ok_or("missing y")? as u32;
    let direction = args["direction"].as_str().ok_or("missing direction")?;

    let dist: i32 = 600;
    let h = mgr.height as i32;
    let w = mgr.width as i32;

    // "scroll down" = see content below = finger swipes up (y decreases)
    // "scroll up"   = see content above = finger swipes down (y increases)
    let (sx1, sy1, sx2, sy2) = match direction {
        "up" => (
            x,
            (y as i32 - dist / 2).clamp(80, h - 80) as u32,
            x,
            (y as i32 + dist / 2).clamp(80, h - 80) as u32,
        ),
        "down" => (
            x,
            (y as i32 + dist / 2).clamp(80, h - 80) as u32,
            x,
            (y as i32 - dist / 2).clamp(80, h - 80) as u32,
        ),
        "left" => (
            (x as i32 - dist / 2).clamp(80, w - 80) as u32,
            y,
            (x as i32 + dist / 2).clamp(80, w - 80) as u32,
            y,
        ),
        "right" => (
            (x as i32 + dist / 2).clamp(80, w - 80) as u32,
            y,
            (x as i32 - dist / 2).clamp(80, w - 80) as u32,
            y,
        ),
        _ => return Err(format!("Invalid direction: {}", direction)),
    };

    if ensure_touch(mgr, touch).await {
        let s = touch.as_mut().unwrap();
        if s.ensure_driver().await.is_ok() {
            if execute_socket_micro_swipe(s, sx1, sy1, sx2, sy2, 6, 8).await.is_ok() {
                return Ok(text_content(&format!(
                    "Scrolled {} at ({}, {})",
                    direction, x, y
                )));
            }
        }
        *touch = None;
    }

    mgr.shell(&format!(
        "uitest uiInput fling {} {} {} {} 3000",
        sx1, sy1, sx2, sy2
    ))?;
    Ok(text_content(&format!(
        "Scrolled {} at ({}, {}) [shell]",
        direction, x, y
    )))
}

async fn tool_press_button(
    mgr: &DeviceManager,
    args: &Value,
    touch: &mut Option<TouchSocket>,
) -> Result<Vec<Value>, String> {
    let button = args["button"].as_str().ok_or("missing button")?;

    if ensure_touch(mgr, touch).await {
        let s = touch.as_mut().unwrap();
        if s.ensure_driver().await.is_ok() {
            let result = match button {
                "back" => s.call_driver("Driver.pressBack", "[]").await,
                "home" => s.call_driver("Driver.triggerKey", "[1]").await,
                "volume_up" => s.call_driver("Driver.triggerKey", "[16]").await,
                "volume_down" => s.call_driver("Driver.triggerKey", "[17]").await,
                "power" => s.call_driver("Driver.triggerKey", "[18]").await,
                _ => Err(format!("Unknown button: {}", button)),
            };
            if result.is_ok() {
                return Ok(text_content(&format!("Pressed {}", button)));
            }
        }
        *touch = None;
    }

    let key = match button {
        "back" => "Back",
        "home" => "Home",
        "volume_up" => "VolumeUp",
        "volume_down" => "VolumeDown",
        "power" => "Power",
        _ => return Err(format!("Unknown button: {}", button)),
    };
    mgr.shell(&format!("uitest uiInput keyEvent {}", key))?;
    Ok(text_content(&format!("Pressed {} [shell]", button)))
}

fn tool_type_text(mgr: &DeviceManager, args: &Value) -> Result<Vec<Value>, String> {
    let text = args["text"].as_str().ok_or("missing text")?;
    mgr.shell(&format!("uitest uiInput inputText {}", text))?;
    Ok(text_content(&format!("Typed: {}", text)))
}

fn tool_list_apps(mgr: &DeviceManager) -> Result<Vec<Value>, String> {
    let out = mgr.shell("bm dump -a")?;
    Ok(text_content(&out))
}

fn tool_launch_app(mgr: &DeviceManager, args: &Value) -> Result<Vec<Value>, String> {
    let bundle = args["bundle_name"].as_str().ok_or("missing bundle_name")?;

    // Get main ability name from bundle metadata
    let dump = mgr.shell(&format!("bm dump -n {}", bundle))?;
    let ability = extract_main_ability(&dump).unwrap_or_else(|| "MainAbility".to_string());

    let result = mgr.shell(&format!("aa start -a {} -b {}", ability, bundle))?;
    Ok(text_content(&format!(
        "Launched {} (ability={})\n{}",
        bundle,
        ability,
        result.trim()
    )))
}

fn extract_main_ability(dump: &str) -> Option<String> {
    // Try JSON parse
    if let Ok(v) = serde_json::from_str::<Value>(dump) {
        // Check common HarmonyOS bundle metadata paths
        for path in &[
            "/hapModuleInfos/0/mainElementName",
            "/mainElementName",
        ] {
            if let Some(name) = v.pointer(path).and_then(|v| v.as_str()) {
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
    }

    // Fallback: text search
    for line in dump.lines() {
        let trimmed = line.trim();
        if trimmed.contains("mainElementName") {
            if let Some(pos) = trimmed.find(':') {
                let val = trimmed[pos + 1..]
                    .trim()
                    .trim_matches(|c: char| c == '"' || c == ',' || c == ' ');
                if !val.is_empty() {
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

fn tool_terminate_app(mgr: &DeviceManager, args: &Value) -> Result<Vec<Value>, String> {
    let bundle = args["bundle_name"].as_str().ok_or("missing bundle_name")?;
    let result = mgr.shell(&format!("aa force-stop {}", bundle))?;
    Ok(text_content(&format!(
        "Terminated {}\n{}",
        bundle,
        result.trim()
    )))
}

fn tool_shell(mgr: &DeviceManager, args: &Value) -> Result<Vec<Value>, String> {
    let command = args["command"].as_str().ok_or("missing command")?;
    let out = mgr.shell(command)?;
    Ok(text_content(&out))
}
