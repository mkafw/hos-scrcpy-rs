use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tracing::{info, warn};

const AGENT_SO_BYTES: &[u8] = include_bytes!("../resources/agent.so");
const SCREEN_CASTING_SO_BYTES: &[u8] = include_bytes!("../resources/libscreen_casting.z.so");

#[derive(Debug, Clone)]
pub enum InputCommand {
    TouchDown { x: u32, y: u32 },
    TouchMove { x: u32, y: u32 },
    TouchUp { x: u32, y: u32 },
    Click { x: u32, y: u32 },
    Wheel {
        x: u32,
        y: u32,
        delta_y: i32,
        sensitivity: f32,
    },
    Swipe {
        x1: u32,
        y1: u32,
        x2: u32,
        y2: u32,
        velocity: u32,
    },
    Key(String),
    Text(String),
}

#[derive(Clone)]
pub struct DeviceManager {
    pub target: String,
    pub hdc_path: String,
    pub grpc_port: u16,
    pub touch_port: u16,
    pub width: u32,
    pub height: u32,
    input_tx: mpsc::Sender<InputCommand>,
    input_rx: Arc<Mutex<Option<mpsc::Receiver<InputCommand>>>>,
}

impl DeviceManager {
    /// Creates a new DeviceManager and initializes its non-blocking touch command channel.
    pub fn new(target: String, hdc_path: String, grpc_port: u16) -> Self {
        let (input_tx, input_rx) = mpsc::channel(256);
        Self {
            target,
            hdc_path,
            grpc_port,
            touch_port: grpc_port + 1,
            width: 1128,
            height: 2444,
            input_tx,
            input_rx: Arc::new(Mutex::new(Some(input_rx))),
        }
    }

    pub fn exec_hdc(&self, args: &[&str]) -> Result<String, String> {
        let mut cmd = Command::new(&self.hdc_path);
        if !self.target.is_empty() {
            cmd.arg("-t").arg(&self.target);
        }
        cmd.args(args);

        let output = cmd
            .output()
            .map_err(|e| format!("Failed to exec hdc: {}", e))?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() && !stderr.is_empty() {
            return Err(format!("HDC error: {}", stderr.trim()));
        }
        Ok(stdout)
    }

    pub fn shell(&self, command: &str) -> Result<String, String> {
        self.exec_hdc(&["shell", command])
    }

    fn ensure_remote_so(&self, remote_name: &str, bytes: &[u8]) -> Result<(), String> {
        let remote_path = format!("/data/local/tmp/{}", remote_name);
        let check_cmd = format!("[ -s {} ] && echo OK || echo MISSING", remote_path);
        if self.shell(&check_cmd).unwrap_or_default().contains("OK") {
            return Ok(());
        }

        info!("Deploying {} to device ({} bytes)...", remote_path, bytes.len());
        let local_tmp: PathBuf = std::env::temp_dir().join(format!("hos_scrcpy_{}", remote_name));
        std::fs::write(&local_tmp, bytes)
            .map_err(|e| format!("Failed to write temp {}: {}", local_tmp.display(), e))?;
        let res = self.exec_hdc(&[
            "file",
            "send",
            local_tmp.to_string_lossy().as_ref(),
            &remote_path,
        ]);
        let _ = std::fs::remove_file(&local_tmp);
        res?;
        let _ = self.shell(&format!("chmod 755 {}", remote_path));
        Ok(())
    }

