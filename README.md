<div align="center">
<pre>
       _      _ _     ____          _
      | | ___(_) | __/ ___|___   __| | ___
   _  | |/ _ \ | |/ / |   / _ \ / _` |/ _ \
  | |_| |  __/ |   <| |__| (_) | (_| |  __/
   \___/ \___|_|_|\_\\____\___/ \__,_|\___|
</pre>
</div>

<p align="center">
  <strong>Ultra-fast, Autonomous Open-Source Terminal AI Coding Agent Built with Rust</strong>
</p>

<p align="center">
  English · <a href="./README.zh-CN.md">简体中文</a>
</p>

<p align="center">
  <a href="#quick-start">Quick Start</a> ·
  <a href="#-multi-agent-in-depth-comparison">Agent Comparison</a> ·
  <a href="#-architecture--30-day-innovations">Architecture</a> ·
  <a href="#-features">Features</a> ·
  <a href="#-installation">Installation</a> ·
  <a href="#-keybindings--commands">Commands</a> ·
  <a href="#-project-knowledge-packs--rules">Knowledge Packs</a>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/version-6.0.26-blue.svg" alt="version">
  <img src="https://img.shields.io/badge/rust-1.88%2B-orange.svg" alt="rust">
  <img src="https://img.shields.io/badge/license-MIT-green.svg" alt="license">
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows%20%7C%20HarmonyOS-lightgrey.svg" alt="platform">
  <a href="https://github.com/jeikl/jeikcode" target="_blank">
    <img src="https://img.shields.io/github/stars/jeikl/jeikcode?style=social" alt="GitHub Stars"/>
  </a>
</p>

---

**JeikCode** is a next-generation autonomous AI coding agent designed from scratch in **Rust** to live in your terminal. Engineered for extreme speed, sub-millisecond cold starts, and minimal memory footprint (<30MB RAM), JeikCode understands your entire codebase topology, navigates AST semantics, modifies files in batch, runs tests, and autonomously verifies & self-heals errors in a robust loop.

Whether used as your primary terminal pair programmer or deployed as a headless gateway for CI/CD, IDEs, and WebUI, JeikCode provides an unmatched engineering foundation compared to **Claude Code**, **OpenCode**, and **Grok Build**.

---

## 🌟 Multi-Agent In-Depth Comparison

The following matrix compares architectural designs, runtime performance, and core agentic mechanisms based on codebase inspection across major open-source and commercial coding agents:

| Dimension | **JeikCode (This Project)** | **Claude Code (Anthropic)** | **OpenCode (OpenCode AI)** | **Grok Build (SpaceXAI)** | **Legacy Baseline** |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **Language & Runtime** | **Native Rust**<br>• Memory: <30MB RAM<br>• Zero GC jitter, <10ms startup<br>• Single self-contained binary | **TypeScript / Node.js**<br>• Memory: ~200MB+<br>• Heavy V8/Node dependency<br>• Distributed via npm | **TypeScript / Bun / Effect**<br>• Memory: ~150MB<br>• Bun / Effect-TS stack<br>• SQLite session journal | **Native Rust Multi-Crate**<br>• Memory: <50MB<br>• Monolithic 76+ crates<br>• Requires DotSlash/protoc | **Early Rust**<br>• Single-session basic loop<br>• Memory leaks & lack of AST caching |
| **Architecture Layering** | **Strict L0/L1/L2 Decoupling**<br>• L0: Neutral Kernel Loop<br>• L1: Reusable Tools & Graph<br>• L2: Coding State Machine<br>• Drivers: TUI / WebUI / Serve / ACP | **Monolithic CLI Pipeline**<br>• Coupled directly to Claude API<br>• Hardcoded workflow in TS scripts<br>• Single terminal driver | **5-Layer Modular Workspace**<br>• Schema → Protocol → Core → Server → Client<br>• Multi-process Effect pipeline | **Monolithic Pager System**<br>• PTY pipeline & Pager TUI<br>• Tailored for xAI proprietary backend<br>• Internal middleware coupling | **Weak 2-Layer Model**<br>• Legacy Core + Bridge<br>• Driver logic coupled to runtime |
| **KV Cache & Prompt Stability** | **Byte-Level Append-Only Guarantee**<br>• `user-wrap.md` dynamic tail wrapping<br>• `sacred_floor` memory protection<br>• Initial Git snapshot (anti-thrashing)<br>• Live mtime hot-reload (0 cost) | **Ephemeral Cache Headers**<br>• Relies on Anthropic cache headers<br>• Restricted to Claude models<br>• No dynamic local prompt wrapper | **Message Array Streaming**<br>• Standard API message arrays<br>• Mid-session prompt injections corrupt prefix cache<br>• Compression loses context | **Transcript Compaction**<br>• SQLite-based event journal<br>• Proprietary compaction transcripts<br>• Lacks dynamic user wrap | **Static Prefix**<br>• Dynamic reminders break KV cache<br>• Coarse compression rules |
| **Code Intelligence & Graph** | **CodeIntel 2.0 Deep Graph**<br>• Tree-Sitter fullstack AST<br>• 6-Category topology graph<br>• **9-Domain bilingual thesaurus**<br>• BM25 + Concept Vector hybrid ranking<br>• `zstd` binary multi-session cache | **Grep / Glob / View**<br>• No local AST graph index<br>• High token burn on large repos<br>• No domain thesaurus mapping | **File Grep + Basic LSP**<br>• Ripgrep & standard LSP hooks<br>• No global topological spine<br>• Lacks bilingual concept dictionary | **xai-codebase-graph**<br>• Rust graph & fuzzy search<br>• File system watcher (fsnotify)<br>• No domain thesaurus mapping | **Basic Hash Vector**<br>• Slow cold start per session<br>• Prone to zero-hit explore misses<br>• Truncated directory tree |
| **Tool Resilience & Repair** | **5-Stage Repair Chain + Circuit Breaker**<br>• Relaxed JSON & Regex extraction<br>• Windows backslash path rescue<br>• Schema type coercion (`"3"`→`3`)<br>• Structured diagnostic feedback<br>• 3-Attempt Loop Guard | **Basic Error Observation**<br>• Tool failures returned as raw error strings<br>• Prone to repetitive error loops<br>• Relies solely on LLM self-correction | **Effect Schema Validation**<br>• Zod / Effect type validation<br>• Hard failure with error reason<br>• Lacks multi-tier auto-salvaging | **Structured Diagnostics + Loop Guard**<br>• Good parameter correction<br>• Optimized for Grok tool calling<br>• Mediocre Windows path support | **3-Tier Basic Repair**<br>• Trailing comma / quotes only<br>• No schema type self-healing<br>• Windows path syntax errors |
| **First-Token Liveness Timeout** | **Dual-Arm Independent Timers**<br>• **First-Token Timer (60s × 3 retries)**<br>• Seamless handling of DeepSeek-R1 / O1 / Grok 3 silent reasoning latency<br>• 900s compilation bash timeout | **Unified Stream Timeout**<br>• Single request timeout<br>• Silent reasoning models may prematurely abort | **Request Timeout**<br>• Managed by Effect runtime<br>• Does not differentiate first token from token gaps | **PTY Process Watchdog**<br>• Robust process watchdog<br>• Coordinated with xAI cloud | **Single Stream Timeout**<br>• Hangs indefinitely during long reasoning latency |
| **Model & Protocol Freedom** | **Fully Decoupled Multi-Protocol**<br>• **OpenAI Responses (/v1/responses)**<br>• Chat Completions / Anthropic / Ollama<br>• **4-Gear Reasoning Effort (low~xhigh)**<br>• Dynamic upstream `/models` polling<br>• Vision Preprocessor delegation | **Locked to Anthropic**<br>• Optimized for Claude 3.5/3.7<br>• Thinking budget integrated<br>• Third-party models require proxies | **Broad Multi-Model**<br>• OpenAI / Anthropic / Gemini / OpenRouter / Ollama<br>• Visual model selector<br>• Requires manual frontend tuning | **Locked to xAI Grok**<br>• Tailored for Grok 2/3 models<br>• Proprietary reasoning schema<br>• Not suitable for custom offline models | **Basic OpenAI Compatible**<br>• Tight account-model coupling<br>• No Responses reasoning replay<br>• Stale bindings on model switch |
| **Terminal & UI Experience** | **Anti-Accidental-Touch & TTY Guard**<br>• TUI: **Double-press ESC / Ctrl+C Cancel**<br>• Linux TTY foreground grabbing<br>• **WebUI Gateway** (real-time tokens)<br>• **Remote Headless Serve Multi-Instance**<br>• Native ACP server | **Terminal CLI**<br>• Clean & minimal<br>• No Web UI gateway<br>• Single-press interrupts can drop state | **Multi-Platform Matrix**<br>• Terminal TUI<br>• Desktop App (Tauri/Electron)<br>• Web Console + Slack bot<br>• Feature-rich but heavy | **Pager-Style TUI**<br>• Excellent scrollback & diff viewer<br>• Tailored keyboard bindings<br>• No standalone lightweight WebUI | **Basic TUI**<br>• Single ESC/Ctrl+C drops context<br>• Linux TTY hangs / stopped signal issues |
| **Config & Self-Update** | **Teaches KB + Single-Step Update**<br>• Embedded 8-chapter progressive guides<br>• `jeikcode_config_guide` self-inspection<br>• **GitHub Releases Single-Step Re-exec**<br>• Interactive config migration | **npm Global Update**<br>• `npm update -g @anthropic-ai/claude-code`<br>• Static online docs | **Multi-Channel PMs**<br>• Homebrew / Scoop / npm / Nix<br>• Online documentation portal | **Source Sync / DotSlash**<br>• Monorepo sync / script install<br>• Online documentation | **Basic File Replace**<br>• Static updater<br>• No built-in config guide tool |
| **Open Source & Privacy** | **100% Open Source (MIT)**<br>• Unrestricted commercial & self-hosted use<br>• 100% Local privacy control | **Partially Closed Source**<br>• Distributed as obfuscated JS<br>• Core orchestration runs on server | **100% Open Source (MIT)**<br>• Active community & plugins | **Official Sync Open Source**<br>• Dependent on xAI cloud infrastructure | **Open Source (MIT)** |

---

## 🚀 Architecture & 30-Day Innovations (v6.0.0 ~ v6.0.26)

```text
┌────────────────────────────────────────────────────────────────────────┐
│               JeikCode Unified Runtime Pipeline (CodingRuntime)        │
└────────────────────────────────────────────────────────────────────────┘
     CLI / TUIX  │  WebUI Gateway  │  Remote Serve  │  Daemon  │  ACP
                 └───────────────┬──────────────────┘
                                 │
                                 ▼
                   JeikCode CodingRuntime (L2)
       ┌──────────────────────────────────────────────────┐
       │ • Session Lifecycle & State Machine              │
       │ • Dynamic Prompt Templates & user-wrap.md Reload │
       │ • Sacred Floor Context Protection (Memory/Rules) │
       │ • First-Token Liveness Timeout & Auto-Retry      │
       │ • Reasoning Effort Control & Responses Protocol  │
       │ • Subagent Dispatcher                            │
       └─────────────────────────┬────────────────────────┘
                                 │
                                 ▼
                   atomcode-capabilities (L1)
       ┌──────────────────────────────────────────────────┐
       │ • CodeIntel 2.0 (AST / 6-Topology / 9-Thesaurus) │
       │ • 5-Stage Resilient Repair Chain & Loop Guard    │
       │ • jeikcode_config_guide Autonomous Tool          │
       │ • Cross-Platform I/O (Windows \\?\ & UTF-8 BOM)  │
       └─────────────────────────┬────────────────────────┘
                                 │
                                 ▼
                   atomcode-kernel (L0 Neutral Core)
       ┌──────────────────────────────────────────────────┐
       │ • Neutral Agent Execution Loop                   │
       │ • Streaming Token Sink & Observation Return      │
       │ • Strictly Unidirectional & Provider-Agnostic    │
       └──────────────────────────────────────────────────┘
