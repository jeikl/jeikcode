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
  <a href="#1-what-is-jeikcode">What is JeikCode</a> ·
  <a href="#2-multi-agent-functional--architectural-comparison">Mechanism Comparison</a> ·
  <a href="#3-deep-dive-into-core-mechanisms">Core Mechanisms</a> ·
  <a href="#4-installation--quick-start">Quick Start</a> ·
  <a href="#5-keybindings--commands">Commands</a> ·
  <a href="#6-multi-project-knowledge-packs--rules">Knowledge Packs</a>
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

## 1. What is JeikCode?

**JeikCode is a high-performance AI coding agent developed by deeply decomposing, re-architecting, optimizing, and enhancing the foundation of AtomCode.**

During its evolution, JeikCode synthesized the core design strengths of industry-leading open-source and commercial agents while inventing critical proprietary architectures:
- 🛡️ **Absorbed Grok Build's Resilient Tooling & Prompt Strategies**: Integrated rigid prompt precedence adjudication, 5-stage tool error recovery (Repair Chain), structured diagnostic feedback, and repetitive tool invocation circuit breakers (Loop Guard);
- 🌐 **Absorbed OpenCode's Remote Extensible Architecture**: Engineered multi-instance headless remote execution (Serve), interactive WebUI Gateway, and lightweight cross-platform real-time synchronization;
- ⚡ **Proprietary Core Architectural Breakthroughs**:
  - **High Cache Hit Prefix Architecture**: `sacred_floor` memory protection + dynamic `user-wrap.md` tail wrapping, ensuring strict byte-level Append-only prefix immutability to prevent provider KV cache thrashing;
  - **CodeIntel 2.0 Full Topological Graph & Bilingual Semantic Search**: Tree-Sitter fullstack AST parsing + 6-category topology flow + 9 built-in domain bilingual thesauruses + BM25/concept vector hybrid retrieval + `units.v4.bin` (zstd) process-wide shared index;
  - **Autonomous Agent Self-Configuration (Teaches KB + `jeikcode_config_guide`)**: Embedded 8-chapter progressive documentation empowering the agent to self-inspect and guide system configuration;
  - **Fully Customizable Dynamic Prompts with Live Hot-Reloading**: `init.yaml`, `rules.yaml`, and `user-wrap.md` hot-reload dynamically on mtime without server restarts.

---

## 2. Multi-Agent Functional & Architectural Comparison

The following comparison focuses strictly on **agent execution loops, code retrieval, context management, tool resilience, and protocol mechanisms**:

| Mechanism Dimension | **JeikCode (This Project)** | **Claude Code (Anthropic)** | **OpenCode (OpenCode AI)** | **Grok Build (SpaceXAI)** | **Legacy Baseline** |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **1. Agent Loop & Closure** | **L0/L1/L2 Decoupled Scheduling**<br>• Dynamic step budget adaptation<br>• Autonomous Read-Edit-Test-Verify loop<br>• Multi-turn state machine & subagent dispatch | **Monolithic CLI Loop**<br>• Tightly coupled to Claude API<br>• Relies on server-side turn drive<br>• Single task pipeline | **Effect-TS Pipeline Loop**<br>• Effect state machine & Fibers<br>• Multi-agent plugin collaboration<br>• Async event bus dispatch | **PTY Process-Level Flow**<br>• Proprietary agent state machine<br>• Robust cancellation & interrupts<br>• Coordinated with xAI cloud | **Weak 2-Layer Loop**<br>• Relies on Bridge glue code<br>• Rigid step count bounds<br>• Lack of state isolation |
| **2. Multi-Tier Tool Repair & Resilience** | **5-Stage Repair Chain + Circuit Breaker**<br>• Direct → Relaxed JSON → Regex<br>• **Windows path rescue (`D:\...`)**<br>• **Schema type coercion (`"3"`→`3`)**<br>• Field-level diagnostic hints<br>• 3-Attempt Loop Guard circuit breaker | **Basic Error String Return**<br>• Returns raw tool failure messages<br>• Relies solely on LLM guess-retry<br>• No automatic parameter type coercion | **Effect / Zod Schema Validation**<br>• Strict schema validation<br>• Aborts on error with reason<br>• No multi-tier self-healing fallback | **Structured Diagnostics + Loop Guard**<br>• Type correction & diagnostics<br>• Repetitive loop circuit breaker<br>• Mediocre Windows path support | **3-Tier Basic Repair**<br>• Trailing commas/quotes only<br>• Cannot fix schema type mismatch<br>• Windows paths easily fail serde |
| **3. Code Graph & Topological Exploration** | **CodeIntel 2.0 Deep Graph**<br>• **Tree-Sitter fullstack AST**<br>• 6-Category topology flow (flow/chain/sub)<br>• Vue/React/Svelte/Astro SFC support<br>• Full non-truncated directory tree in `repo_map` | **File Filtering (Grep/Glob)**<br>• No local AST topology graph<br>• Heavy token burn on large repos<br>• Context window easily overwhelmed | **Basic LSP + File Search**<br>• Standard ripgrep & LSP tools<br>• No global topological flow graph<br>• Lacks frontend SFC AST graph | **xai-codebase-graph**<br>• Rust code graph & fuzzy search<br>• File system watcher (fsnotify)<br>• Focuses on standard symbols | **Basic Hash Vector**<br>• Single topology dimension<br>• Truncated directory tree<br>• Weak frontend SFC support |
| **4. High Cache Hit Prefix Architecture** | **Strict Append-Only Guarantee**<br>• **`user-wrap.md` dynamic tail wrapping**<br>• **`sacred_floor` memory protection**<br>• Initial Git snapshot (anti-thrashing)<br>• Live mtime hot-reload without cache loss | **Ephemeral Cache Headers**<br>• Relies on Anthropic cache breakpoints<br>• Restricted to Claude series<br>• In-session injections break cache | **Standard Message Streaming**<br>• Relies on native provider caching<br>• Dynamic prompt injections corrupt prefix<br>• Compaction loses key instructions | **Transcript Compaction**<br>• SQLite-based event journal<br>• Proprietary Compaction Transcripts<br>• Lacks dynamic user wrap template | **Basic Static Prefix**<br>• Dynamic reminders break KV cache<br>• Coarse compression rules |
| **5. Fully Customizable Prompts & Live Reload** | **Fully Externalized + Millisecond Reload**<br>• `init.yaml` (identity/security/prefix)<br>• `rules.yaml` (workflow/disciplines)<br>• `user-wrap.md` (prompt wrapping)<br>• **Immediate live effect without restart** | **Built-in + Limited External**<br>• Prompts compiled in npm package<br>• Supports `CLAUDE.md`<br>• Cannot alter core execution rules | **External Config + Env Vars**<br>• Custom system prompt support<br>• Rules require session restart<br>• Lacks granular YAML layers | **Built-in Prompts + Precedence**<br>• Robust prompt hierarchy<br>• Rule precedence overrides<br>• Core prompt edit requires recompile | **Semi-Static Config**<br>• Hardcoded rules in binary<br>• Requires restart on edit |
| **6. Multi-Protocol, Reasoning & Accounts** | **All-Protocol Support + 4-Gear Effort**<br>• **OpenAI Responses (/v1/responses)**<br>• Chat Completions / Anthropic / Ollama<br>• **Dynamic 4-gear reasoning effort**<br>• Decoupled accounts & dynamic `/models` pull | **Anthropic Locked**<br>• Tailored for Claude 3.5/3.7<br>• Native Thinking Budget<br>• Third-party models require proxies | **Broad Multi-Model**<br>• Covers major commercial/local models<br>• Model selection interface<br>• Manual frontend effort tuning | **xAI Grok Locked**<br>• Tailored for Grok 2/3 models<br>• Proprietary reasoning schemas<br>• Inflexible for custom models | **Basic OpenAI Compatible**<br>• Coupled accounts and models<br>• No Responses reasoning replay<br>• Stale bindings on model switch |
| **7. CodeExplore Indexing & Shared Storage** | **`units.v4.bin` Shared Binary Index**<br>• **zstd ultra-fast compression** + Sidecars<br>• **Rayon multi-core scoring (<1ms query)**<br>• Process-wide shared index, instant cold start<br>• Incremental diff sync & jitter guard | **No Persistent Local Index**<br>• On-demand live search per turn<br>• No shared index persistence | **In-Memory / File Cache**<br>• Node/Bun in-memory caching<br>• Project switch requires rebuild | **Rust Process Graph Index**<br>• High-performance index & watcher<br>• Internal incremental graph updates | **Single-Session JSON Index**<br>• High memory overhead<br>• Slow cold start on large repos |
| **8. Bilingual Thesaurus & Semantic Alignment** | **9 Built-in Domains + Project Thesaurus**<br>• AI Agent/Fullstack/Ecommerce/Admin/Med<br>• Project `<project>/.atomcode/thesaurus/`<br>• **Multi-to-multi CN/EN semantic alignment** | **No Thesaurus Mechanism**<br>• Relies solely on LLM translation<br>• Difficult to map domain terms to code | **No Thesaurus Mechanism**<br>• Relies on symbol search<br>• Lacks domain term mapping | **No Thesaurus Mechanism**<br>• Focuses on AST symbol graph<br>• No bilingual domain dictionaries | **Basic Built-in Thesaurus**<br>• Few basic dictionaries<br>• No project-level expansion |
| **9. Multi-Project Knowledge Packs & Precedence** | **4-Layer Knowledge Packs + Strict Precedence**<br>• `AGENTS.md` / `ATOMCODE.md` (Main Spec)<br>• `rules.md` (Rules) · `dbwords.md` (DB)<br>• `glossary.md` (Glossary) · Live hot-reload<br>• **Project rules strictly override System rules** | **Single File (`CLAUDE.md`)**<br>• Single file instruction loading<br>• No multi-dimensional knowledge packs<br>• Precedence left to model discretion | **Multi-File Config Support**<br>• Project rules & instruction injection<br>• Loaded via context concatenation<br>• Lacks rigid precedence guarantee | **AGENTS.md / Rule Hierarchy**<br>• Robust project rule parsing<br>• Structured prompt precedence | **Single File Match**<br>• Only recognizes `.atomcode.md`<br>• No DbWords / Rules / Glossary split |
| **10. Agent Self-Configuration via Teaches KB** | **Embedded 8-Chapter Teaches + Config Tool**<br>• Comprehensive guides from prompts to models<br>• Agent can **self-inspect system configs** via `jeikcode_config_guide`<br>• Build-time auto-sync with host assets | **Static Online Docs**<br>• Manual human documentation<br>• No agent self-inspection tool | **Online Documentation**<br>• Community-maintained Markdown<br>• No built-in config guidance tool | **Internal Documentation**<br>• Rich user manual for CLI/TUI<br>• Tailored for internal workflows | **No Built-in Config Tool**<br>• Relies on external README<br>• Agent cannot query its config |
| **11. Skills & MCP Ecosystem** | **Dynamic Skills + Standard MCP**<br>• Dynamic hot-loading from `~/.atomcode/skills/`<br>• Standard Model Context Protocol integration<br>• Progressive loading to protect context | **Native MCP Integration**<br>• Official & community MCP support<br>• Standard Tool injection | **Plugins & MCP Ecosystem**<br>• Rich plugin architecture & MCP<br>• Community tool extensions | **Built-in Tools + MCP**<br>• Rich built-in tools & extensions<br>• Tailored for internal tools | **Basic Skills**<br>• Basic local skill loading<br>• Weak MCP error isolation |
| **12. Fullstack Language & Semantic Retrieval** | **Fullstack Language AST + Hybrid Retrieval**<br>• Vue SFC/TSX/JSX/Svelte/Astro/SCSS/Rust/Go/Java/C++<br>• **BM25 + Concept Vector hybrid ranking**<br>• Soft anchor de-weighting & cross-language flow | **Standard Text Search**<br>• Regex & file text search<br>• Lacks component-specific parsers | **Standard LSP Multi-Language**<br>• Relies on individual language LSPs<br>• Hybrid retrieval handled on client | **Rust / C++ / Python Focus**<br>• Deep backend language optimization<br>• Generic frontend SFC support | **Basic Multi-Language**<br>• Low recognition for Vue/React SFC<br>• No hybrid ranking algorithms |