    fn clean_fports(&self) {
        let fport_ls = self.exec_hdc(&["fport", "ls"]).unwrap_or_default();
        for line in fport_ls.lines() {
            let line = line.trim();
            if line.contains("scrcpy_grpc_socket") || line.contains("uitest_socket") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 3 {
                    info!("Cleaning stale fport rule: {} {}", parts[1], parts[2]);
                    let _ = self.exec_hdc(&["fport", "rm", parts[1], parts[2]]);
                }
            }
        }
    }

    fn setup_fport(&self, local_port: u16, remote_socket: &str) -> Result<(), String> {
        let local = format!("tcp:{}", local_port);
        let _ = self.exec_hdc(&["fport", "rm", &local, remote_socket]);
        info!("Setting up port forwarding: {} -> {}", local, remote_socket);
        let res = self.exec_hdc(&["fport", &local, remote_socket])?;
        if !res.contains("success") && !res.contains("OK") {
            warn!("fport output: {}", res.trim());
        }
        Ok(())
    }

    fn start_touch_daemon(&self) -> Result<(), String> {
        self.shell("cd /data/local/tmp && /system/bin/uitest start-daemon singleness --extension-name agent.so >/dev/null 2>&1 &")?;
        Ok(())
    }

    /// Initializes device connection, both video & touch daemons, port forwarding, and spawns the async touch actor.
    pub async fn ensure_daemon(&self) -> Result<(), String> {
        info!("Ensuring device is connected and screen casting/touch daemons are running...");

        // 1. Verify target connection, auto-reconnect if needed
        let mut targets = self.exec_hdc(&["list", "targets"])?;
        if !targets.contains(&self.target) && !self.target.is_empty() {
            info!("Device {} not in targets, attempting tconn...", self.target);
            let _ = self.exec_hdc(&["tconn", &self.target]);
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            targets = self.exec_hdc(&["list", "targets"])?;
            if !targets.contains(&self.target) {
                return Err(format!(
                    "Device {} not found after tconn: {}",
                    self.target, targets
                ));
            }
        }

        // 2. Ensure required native extensions exist on device
        self.ensure_remote_so("libscreen_casting.z.so", SCREEN_CASTING_SO_BYTES)?;
        self.ensure_remote_so("agent.so", AGENT_SO_BYTES)?;

        // 3. Clean stale fports & uitest processes
        self.clean_fports();
        info!("Ensuring fresh screen casting & real-time touch daemons on device...");
        let _ = self.shell("killall -9 uitest; pkill -9 uitest");
        tokio::time::sleep(Duration::from_millis(500)).await;

        // 4. Start video gRPC extension daemon & real-time touch agent daemon side by side
        let video_cmd = "cd /data/local/tmp && /system/bin/uitest start-daemon singleness --extension-name libscreen_casting.z.so -scale 1 -frameRate 60 -bitRate 20000000 -p 5000 -iFrameInterval 2000 -repeatInterval 33 &";
        self.shell(video_cmd)?;
        if let Err(e) = self.start_touch_daemon() {
            warn!("Failed to start uitest touch agent daemon (will fallback to shell): {}", e);
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;

        // 5. Keep screen awake and turn on display
        info!("Setting screen timeout to 1 hour and waking up display...");
        let _ = self.shell("power-shell timeout -o 3600000");
        let _ = self.shell("power-shell wakeup");

        // 6. Set up both fports
        self.setup_fport(self.grpc_port, "localabstract:scrcpy_grpc_socket")?;
        self.setup_fport(self.touch_port, "localabstract:uitest_socket")?;

        // 7. Spawn the persistent TCP touch actor once
        if let Some(rx) = self.input_rx.lock().await.take() {
            let mgr = self.clone();
            tokio::spawn(async move {
                run_touch_actor(mgr, rx).await;
            });
        }

        Ok(())
    }

    /// Enqueues an input command without blocking the WebSocket receive loop.
    pub fn send_input(&self, cmd: InputCommand) {
        if let Err(e) = self.input_tx.try_send(cmd) {
            warn!("Input channel full or closed: {}", e);
        }
    }

    pub async fn wakeup(&self) -> Result<(), String> {
        let mgr = self.clone();
        tokio::task::spawn_blocking(move || {
            let _ = mgr.shell("power-shell timeout -o 3600000; power-shell wakeup");
        })
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    }
}

pub(crate) struct TouchSocket {
    stream: TcpStream,
    driver_ready: bool,
}

