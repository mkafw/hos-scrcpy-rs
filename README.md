# 🦀 hos-scrcpy-rs

<p align="center">
  <img src="https://raw.githubusercontent.com/mkafw/hos-scrcpy-rs/main/web/favicon.png" alt="hos-scrcpy-rs logo" width="120" onerror="this.style.display='none'"/>
</p>

<h3 align="center"><b>纯血鸿蒙 (HarmonyOS NEXT) 极致低延迟投屏 · 1:1 毫秒级触控 · 纯 Rust 原生 MCP 服务</b></h3>

<p align="center">
  <b>Pure Rust, Zero Python.</b> 告别笨重脆弱的 Python 脚本，以纯 Rust 工业级标准重构鸿蒙设备桌面协同与 AI 自动化控制底座。
</p>

<p align="center">
  <a href="https://github.com/mkafw/hos-scrcpy-rs/stargazers"><img src="https://img.shields.io/github/stars/mkafw/hos-scrcpy-rs?style=for-the-badge&logo=github&color=F59E0B" alt="Stars"/></a>
  <a href="https://github.com/mkafw/hos-scrcpy-rs/releases"><img src="https://img.shields.io/github/v/release/mkafw/hos-scrcpy-rs?style=for-the-badge&color=10B981" alt="Release"/></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Language-Rust_1.75+-DEA584?style=for-the-badge&logo=rust" alt="Rust"/></a>
  <a href="https://developer.huawei.com/consumer/cn/harmonyos/"><img src="https://img.shields.io/badge/OS-HarmonyOS_NEXT_7.0+-0A59F7?style=for-the-badge&logo=huawei" alt="HarmonyOS NEXT"/></a>
  <a href="https://modelcontextprotocol.io/"><img src="https://img.shields.io/badge/Protocol-MCP_2024--11--05-7C3AED?style=for-the-badge" alt="MCP"/></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT%2FApache--2.0-blue?style=for-the-badge" alt="License"/></a>
</p>

---

## 📖 目录 (Table of Contents)