---

## 3. Deep Dive into Core Mechanisms

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
       │ • First-Token Liveness Timeout (60s × 3)         │
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

### 1. High Cache Hit Prefix Architecture
- **Append-Only Byte Immutability**: System identity, `MEMORY` entries, `SKILLS`, `MCP` tools, and project knowledge rules (`AGENTS.md`, `rules.md`, `dbwords.md`) are merged in the session head under `sacred_floor` protection. Initial Git state is snapshotted to prevent environment drift from invalidating provider KV caches.
- **`user-wrap.md` Dynamic Tail Wrapping**: Wraps only the final user message via `{{input}}` with project precedence (`./user-wrap.md` > `~/.atomcode/user-wrap.md`). Millisecond mtime hot-reload with zero server restarts.
- **`sacred_floor` Guard**: When `/compact` executes, sacred memory and rules are preserved below the floor line, guaranteeing critical instructions are never lost.
- **Clean UI Restoration**: WebUI and TUIX automatically unwrap messages for display so users see clean chat history while the LLM receives structured instructions.

### 2. CodeIntel 2.0 Full Topological Graph & Bilingual Thesaurus
- **Fullstack Tree-Sitter AST Parsing**: Deeply analyzes Rust, Go, Python, Java, C++, and complete frontend frameworks (Vue2/3 SFC dual `template`+`script` parsing, React TSX/JSX element extraction, Svelte, Astro, and CSS/SCSS/LESS class selectors).
- **6-Category Topological Flow**: Explores anchors, subtree modules, parent dependency chains, siblings, graph connectivity flows, and path tokens, combined with BM25 and bilingual concept vector hybrid ranking.
- **9 Built-in Domain Bilingual Thesauruses**: Covers Computer Science, AI Agents, Fullstack Dev, E-commerce, Admin Systems, Robotics, and Medical terminology to bridge natural language queries to code identifiers.
- **`units.v4.bin` Shared Binary Index**: Process-wide zstd cache + Rayon parallel scoring enables sub-millisecond query latency.