```

### 1. KV Cache Prefix Stability & Dynamic User Wrap (`user-wrap.md`)
- **Append-Only Immutability**: Rules, system instructions, and memory are merged at session creation under `sacred_floor` protection, preventing KV cache invalidation across turns.
- **`user-wrap.md` Dynamic Interception**: Wrap only the latest real user prompt via `{{input}}` with project-level precedence (`./user-wrap.md` > `~/.atomcode/user-wrap.md`). Millisecond mtime hot-reload with zero server restart.
- **Clean UI Display**: WebUI and TUIX automatically unwrap the raw user prompt for display, keeping chat history clean while feeding protected structured instructions to the model.

### 2. CodeIntel 2.0 Code Graph & Bilingual Semantic Search
- **Full Frontend & Backend AST Parsing**: Tree-sitter powered parsing across Rust, Go, Python, Java, C++, TypeScript, TSX/JSX, Vue2/3 SFC, Svelte, Astro, and CSS/SCSS/LESS.
- **6-Category Topological Graph**: Explores anchor, subtree, parent chain, siblings, connected graph flow, and path tokens to eliminate blind grepping.
- **9-Domain Bilingual Thesaurus**: Multi-to-multi semantic alignment for Computer Science, AI Agents, Fullstack Dev, E-commerce, Admin Systems, Robotics, and Medical domains.
- **Multi-Session Shared Cache**: Process-wide `units.v4.bin` (zstd compression) + Rayon parallel scoring enables sub-millisecond query latency.

### 3. 5-Stage Resilient Tool Repair Chain (Surpassing Grok)
- **5-Stage Salvaging**: Direct JSON → Relaxed Repair (trailing commas, unquoted keys, markdown code fences) → `edit_file` Regex extraction → Schema-bound stringified decoding → Key-Value fallback.
- **Windows Path Rescue**: Unescapes single backslash paths (`D:\test`) before serde deserialization.
- **Schema Type Coercion**: Automatically coerces string numbers (`"3"` → `3`) and booleans (`"true"` → `true`).
- **Loop Guard & Circuit Breaker**: Returns field-level diagnostic hints on failure; aborts repetitive tool-call loops after 3 consecutive errors.

### 4. Independent First-Token Liveness Timeout
- Solves silent reasoning hangs in ultra-large reasoning models (DeepSeek-R1, Grok 3, o1). Employs an independent `first_token_timeout` (default 60s × 3 retries) separate from stream gap timeouts.

### 5. Multi-Protocol Support & Reasoning Effort Control
- **4 Protocol Adapters**: OpenAI Responses (`/v1/responses`), Chat Completions, Anthropic, and Ollama.
- **4-Gear Reasoning Effort**: Switch reasoning intensity (`low`, `medium`, `high`, `xhigh`, `off`) dynamically via `/effort` or WebUI.
- **Dynamic `/models` Polling**: Automatically queries and autocompletes upstream model lists in `/modeladd`.

### 6. Progressive Teaches Knowledge Base & Config Guide Tool
- Built-in 8-chapter progressive documentation (`01_prompts_and_context.md` - `08_updates_and_releases.md`).
- Native `jeikcode_config_guide` tool allows the agent to self-inspect and guide users on system configurations.

---

## 🛠️ Features

### 1. Terminal TUIX Experience
- **Double-ESC/Ctrl+C Anti-Misoperation**: Prevents accidental turn cancellation while providing instant input recovery.
- **TTY Foreground Grabbing**: Actively reclaims terminal foreground on Linux upon turn completion; ignores `SIGTTIN`/`SIGTTOU`/`SIGTSTP` hang signals.
- **Multiline Input & Themes**: Supports `\` + `Enter` across all terminals, Kitty keyboard protocol, and `base16-ocean.dark` markdown syntax highlighting.
- **Clipboard Image Support**: Paste screenshots directly via `Alt+V` / `Ctrl+Alt+V` / `/paste`.

### 2. WebUI Gateway & Remote Serve
- **Local WebUI**: Launch the interactive browser gateway via `/webui` or `jeikcode webui`.
- **Real-Time Token Popup**: Detailed breakdown of prompt tokens, reasoning tokens, cache read/write tokens, and Sacred Floor retention.
- **Headless Remote Serve**:
  ```bash
  # Launch multi-instance server on host
  jeikcode serve --host 0.0.0.0 --port 4096 --token sk-my-secret

  # Connect from remote client
  jeikcode attach http://192.168.1.100:4096 --token sk-my-secret
  ```

### 3. Autonomy & Modes
- **Plan Mode (`/plan`)**: Read-only codebase exploration and architecture design.
- **Build Mode (`/build`)**: Full autonomous execution and code modification.
- **Goal Mode (`/goal <target>`)**: Autonomous multi-turn task completion loop until the goal condition is satisfied.
- **Detached Background Sessions (`/bg`)**: Offload long-running tasks while maintaining interactive TUI usage.

---

## 📦 Installation

### Option 1: GitHub Releases Prebuilt Binary (Recommended)

Download precompiled binaries directly from [GitHub Releases](https://github.com/jeikl/jeikcode/releases):

```bash
# Linux / macOS
curl -fsSL https://raw.githubusercontent.com/jeikl/jeikcode/local-dev/scripts/install.sh | bash