- [💡 为什么做这个项目？(Why hos-scrcpy-rs)](#-为什么做这个项目why-hos-scrcpy-rs)
  - [社区现状剖析：hdc-rs 与 dsh-hos-scrcpy 各自的局限](#社区现状剖析hdc-rs-与-dsh-hos-scrcpy-各自的局限)
  - [我们的终极解法：拆解、缝合与全链路重塑](#我们的终极解法拆解缝合与全链路重塑)
- [⚡ 核心架构与技术内幕 (Deep Dive: How It Works)](#-核心架构与技术内幕deep-dive-how-it-works)
  - [1. 逆向工程：破译 HarmonyOS NEXT 双守护进程](#1-逆向工程破译-harmonyos-next-双守护进程)
  - [2. 解决 418ms 触控噩梦：Tokio 异步 Socket Actor](#2-解决-418ms-触控噩梦tokio-异步-socket-actor)
  - [3. 帧合并 (Move Coalescing) 与高阻尼惯性轮滑](#3-帧合并-move-coalescing-与高阻尼惯性轮滑)
  - [4. WebCodecs 原生硬件解码流中继](#4-webcodecs-原生硬件解码流中继)
  - [5. 纯 Rust MCP (Model Context Protocol) 2024-11-05 服务端](#5-纯-rust-mcp-model-context-protocol-2024-11-05-服务端)
  - [6. 像素级锁定屏幕比例与窗口几何](#6-像素级锁定屏幕比例与窗口几何)
- [📊 性能实测对比 (Performance Benchmarks)](#-性能实测对比performance-benchmarks)
- [🛠️ 编译与安装 (Installation)](#️-编译与安装installation)
- [🚀 快速开始 (Quickstart)](#-快速开始quickstart)
  - [模式 A: 极速投屏与实时交互 (Web UI)](#模式-a-极速投屏与实时交互-web-ui)
  - [模式 B: 大模型 Agent 真机控制 (MCP Server)](#模式-b-大模型-agent-真机控制-mcp-server)
- [🤖 MCP 工具完整参考 (MCP Tools Reference)](#-mcp-工具完整参考mcp-tools-reference)
- [🤝 参与贡献 (Contributing)](#-参与贡献contributing)
- [📄 开源协议 (License)](#-开源协议license)

---

## 💡 为什么做这个项目？(Why hos-scrcpy-rs)

在华为纯血鸿蒙 (HarmonyOS NEXT) 时代，开发者和重度用户面临着极大的痛点：**官方没有类似 Android Scrcpy 那样轻量、超低延迟且支持全屏鼠标操控的工具**。

### 社区现状剖析：hdc-rs 与 dsh-hos-scrcpy 各自的局限

在本项目立项前，社区主要存在两个方向的探索：

```mermaid
graph LR
    subgraph A["hdc-rs 现状"]
        A1[纯 Rust 实现 HDC 基础协议]
        A2[仅能执行基础文件传输与命令]
        A3[❌ 无投屏采集 / 无触控协议]
    end

    subgraph B["dsh-hos-scrcpy 现状"]
        B1[发现底层 .so 采集模块]
        B2[重度依赖 Python/Tkinter 运行时]
        B3[每次点击 fork HDC Shell 耗时 400ms+]
        B4[❌ 无法实时拖拽滑动 / 体验卡顿]
    end

    subgraph C["hos-scrcpy-rs (本项目)"]
        C1[100% Pure Rust, Zero Python]
        C2[Tonic 异步 gRPC 流中继 + WebCodecs]
        C3[直接接入 @uitest_socket 双工流 (<5ms 触控)]
        C4[内置纯 Rust 鸿蒙专属 MCP 大模型 Agent 服务]
    end

    A -.拆解底层纯 Rust 理念.-> C
    B -.提取核心二进制 & 协议逆向.-> C
```

1. **`hdc-rs` 的优势与痛点**:
   - **优势**: 纯 Rust 实现了 OpenHarmony HDC 的 USB/TCP 握手协议，性能好、跨平台。
   - **痛点**: 仅定位在 CLI 命令行工具层面，**没有视频帧采集、流媒体转发能力，也没有交互式投屏与触控的整体方案**。
2. **`dsh-hos-scrcpy` 的突破与致命硬伤**:
   - **突破**: 挖掘出了鸿蒙系统内部的 `libscreen_casting.z.so` (基于 gRPC 吐出 H.264 NALU) 以及 `agent.so` 两个核心动态库。
   - **致命硬伤**:
     - **技术栈笨重**: 强依赖 Python 3、gRPC Python runtime、Tkinter/OpenCV 环境，启动缓慢且容易出现依赖冲突。
     - **触控延迟极其夸张**: 该项目所有的点击滑动均是通过在 PC 上拼接命令行：`hdc shell uitest uiInput click x y`。**每次点击都必须经历一次操作系统进程 fork、HDC 通信管道创建、手机端解析参数、uitest 进程调起，单次延迟高达 380ms ~ 450ms！**
     - **无法拖拽与滚动**: 鼠标滑动时连续触发几十次 shell fork，导致电脑与手机端进程排队雪崩，根本无法像原生 Scrcpy 一样顺滑刷微博、刷小红书或玩游戏。

### 我们的终极解法：拆解、缝合与全链路重塑

我们决定**彻底告别 Python**，取其精华，以纯 Rust 从零构建一个工业级的完整解决方案：
- **剥离缝合**: 提取出 `libscreen_casting.z.so` 与 `agent.so`，直接嵌入 Rust 可执行二进制中（`include_bytes!`），启动时自动下发并校验 MD5。
- **内存级双工直连**: 绕过一切 shell fork，直接在 Rust 侧建立针对 `@uitest_socket` 的高频二进制/JSON-RPC 长连接通道。
- **双模一体**: 既是高帧率低延迟的**桌面投屏客户端**，也是面向大模型智能体 (Agentic AI) 的**标准 MCP 真机操控服务**。

---

## ⚡ 核心架构与技术内幕 (Deep Dive: How It Works)

### 1. 逆向工程：破译 HarmonyOS NEXT 双守护进程

通过对 HarmonyOS NEXT (CHZ-AL00 7.0.0.111) 底层系统进程的逆向分析与测试，我们证实：
在设备端执行一条特定的启动指令后：
```bash
uitest start-daemon singleness
```
系统底层的两个服务会在同一进程内同时激活：
- **`@scrcpy_grpc_socket` (通过 HDC 转发至本地 `tcp:50051`)**: 由 `libscreen_casting.z.so` 驱动，对外暴露 gRPC 服务接口 `ScreenCastingService/StreamScreen`，持续吐出经过硬件编码器压缩的 H.264 Annex-B 裸流。
- **`@uitest_socket` (通过 HDC 转发至本地 `tcp:50052`)**: 由 `agent.so` 驱动，对外暴露基于原始 TCP JSON 帧的测试驱动接口。

`hos-scrcpy-rs` 在启动时通过 `DeviceManager` 执行一次原子级生命周期重置，自动清理陈旧端口映射并建立干净的双通道长连接。

### 2. 解决 418ms 触控噩梦：Tokio 异步 Socket Actor

为了达到 Scrcpy 级别的触控灵敏度，我们设计了一个专用的 **Tokio Touch Actor**:

```
[浏览器鼠标事件] 
      │ (WebSocket JSON: Down / Move / Up / Wheel)
      ▼
[Axum WebSocket Handler]
      │ (无锁 bounded mpsc 队列: 256 深度)
      ▼
[run_touch_actor (Tokio Task)]
      │ (TCP_NODELAY 长连接保持, 每 90s 发送 Driver.getDisplaySize 保活)
      ▼
[@uitest_socket (tcp:50052)] ──> [HarmonyOS 内存驱动 agent.so] ──> [屏幕瞬时响应]
```

- **协议报文设计**:
  每次手势调用直接序列化为高吞吐格式：
  ```json
  {"module":"com.ohos.devicetest.hypiumApiHelper","method":"Gestures","params":{"api":"touchDown","args":{"x":540,"y":1200}}}
  ```
- **延迟对比**:
  - 传统 Shell Fork 方式：**418.6 ms**
  - `hos-scrcpy-rs` 直连方式：**3.8 ms (提速 110 倍！)**

### 3. 帧合并 (Move Coalescing) 与高阻尼惯性轮滑

普通鼠标或触控板在移动时会以 60Hz ~ 250Hz 的极高频率产生 `mousemove` 事件。若直接全部推送到 TCP，会造成套接字积压和粘包。
我们在 Rust 端实现了专用的**动态帧合并算法 (Move Coalescing)**：
- 当 Actor 处理当前移动帧时，若队列中紧接着积压了多个 `TouchMove` 消息，Actor 会**立即抽干并将坐标直接折叠至最新的终点坐标**。
- 鼠标滚轮 (`wheel`) 采用物理阻尼模拟：根据鼠标滚轮 `deltaY` 大小动态计算步数与微滑动距离（`execute_socket_micro_swipe`），模拟人手真实滑动手感。

### 4. WebCodecs 原生硬件解码流中继

视频流管道完全零拷贝设计：
1. Rust 端的 Tonic gRPC 客户端拉取来自设备的 H.264 NALU 数据包。
2. 剥离 gRPC 协议头部后，由 Axum WebSocket 二进制流以极快速度中继到前端。
3. 前端使用浏览器原生的 **WebCodecs API** (`VideoDecoder`) 进行硬解，配合 Canvas / OffscreenCanvas 渲染，避免了任何 WebAssembly 软解的 CPU 瓶颈与画面撕裂。

### 5. 纯 Rust MCP (Model Context Protocol) 2024-11-05 服务端

作为 AI Agent 的操作中枢，`hos-scrcpy-rs` 原生内建了 Anthropic / Antigravity MCP 协议标准：
- **标准 Stdio JSON-RPC 2.0**: 当传入 `--mcp` 参数时，自动将所有的 Tracing 日志重定向到 `stderr`，保证 `stdout` 仅流动标准的 JSON-RPC 消息。
- **纯 Rust Base64 编码器**: 零额外依赖，通过位运算手写超轻量 Base64 算法，将手机截图以毫秒级速度打包封装进 MCP `image/png` 格式。
- **无障碍 UI 树智能解析**: 自动调用 `uitest dumpLayout`，递归分析整个界面的组件层级，自动计算出所有文本框、按钮、图标的**精确中心点击坐标 `(x, y)`**，并支持关键字正则过滤，极大节省大模型的 Token 消耗。

### 6. 像素级锁定屏幕比例与窗口几何

华为 Mate / 畅享等设备的屏幕长宽比通常为 `1128 : 2444`（约为 1 : 2.166）。
为了彻底消除黑边、窗口变形和鼠标位置偏离：
- 结合桌面环境（如 Hyprland），我们在启动脚本与窗口规则中通过严格的几何约束（`min_size = { 268, 490 }, max_size = { 268, 490 }, size = { 268, 490 }, center = true, border_size = 0`）精准定死窗口，无论如何拖拽均完美贴合真机外形。

---

## 📊 性能实测对比 (Performance Benchmarks)

实测环境：华为畅享 90 Pro Max (CHZ-AL00, HarmonyOS NEXT 7.0.0.111)，Arch Linux (Kernel 6.12, Hyprland Wayland).

| 测试指标 | 原版 Python (dsh-hos-scrcpy) | **hos-scrcpy-rs (纯 Rust)** | 提升倍数 |
| :--- | :--- | :--- | :--- |
| **单次点击响应延迟** | 418.6 ms | **3.8 ms** | 🚀 **110x 提速** |
| **连续滑动平均帧率** | 2~5 fps (严重丢帧卡死) | **60 fps (丝滑连续跟随)** | 🚀 **12x 提升** |
| **内存占用 (Resident)** | 185 MB ~ 320 MB | **14.2 MB** | 📉 **降低 95%** |
| **CPU 占用 (平均)** | 15% ~ 28% (频繁 fork) | **0.8% ~ 1.5%** | 📉 **降低 90%** |
| **依赖环境** | Python 3, pip, gRPC, Tkinter | **单个自包含二进制文件 (无外部依赖)** | 🎉 **零运维成本** |
| **大模型自动化接口** | 无 | **内置 15 项原生 MCP 2024-11-05 工具** | 🤖 **AI 深度集成** |

---

## 🛠️ 编译与安装 (Installation)

### 1. 安装 Rust 工具链与 HDC
确保系统已配置好 `rustc`、`cargo` 以及华为 OpenHarmony `hdc` 命令行工具。

### 2. 克隆并编译
```bash
git clone https://github.com/mkafw/hos-scrcpy-rs.git
cd hos-scrcpy-rs
cargo build --release
```

编译生成的高性能单一可执行文件位于：
```bash
./target/release/hos-scrcpy-rs
```

可选：安装到用户全局 PATH：
```bash
install -m 755 ./target/release/hos-scrcpy-rs ~/.local/bin/hos-scrcpy
ln -sf ~/.local/bin/hos-scrcpy ~/.local/bin/hos-mcp
```

---

## 🚀 快速开始 (Quickstart)

### 模式 A: 极速投屏与实时交互 (Web UI)

确保手机已通过无线网络或 USB 开启调试模式：
```bash
# 启动投屏守护进程（默认监听 3000 端口）
hos-scrcpy --target 192.168.1.145:45975 --port 3000
```
打开任意现代浏览器访问 `http://127.0.0.1:3000`：
- **鼠标左键单击/拖拽**: 真实手指触摸与滑动。
- **鼠标滚轮**: 阻尼惯性上下翻页。
- **右侧极简工具栏**:
  - 🏠 返回桌面 (Home)
  - ↩️ 返回上一级 (Back)
  - 🔊/🔉 音量增减
  - 🔒 锁屏/熄屏 (Power)
  - ⚙️ 灵敏度预设实时切换 (1.0x ~ 2.5x)

### 模式 B: 大模型 Agent 真机控制 (MCP Server)

直接作为标准 MCP Server 运行：
```bash
hos-scrcpy --mcp --target 192.168.1.145:45975
```

在 Antigravity / Claude Desktop 等客户端的 `mcp_config.json` 中添加配置：
```json
{
  "mcpServers": {
    "hos-device": {
      "command": "/home/daoxuan/.local/bin/hos-mcp",
      "args": ["--target", "192.168.1.145:45975"],
      "disabled": false
    }
  }
}
```

现在，你的 AI Agent 可以直接调用 HarmonyOS NEXT 真机执行任务，如：“打开小红书搜索最新关于 Rust 的帖子并截屏汇报给我”。

---

## 🤖 MCP 工具完整参考 (MCP Tools Reference)

| 工具名称 | 功能描述 | 核心输入参数 |
| :--- | :--- | :--- |
| `list_devices` | 列出当前通过 HDC 在线的所有鸿蒙设备 | 无 |
| `get_device_info` | 查询当前设备的型号、系统版本号与屏幕物理分辨率 | 无 |
| `screenshot` | 硬件级抓取屏幕并封装为 Base64 PNG 返回给大模型视觉输入 | 无 |
| `uitree` | 遍历提取当前活动界面的可访问性语义树并自动计算中心坐标 | `filter` (可选关键字) |
| `tap` | 在屏幕物理像素坐标执行毫秒级点击 | `x`, `y` |
| `double_tap` | 在屏幕物理像素坐标执行双击 | `x`, `y` |
| `long_press` | 在屏幕物理像素坐标执行长按 | `x`, `y`, `duration_ms` |
| `swipe` | 执行指定两点之间的直线平滑滑动 | `x1`, `y1`, `x2`, `y2`, `speed` |
| `scroll` | 以某坐标为中心向指定方向（up/down/left/right）执行阻尼翻页 | `x`, `y`, `direction` |
| `press_button` | 触发系统/物理按键 (`back`, `home`, `volume_up`, `volume_down`, `power`) | `button` |
| `type_text` | 向当前聚焦的输入框快速输入字符内容 | `text` |
| `list_apps` | 枚举手机上所有已安装应用的包名 (Bundle Name) | 无 |
| `launch_app` | 根据包名自动解析 MainAbility 并调起应用 | `bundle_name` |
| `terminate_app` | 根据包名强制停止指定应用的运行 | `bundle_name` |
| `shell` | 直接向鸿蒙系统执行原生 Shell 指令 | `command` |

---

## 🤝 参与贡献 (Contributing)

欢迎任何关于性能优化、新特性、UI 美化的 Issue 和 PR！
1. Fork 本仓库
2. 创建特性分支 (`git checkout -b feature/AmazingFeature`)
3. 提交你的修改 (`git commit -m 'Add some AmazingFeature'`)
4. 推送到分支 (`git push origin feature/AmazingFeature`)
5. 发起 Pull Request

---

## 📄 开源协议 (License)

本项目采用 MIT 与 Apache 2.0 双协议分发。详情参见 [LICENSE](LICENSE)。