### 3. 5-Stage Resilient Tool Repair Chain (Surpassing Grok)
- **5-Tier Self-Healing Pipeline**:
  1. `Direct Parse`: Standard JSON decoding;
  2. `Relaxed Repair`: Strips markdown fences, fixes trailing commas and unquoted keys;
  3. `edit_file Regex Extraction`: Recovers multiline blocks from file modification calls;
  4. `Schema-Bound String Decoding`: Resolves stringified JSON payload escaping;
  5. `Key-Value Fallback`: Final argument-level extraction safeguard.
- **Windows Path Backslash Rescue**: Unescapes `D:\project\src` backslashes before serde parsing.
- **Schema Type Coercion**: Automatically coerces string numbers (`"3"` → `3`) and booleans (`"true"` → `true`).
- **Loop Guard Circuit Breaker**: Returns field-level diagnostic hints on failure; aborts repetitive tool-call loops after 3 consecutive errors.

### 4. Fully Customizable Prompts with Millisecond Hot-Reload
Externalized under `~/.atomcode/prompts/` with zero-cost mtime caching:
- **`init.yaml`**: System identity, precedence hierarchy, security boundaries, and environment settings;
- **`rules.yaml`**: Workflow reflections, code location disciplines, parallel tool calling standards;
- **`user-wrap.md`**: Custom user prompt interception template;
- **Live Updates**: Any modification takes effect on the next turn immediately without restarting.

### 5. Multi-Project Knowledge Packs & Rigid Precedence
Supports multi-dimensional project knowledge packs that **strictly override System default instructions**:
- **Main Spec**: `AGENTS.md` or `ATOMCODE.md`;
- **Glossary**: `.atomcode/glossary.md` (maps domain terms to code symbol aliases);
- **Rules**: `.atomcode/rules.md` (business workflows, permissions, state transitions);
- **DbWords**: `.atomcode/dbwords.md` (database schemas, table/column semantics, SQL guidelines).