# Windows PowerShell
irm https://raw.githubusercontent.com/jeikl/jeikcode/local-dev/scripts/install.ps1 | iex
```

### Option 2: Build from Source

Requires **Rust 1.88+** ([rustup.rs](https://rustup.rs/)):

```bash
git clone https://github.com/jeikl/jeikcode.git
cd jeikcode

# Build and install binary
cargo install --path crates/atomcode-cli --bin jeikcode --locked

# Verify installation
jeikcode --version
```

---

## 🏁 Quick Start

### 1. Launch & Configure

Run inside any project directory:

```bash
cd /path/to/your/project
jeikcode
```

On first launch, follow the interactive setup wizard. Configuration is stored at `~/.atomcode/config.toml`:

```toml
default_provider = "deepseek"

[provider_accounts.deepseek]
api_key  = "sk-xxxxxxxxxxxxxxxxxxxxxxxx"
base_url = "https://api.deepseek.com/v1"

[models.deepseek-chat]
provider = "deepseek"
model    = "deepseek-chat"
protocol = "chat_completions"

[models.deepseek-reasoner]
provider         = "deepseek"
model            = "deepseek-reasoner"
protocol         = "chat_completions"
reasoning_effort = "high"
```

### 2. Common CLI Commands

```bash
# Start in specific workspace directory
jeikcode -C /path/to/project