impl TouchSocket {
    pub(crate) async fn connect(port: u16) -> Result<Self, String> {
        let addr = format!("127.0.0.1:{}", port);
        let stream = tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(&addr))
            .await
            .map_err(|_| format!("Connect timeout to {}", addr))?
            .map_err(|e| format!("Connect failed to {}: {}", addr, e))?;
        let _ = stream.set_nodelay(true);
        Ok(Self {
            stream,
            driver_ready: false,
        })
    }

    pub(crate) async fn rpc(&mut self, payload: &str) -> Result<String, String> {
        tokio::time::timeout(Duration::from_millis(1500), async {
            self.stream
                .write_all(payload.as_bytes())
                .await
                .map_err(|e| format!("Touch write error: {}", e))?;
            read_json_frame(&mut self.stream).await
        })
        .await
        .map_err(|_| "Touch RPC timeout".to_string())?
    }

    pub(crate) async fn gesture(&mut self, api: &str, x: u32, y: u32) -> Result<(), String> {
        let payload = format!(
            r#"{{"module":"com.ohos.devicetest.hypiumApiHelper","method":"Gestures","params":{{"api":"{}","args":{{"x":{},"y":{}}}}}}}"#,
            api, x, y
        );
        self.rpc(&payload).await?;
        Ok(())
    }

    pub(crate) async fn ensure_driver(&mut self) -> Result<(), String> {
        if self.driver_ready {
            return Ok(());
        }
        let payload = r#"{"module":"com.ohos.devicetest.hypiumApiHelper","method":"callHypiumApi","params":{"api":"Driver.create","this":null,"args":[]}}"#;
        let resp = self.rpc(payload).await?;
        if resp.contains("Driver#") {
            self.driver_ready = true;
            Ok(())
        } else {
            Err(format!("Unexpected Driver.create reply: {}", resp))
        }
    }

    pub(crate) async fn call_driver(&mut self, api: &str, args_json: &str) -> Result<(), String> {
        self.ensure_driver().await?;
        let payload = format!(
            r#"{{"module":"com.ohos.devicetest.hypiumApiHelper","method":"callHypiumApi","params":{{"api":"{}","this":"Driver#0","args":{}}}}}"#,
            api, args_json
        );
        let resp = self.rpc(&payload).await?;
        if resp.contains("\"exception\"") {
            return Err(format!("Driver RPC exception: {}", resp));
        }
        Ok(())
    }
}

/// Reads bytes from TCP until one complete JSON object (`{ ... }`) is balanced.
pub(crate) async fn read_json_frame(stream: &mut TcpStream) -> Result<String, String> {
    let mut buf = Vec::with_capacity(256);
    let mut tmp = [0u8; 2048];
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    let mut started = false;

    loop {
        let n = stream
            .read(&mut tmp)
            .await
            .map_err(|e| format!("Touch read error: {}", e))?;
        if n == 0 {
            return Err("Touch socket closed by peer".to_string());
        }
        for &b in &tmp[..n] {
            buf.push(b);
            if !started {
                if b == b'{' {
                    started = true;
                    depth = 1;
                }
                continue;
            }
            if escaped {
                escaped = false;
                continue;
            }
            if b == b'\\' && in_str {
                escaped = true;
                continue;
            }
            if b == b'"' {
                in_str = !in_str;
                continue;
            }
            if !in_str {
                if b == b'{' {
                    depth += 1;
                } else if b == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(String::from_utf8_lossy(&buf).into_owned());
                    }
                }
            }
        }
    }
}

async fn run_touch_actor(mgr: DeviceManager, mut rx: mpsc::Receiver<InputCommand>) {
    let mut sock: Option<TouchSocket> = None;
    let mut pending_next: Option<InputCommand> = None;
    let mut keepalive = tokio::time::interval(Duration::from_secs(90));
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        // Pull next queued item (or cached peeked item) while coalescing consecutive Move events
        let mut cmd = if let Some(next) = pending_next.take() {
            next
        } else {
            tokio::select! {
                maybe = rx.recv() => match maybe {
                    Some(c) => c,
                    None => break,
                },
                _ = keepalive.tick() => {
                    if let Some(s) = sock.as_mut() {
                        // Prevent agent.so's 5-minute idle timeout shutdown
                        if let Err(e) = s.call_driver("Driver.getDisplaySize", "[]").await {
                            warn!("Touch keepalive failed ({}), clearing socket", e);
                            sock = None;
                        }
                    }
                    continue;
                }
            }
        };

        // Drain stale intermediate Move events so phone always tracks the latest mouse position cleanly
        if matches!(cmd, InputCommand::TouchMove { .. }) {
            while let Ok(next) = rx.try_recv() {
                if matches!(next, InputCommand::TouchMove { .. }) {
                    cmd = next;
                } else {
                    pending_next = Some(next);
                    break;
                }
            }
        }

        // Lazy connect / reconnect to @uitest_socket if disconnected
        if sock.is_none() {
            sock = connect_or_recover(&mgr).await;
            if sock.is_some() {
                info!("Persistent TCP touch stream connected on port {}", mgr.touch_port);
            }
        }

        // Fast-path over persistent TCP socket; if socket fails once, recover and retry once
        if !dispatch_over_socket(&mut sock, &cmd, &mgr).await {
            sock = connect_or_recover(&mgr).await;
            if !dispatch_over_socket(&mut sock, &cmd, &mgr).await {
                dispatch_shell_fallback(&mgr, &cmd).await;
            }
        }
    }
}