### 6. Agent Self-Configuration via Teaches Knowledge Base
- Built-in 8-chapter progressive guides (`01_prompts_and_context.md` to `08_updates_and_releases.md`).
- Native **`jeikcode_config_guide`** tool: The agent can autonomously inspect configuration specifications to resolve user configuration inquiries.

### 7. Multi-Protocol Support & 4-Gear Reasoning Effort
- **4 Protocols**: OpenAI Responses (`/v1/responses`), Chat Completions, Anthropic, and Ollama.
- **4-Gear Reasoning Intensity**: Switch effort (`low`, `medium`, `high`, `xhigh`, `off`) dynamically via `/effort` or WebUI.
- **Decoupled Accounts & Models**: `[provider_accounts.*]` manages credentials, `[models.*]` configures model parameters; `/modeladd` automatically polls upstream `/models`.

### 8. Independent First-Token Liveness Timeout
- Dedicated `first_token_timeout` (default 60s × 3 retries) handles long reasoning silences (DeepSeek-R1, Grok 3) independently from stream token gaps.

### 9. Remote Headless Serve & WebUI Gateway (Absorbing OpenCode into Native Rust)
- **Local WebUI**: Launch via `/webui` or `jeikcode webui`.
- **Real-Time Token Popup**: Visual breakdown of prompt tokens, reasoning tokens, cache hits, and Sacred Floor retention.
- **Headless Serve**:
  ```bash
  # Launch headless server
  jeikcode serve --host 0.0.0.0 --port 4096 --token sk-my-secret

  # Connect from remote client
  jeikcode attach http://192.168.1.100:4096 --token sk-my-secret
  ```

### 10. Anti-Accidental-Touch & TTY Guard
- **Double-ESC/Ctrl+C Cancel**: Stops active generation and restores the input box without risking accidental single-key drops.
- **TTY Foreground Grabbing**: Reclaims terminal foreground on Linux upon turn completion; ignores `SIGTTIN`/`SIGTTOU`/`SIGTSTP` hang signals.

---

## 4. Installation & Quick Start

### Option 1: GitHub Releases Prebuilt Binary (Recommended)

Download precompiled binaries from [GitHub Releases](https://github.com/jeikl/jeikcode/releases):

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

cargo install --path crates/atomcode-cli --bin jeikcode --locked
jeikcode --version
```

### Configure & Launch

Run inside your workspace:

```bash
cd /path/to/your/project
jeikcode
```

Configuration is stored at `~/.atomcode/config.toml`:

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

Common commands:
```bash
# Start in specific workspace directory
jeikcode -C /path/to/project

# Specify model
jeikcode --model deepseek-reasoner

# Headless mode for scripting / CI (outputs to stdout)
jeikcode -p "Investigate and fix the OAuth callback 404 issue"

# Continue last session
jeikcode -c
```

---

## 5. Keybindings & Commands

### Terminal Keybindings

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

### Slash Commands

| Category | Command | Description |
| :--- | :--- | :--- |
| **Modes & Execution** | `/plan` | Switch to read-only exploration mode |
| | `/build` | Switch to execution & modification mode |
| | `/goal <target>` | Set goal for autonomous loop execution |
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

## 6. Multi-Project Knowledge Packs & Rules

JeikCode enforces strict **Project-Level Precedence**. Knowledge files in your repository dynamically inject rules that **strictly override System instructions**:

```text
your-project/
  ├── AGENTS.md                  # Main project spec (tech stack, style, tests)
  ├── user-wrap.md               # Dynamic user prompt template (with {{input}})
  └── .atomcode/
      ├── rules.md               # Business workflows and permissions
      ├── dbwords.md             # Database schemas and column semantics
      ├── glossary.md            # Domain glossary to code symbol mapping
      └── thesaurus/             # Project-specific domain thesaurus (*.txt)
```

---

## 7. License

This project is licensed under the [MIT License](LICENSE).

<p align="center">
  Crafted with Rust, Tree-Sitter, Ratatui, and Passion for Engineering Excellence.
</p>