# Specify model
jeikcode --model deepseek-reasoner

# Headless mode for scripting / CI (outputs to stdout)
jeikcode -p "Investigate and fix the OAuth callback 404 issue"

# Load prompt from file
jeikcode --prompt-file task.md

# Continue last session
jeikcode -c
```

---

## ⌨️ Keybindings & Commands

### 1. Keybindings

| Key | Description |
| :--- | :--- |
| `Enter` | Send message |
| `\` + `Enter` | Universal multiline newline |
| `Shift+Enter` / `Alt+Enter` | Multiline newline (supported terminals) |
| `Esc` ×2 / `Ctrl+C` ×2 | **Double-press Cancel**: Stop active generation & return to input |
| `Alt+V` / `Ctrl+Alt+V` | Paste image from clipboard |
| `Ctrl+Up` / `Ctrl+Down` | Scroll chat history |
| `PageUp` / `PageDown` | Scroll page up/down |
| `Ctrl+L` | Clear screen (preserves context) |

### 2. Slash Commands

| Category | Command | Description |
| :--- | :--- | :--- |
| **Modes & Execution** | `/plan` | Switch to read-only exploration mode |
| | `/build` | Switch to execution & modification mode |
| | `/goal <text>` | Set goal for autonomous loop execution |
| | `/review` | Run comprehensive code review on Git diff |
| | `/effort` | Toggle reasoning effort (low/med/high/xhigh/off) |
| **Sessions & Background** | `/resume` | Interactively resume / switch sessions |
| | `/bg` | Manage background tasks (`/bg list`) |
| | `/clear` | Reset context and start new session |
| | `/compact` | Trigger manual context compression |
| **Models & Gateway** | `/model` | Switch current active model |
| | `/provider` | Manage Provider credentials & accounts |
| | `/webui` | Launch local WebUI Gateway |
| | `/diff` | View active unstaged Git diffs |
| | `/undo` | Revert file edits from the previous turn |
| **Knowledge & Guides** | `/guide <query>` | Query Teaches knowledge base for configuration guides |
| | `/reload` | Hot-reload `config.toml`, `init.yaml`, and `rules.yaml` |

---

## 📚 Project Knowledge Packs & Rules

JeikCode enforces strict **Project-Level Precedence**. Placing the following files in your repository dynamically injects rules that **strictly override default System instructions**:

| Knowledge Pack | Candidate Paths | Purpose |
| :--- | :--- | :--- |
| **Main Spec** | `AGENTS.md` or `ATOMCODE.md` | Architecture constraints, code style, and test commands |
| **Glossary** | `.atomcode/glossary.md` | Domain terminology mapped to code symbol aliases |
| **Rules** | `.atomcode/rules.md` | Business workflows, permissions, and state transitions |
| **DbWords** | `.atomcode/dbwords.md` | Database schemas, table/column semantics, SQL guidelines |
| **User Wrap** | `user-wrap.md` | Dynamic user prompt template with `{{input}}` interpolation |

---

## 🛡️ Security & Permissions

1. **Destructive Command Confirmation**: Commands like `rm -rf`, `git push --force`, and `DROP TABLE` strictly require explicit user approval.
2. **Workspace Isolation**: Access to paths outside the active workspace triggers layered permission prompts.
3. **Source Deletion Guard**: Direct deletion of codebase source files is never auto-approved.
4. **Instant File Rollback**: In-memory snapshots enable one-click `/undo` rollback of recent file edits.

---

## 🤝 Contributing

Contributions are welcome!

```bash
git clone https://github.com/jeikl/jeikcode.git
cd jeikcode

cargo fmt --all
cargo clippy --all
cargo test --workspace
```

- **New Tools**: Implement the `Tool` trait in `crates/atomcode-capabilities/src/tools/`
- **Code Graph Enhancements**: Expand AST and semantic rules in `crates/atomcode-capabilities/src/codeintel/`
- **Configuration Documentation**: Update guides in `crates/atomcode-capabilities/assets/teaches/`

---

## 📄 License

This project is licensed under the [MIT License](LICENSE).

<p align="center">
  Crafted with Rust, Tree-Sitter, Ratatui, and Passion for Engineering Excellence.
</p>