async fn connect_or_recover(mgr: &DeviceManager) -> Option<TouchSocket> {
    if let Ok(s) = TouchSocket::connect(mgr.touch_port).await {
        return Some(s);
    }

    // Ensure touch daemon & fport without disturbing libscreen_casting.z.so video daemon
    let mgr_clone = mgr.clone();
    let _ = tokio::task::spawn_blocking(move || {
        let alive = mgr_clone
            .shell("grep -a uitest_socket /proc/net/unix")
            .unwrap_or_default()
            .contains("uitest_socket");
        if !alive {
            warn!("Touch agent daemon stopped; restarting agent.so...");
            let _ = mgr_clone.start_touch_daemon();
            std::thread::sleep(Duration::from_millis(1200));
        }
        let _ = mgr_clone.setup_fport(mgr_clone.touch_port, "localabstract:uitest_socket");
    })
    .await;

    TouchSocket::connect(mgr.touch_port).await.ok()
}

/// Returns `true` if handled cleanly (including shell-only ops), `false` if TCP socket failed.
async fn dispatch_over_socket(
    sock_opt: &mut Option<TouchSocket>,
    cmd: &InputCommand,
    mgr: &DeviceManager,
) -> bool {
    // Key actions requiring non-agent APIs (recent tasks gesture, wakeup) use direct non-blocking shell
    if let InputCommand::Key(k) = cmd {
        match k.as_str() {
            "task" | "recent" => {
                let start_x = mgr.width / 2;
                let start_y = mgr.height.saturating_sub(40);
                let end_y = (mgr.height as f32 * 0.65) as u32;
                let shell_cmd = format!(
                    "uinput -T -m {} {} {} {} -k 400 200",
                    start_x, start_y, start_x, end_y
                );
                let m = mgr.clone();
                tokio::spawn(async move {
                    let _ = tokio::task::spawn_blocking(move || m.shell(&shell_cmd)).await;
                });
                return true;
            }
            "wakeup" => {
                let _ = mgr.wakeup().await;
                return true;
            }
            _ => {}
        }
    }
    if matches!(cmd, InputCommand::Text(_)) {
        dispatch_shell_fallback(mgr, cmd).await;
        return true;
    }

    let Some(s) = sock_opt.as_mut() else {
        return false;
    };

    let res = match cmd {
        InputCommand::TouchDown { x, y } => s.gesture("touchDown", *x, *y).await,
        InputCommand::TouchMove { x, y } => s.gesture("touchMove", *x, *y).await,
        InputCommand::TouchUp { x, y } => s.gesture("touchUp", *x, *y).await,
        InputCommand::Click { x, y } => {
            if let Err(e) = s.gesture("touchDown", *x, *y).await {
                Err(e)
            } else {
                tokio::time::sleep(Duration::from_millis(25)).await;
                s.gesture("touchUp", *x, *y).await
            }
        }
        InputCommand::Wheel {
            x,
            y,
            delta_y,
            sensitivity,
        } => {
            let steps = 4i32;
            let raw_dist = ((delta_y.abs() as f32 / 100.0).clamp(0.6, 2.2) * 320.0 * sensitivity)
                .clamp(160.0, 960.0) as i32;
            let dir = if *delta_y > 0 { -1 } else { 1 }; // scroll down = finger moves up
            let half = raw_dist / 2;
            let y1 = (*y as i32 - dir * half).clamp(80, mgr.height as i32 - 80) as u32;
            let y2 = (*y as i32 + dir * half).clamp(80, mgr.height as i32 - 80) as u32;
            execute_socket_micro_swipe(s, *x, y1, *x, y2, steps, 7).await
        }
        InputCommand::Swipe {
            x1,
            y1,
            x2,
            y2,
            velocity,
        } => {
            let steps = if *velocity > 5000 { 4 } else { 7 };
            execute_socket_micro_swipe(s, *x1, *y1, *x2, *y2, steps, 8).await
        }
        InputCommand::Key(k) => match k.as_str() {
            "back" => s.call_driver("Driver.pressBack", "[]").await,
            "home" => s.call_driver("Driver.triggerKey", "[1]").await,
            "vol_up" | "volume_up" => s.call_driver("Driver.triggerKey", "[16]").await,
            "vol_down" | "volume_down" => s.call_driver("Driver.triggerKey", "[17]").await,
            "power" => s.call_driver("Driver.triggerKey", "[18]").await,
            _ => Ok(()),
        },
        InputCommand::Text(_) => unreachable!(),
    };

    if let Err(e) = res {
        warn!("Touch socket command failed ({:?}): {}", cmd, e);
        *sock_opt = None;
        false
    } else {
        true
    }
}

pub(crate) async fn execute_socket_micro_swipe(
    s: &mut TouchSocket,
    x1: u32,
    y1: u32,
    x2: u32,
    y2: u32,
    steps: i32,
    step_ms: u64,
) -> Result<(), String> {
    s.gesture("touchDown", x1, y1).await?;
    for i in 1..=steps {
        tokio::time::sleep(Duration::from_millis(step_ms)).await;
        let t = i as f32 / steps as f32;
        let cx = (x1 as f32 + (x2 as f32 - x1 as f32) * t).round() as u32;
        let cy = (y1 as f32 + (y2 as f32 - y1 as f32) * t).round() as u32;
        s.gesture("touchMove", cx, cy).await?;
    }
    s.gesture("touchUp", x2, y2).await
}

async fn dispatch_shell_fallback(mgr: &DeviceManager, cmd: &InputCommand) {
    let shell_cmd = match cmd {
        InputCommand::TouchDown { x, y } => format!("uinput -T -d {} {}", x, y),
        InputCommand::TouchMove { x, y } => format!("uinput -T -m {} {}", x, y),
        InputCommand::TouchUp { x, y } => format!("uinput -T -u {} {}", x, y),
        InputCommand::Click { x, y } => format!("uitest uiInput click {} {}", x, y),
        InputCommand::Wheel {
            x,
            y,
            delta_y,
            sensitivity,
        } => {
            let dist = (380.0 * sensitivity).clamp(160.0, 960.0) as i32;
            let dir = if *delta_y > 0 { -1 } else { 1 };
            let y1 = (*y as i32 - dir * dist / 2).clamp(80, mgr.height as i32 - 80);
            let y2 = (*y as i32 + dir * dist / 2).clamp(80, mgr.height as i32 - 80);
            let vel = (3500.0 * sensitivity) as u32;
            format!("uitest uiInput fling {} {} {} {} {}", x, y1, x, y2, vel)
        }
        InputCommand::Swipe {
            x1,
            y1,
            x2,
            y2,
            velocity,
        } => {
            let vel = (*velocity).clamp(300, 35000);
            format!("uitest uiInput swipe {} {} {} {} {}", x1, y1, x2, y2, vel)
        }
        InputCommand::Key(k) => match k.as_str() {
            "back" => "uitest uiInput keyEvent Back".to_string(),
            "home" => "uitest uiInput keyEvent Home".to_string(),
            "power" => "uitest uiInput keyEvent Power".to_string(),
            "vol_up" | "volume_up" => "uitest uiInput keyEvent 16".to_string(),
            "vol_down" | "volume_down" => "uitest uiInput keyEvent 17".to_string(),
            _ => return,
        },
        InputCommand::Text(text) => {
            format!("uitest uiInput text '{}'", text.replace('\'', "'\\''"))
        }
    };

    let m = mgr.clone();
    tokio::spawn(async move {
        let _ = tokio::task::spawn_blocking(move || m.shell(&shell_cmd)).await;
    });
}
