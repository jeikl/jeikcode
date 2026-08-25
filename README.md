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
  <a href="#2-functional--architectural-comparison">Feature Comparison</a> ·
  <a href="#3-native-codeexplore--repomap-deep-retrieval">CodeExplore</a> ·
  <a href="#4-core-architectural-highlights">Highlights</a> ·
  <a href="#5-installation--quick-start">Quick Start</a> ·
  <a href="#6-keybindings--commands">Commands</a> ·
  <a href="#7-multi-project-knowledge-packs">Knowledge Packs</a>
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

- 🛡️ **Absorbed Grok Build's Hardcore Control Strategies**: Integrated rigid prompt precedence hierarchy (Precedence), 5-stage tool error recovery (Repair Chain), structured diagnostic feedback, and repetitive tool invocation circuit breakers (Loop Guard);
- 🌐 **Absorbed OpenCode's Remote Extensible Architecture**: Engineered multi-instance headless remote execution (Serve), interactive WebUI Gateway, and lightweight cross-platform real-time synchronization;
- 🔍 **Inspired by and Surpassing CodeGraph with Native CodeExplore**: Resolving CodeGraph's critical limitation of only supporting hardcoded symbol indexing with zero natural language semantic understanding, JeikCode engineered its own **Weighted AST Vectors + Bilingual Code & Comment Multi-Vector Semantic Search + Weighted Score Ranking Algorithm**, boosting code search efficiency by **60% - 70%** with **90%+ accuracy**;
- ⚡ **Proprietary High Cache Hit Prefix Architecture**: `sacred_floor` memory protection + dynamic `user-wrap.md` tail wrapping, ensuring strict byte-level Append-only prefix immutability to eliminate provider KV cache thrashing;
- 🧠 **Agent Self-Configuration via Teaches Knowledge Base**: Embedded 8-chapter progressive documentation empowering the agent to self-inspect and guide system configuration via `jeikcode_config_guide`. All prompts hot-reload dynamically on mtime without server restarts.

---

## 2. Functional & Architectural Comparison

The following comparison focuses strictly on **agent execution loops, code retrieval, context management, tool resilience, and protocol mechanisms** using clear ✅ and ❌ indicators:

### 1. Core Agentic Feature Matrix

| Core Feature & Mechanism | **JeikCode (This Project)** | **Claude Code** | **OpenCode** | **Grok Build** | **Legacy Baseline** |
| :--- | :---: | :---: | :---: | :---: | :---: |
| **Autonomous Read-Edit-Test-Verify Loop** | ✅ Dynamic Budget + State Machine | ✅ Basic CLI Loop | ✅ Effect-TS Pipeline | ✅ PTY Process Loop | ❌ Weak Glue Layer |
| **5-Stage Tool Repair Chain (with Windows Backslash Rescue)** | ✅ 5-Tier Self-Healing | ❌ Raw Error String | ❌ Strict Error Abort | ✅ Basic Parameter Fix | ❌ Prone to Serde Failure |
| **Schema Type Coercion (`"3"`→`3`, `"true"`→`true`)** | ✅ Auto Type Coercion | ❌ Relies on LLM Guess | ❌ Relies on LLM Retry | ✅ Partial Coercion | ❌ No Type Healing |
| **3-Attempt Repetitive Tool Circuit Breaker (Loop Guard)** | ✅ Rigid Loop Guard | ❌ Prone to Infinite Loops | ❌ Context Truncation | ✅ Loop Guard Guard | ❌ No Loop Breaker |
| **Native CodeExplore Bilingual Semantic Search** | ✅ **Weighted Multi-Vector** | ❌ None (Grep only) | ❌ None (LSP/Grep) | ❌ None (Symbol AST) | ❌ Zero-Hit Misses |
| **repo_map Structure-First Directory Tree (No Truncation)** | ✅ Complete Dir Tree | ❌ Truncated by Token | ❌ Basic Dir Listing | ✅ Dir Tree Viewer | ❌ Coarse Elision |
| **Low-Relevance Code Collapsed into Minimal Token Budget** | ✅ Minimal Token Cost | ❌ Heavy Dump Context | ❌ Basic File Folding | ✅ Output Budgeting | ❌ Hard Truncation |
| **KV Cache Prefix Stability (Append-Only Discipline)** | ✅ `user-wrap` Tail Wrap | ❌ Injections Corrupt Cache | ❌ Dynamic Interruption | ❌ Transcript Truncation | ❌ Reminder Thrashing |
| **`sacred_floor` Memory / Rule Protection on Compression** | ✅ Never Lost | ❌ Sliding Window Loss | ❌ Prone to Loss | ✅ Event Compaction | ❌ Drops Rules |
| **Fully Externalized Prompts with Millisecond Hot-Reload** | ✅ Immediate Effect | ❌ Compiled in Binary | ❌ Session Restart Needed | ❌ Recompile Needed | ❌ Restart Needed |
| **Multi-Project Knowledge Packs Precedence (`rules/dbwords`)** | ✅ Strict Over System | ❌ Single `CLAUDE.md` | ❌ Context Concat | ✅ Rule Precedence | ❌ Single File Only |
| **Agent Self-Configuration Tool (`jeikcode_config_guide`)** | ✅ 8-Chapter Guide Tool | ❌ None (Static Web) | ❌ None (Static Docs) | ❌ None (Internal Docs) | ❌ No Self-Inspection |
| **Multi-Protocol Support (Responses / Completions / Anthropic)** | ✅ 4 Protocols Native | ❌ Anthropic Only | ✅ Multi-Model | ❌ Grok Only | ❌ No Responses |
| **4-Gear Reasoning Effort Switching (`low/med/high/xhigh`)** | ✅ Realtime `/effort` | ❌ Thinking Budget Only | ❌ Manual Frontend | ✅ Bound to Grok | ❌ Stale Bindings |
| **Independent First-Token Liveness Timeout (60s × 3)** | ✅ Solves R1/Grok3 Hangs | ❌ Single Stream Timeout | ❌ Request Timeout | ✅ Process Watchdog | ❌ Stream Hangs |
| **Multi-Instance Headless Remote Serve + WebUI Gateway** | ✅ Native Rust Engine | ❌ Local CLI Only | ✅ Web + Desktop App | ❌ Pager TUI Only | ❌ Basic WebUI |
| **Double-Press ESC/Ctrl+C Cancel + Linux TTY Grabbing** | ✅ Anti-Misoperation | ❌ Single ESC Drops State | ❌ Basic Interrupt | ✅ Terminal PTY Guard | ❌ Linux TTY Lockup |

---

### 2. Programming Language AST & Semantic Retrieval Support

JeikCode's native CodeExplore supports fullstack AST parsing across all major programming languages:

| Language & Framework | **JeikCode (CodeExplore)** | **Claude Code** | **OpenCode** | **Grok Build** |
| :--- | :---: | :---: | :---: | :---: |
| **Java** | ✅ **AST Analysis + Semantic Search** | ❌ (Standard Grep) | ✅ (Relies on LSP) | ✅ (Basic Symbol Graph) |
| **C / C++** | ✅ **AST Analysis + Semantic Search** | ❌ (Standard Grep) | ✅ (Relies on LSP) | ✅ (Native Support) |
| **Python** | ✅ **AST Analysis + Semantic Search** | ❌ (Standard Grep) | ✅ (Relies on LSP) | ✅ (Basic Symbol Graph) |
| **Vue (Vue2/3 SFC Dual Parse)** | ✅ **Template + Script Deep Support** | ❌ (Standard Grep) | ❌ (No SFC Graph) | ❌ (Weak Frontend) |
| **TypeScript / JavaScript** | ✅ **JSX / TSX Element Extraction** | ❌ (Standard Grep) | ✅ (Relies on LSP) | ✅ (Basic Symbol Graph) |
| **Rust** | ✅ **AST Analysis + Semantic Search** | ❌ (Standard Grep) | ✅ (Relies on LSP) | ✅ (Native Support) |
| **Go** | ✅ **AST Analysis + Semantic Search** | ❌ (Standard Grep) | ✅ (Relies on LSP) | ✅ (Basic Symbol Graph) |
| **Svelte / Astro / CSS / SCSS** | ✅ **Component & Style Selectors** | ❌ (Standard Grep) | ❌ (No SFC Graph) | ❌ (Unsupported) |

---

## 3. Native CodeExplore & repo_map Deep Retrieval

### 1. Origin & Evolution: From CodeGraph to CodeExplore

The open-source project **CodeGraph** pioneered symbol indexing, but real-world engineering revealed a fatal flaw: **it only understands hardcoded symbols and has zero natural language semantic comprehension**. When developers ask questions in natural language (e.g. "Find the logic that handles payment refund callbacks"), pure symbol search fails completely.

Inspired by this insight, JeikCode completely re-architected **`CodeExplore`** and **`repo_map`**:

1. **Weighted AST Vectors + Bilingual Multi-Vector Semantic Search**:
   - Extracts AST symbols, call hierarchies, and struct definitions;
   - Parses docstrings and comments in both English and Chinese;
   - Constructs multi-vector semantic embeddings cross-referenced against bilingual domain thesauruses.
2. **Weighted Score Ranking Top-Placing Core Code**:
   - Relevant code snippets and implementation logic are ranked and placed directly at the top of the observation context for the agent.
3. **Minimal Token Budget for Low-Relevance Files**:
   - For secondary files and potential dependencies, CodeExplore avoids dumping bloated context and instead generates concise structural summaries with minimal token budget.
4. **Quantifiable Real-World Performance**:
   - 🚀 **Search Efficiency Boosted by 60% - 70%**: The agent locates target code in a single round without endless grep iterations;
   - 🎯 **Accuracy Maintained at 90%+**: Accurately pinpoints implementations for both mixed-language queries and fuzzy business requests.

> 💡 *CodeExplore currently supports bilingual Chinese and English semantic search, with additional natural languages planned based on community demand!*

---

## 4. Core Architectural Highlights

### 1. High Cache Hit Prefix Architecture
- **Strict Append-Only Discipline**: Prompts, memory entries, skills, and project rules are merged at session initiation under `sacred_floor` protection.
- **Dynamic `user-wrap.md` Tail Wrapping**: Intercepts only the latest user message via `{{input}}` with project precedence (`./user-wrap.md` > `~/.atomcode/user-wrap.md`). Modifying templates hot-reloads in milliseconds without breaking the cached prefix.
- **`sacred_floor` Protection**: On `/compact`, core rules and memories below the floor are never dropped.
- **Clean UI Display**: WebUI and TUIX automatically unwrap messages so users see clean chat history while LLMs receive structured instructions.

### 2. 5-Stage Resilient Tool Repair Chain (Surpassing Grok)
- **5-Tier Self-Healing**: Direct JSON → Relaxed JSON (trailing commas/unquoted keys) → `edit_file` Regex extraction → Schema-bound string decoding → Key-Value fallback.
- **Windows Path Backslash Rescue**: Unescapes `D:\project\src` backslashes before serde parsing.
- **Schema Type Coercion**: Automatically coerces `"quantity":"3"` → `3` and `"retry":"true"` → `true`.
- **3-Attempt Loop Guard Circuit Breaker**: Terminates repetitive tool errors after 3 consecutive failures, forcing the agent to switch approaches.

### 3. Fully Externalized Prompts with Millisecond Hot-Reload
Externalized under `~/.atomcode/prompts/` with zero-cost mtime caching:
- `init.yaml`: Identity, precedence hierarchy, security boundaries;
- `rules.yaml`: Workflow rules, tool calling disciplines, output standards;
- `user-wrap.md`: User prompt wrapping template;
- **Live Updates**: Editing files immediately takes effect on the next turn without restarting.

### 4. Multi-Project Knowledge Packs & Rigid Precedence
Knowledge files in your workspace **strictly override System default instructions**:
- `AGENTS.md` / `ATOMCODE.md` (Main project spec)
- `.atomcode/rules.md` (Business workflows & constraints)
- `.atomcode/dbwords.md` (Database schemas & SQL rules)
- `.atomcode/glossary.md` (Domain terminology to code symbol mapping)

### 5. Agent Self-Configuration via Teaches Knowledge Base
- Embedded 8-chapter progressive documentation (`01_prompts_and_context.md` to `08_updates_and_releases.md`).
- Native **`jeikcode_config_guide`** tool: The agent autonomously inspects configuration specifications to guide users and self-diagnose setup issues.

### 6. Multi-Protocol Support & 4-Gear Reasoning Effort
- Native support for OpenAI Responses (`/v1/responses`), Chat Completions, Anthropic, and Ollama;
- Dynamically toggle reasoning effort (`low` / `medium` / `high` / `xhigh` / `off`) via `/effort` or WebUI;
- Decoupled credentials and model parameters with dynamic upstream `/models` polling in `/modeladd`.

### 7. Independent First-Token Liveness Timeout
- Dedicated `first_token_timeout` (default 60s × 3 retries) prevents silent reasoning hangs in DeepSeek-R1 and Grok 3 models.

### 8. Remote Headless Serve & WebUI Gateway
- **Local WebUI**: Launch via `/webui` or `jeikcode webui` with real-time token breakdown popups;
- **Multi-Instance Remote Serve**:
  ```bash
  jeikcode serve --host 0.0.0.0 --port 4096 --token sk-my-secret
  jeikcode attach http://192.168.1.100:4096 --token sk-my-secret
  ```

### 9. Anti-Accidental-Touch & TTY Control Protection
- **Double-ESC/Ctrl+C Cancel**: Stops active execution and restores input without risking accidental single-key drops;
- **TTY Foreground Grabbing**: Reclaims terminal foreground on Linux upon turn completion, ignoring `SIGTTIN`/`SIGTTOU`/`SIGTSTP` hang signals.

---

## 5. Installation & Quick Start

### 1. GitHub Releases Prebuilt Binary (Recommended)

Download precompiled binaries from [GitHub Releases](https://github.com/jeikl/jeikcode/releases):

```bash
# Linux / macOS
curl -fsSL https://raw.githubusercontent.com/jeikl/jeikcode/local-dev/scripts/install.sh | bash

# Windows PowerShell
irm https://raw.githubusercontent.com/jeikl/jeikcode/local-dev/scripts/install.ps1 | iex
```

### 2. Build from Source

Requires **Rust 1.88+** ([rustup.rs](https://rustup.rs/)):

```bash
git clone https://github.com/jeikl/jeikcode.git
cd jeikcode

cargo install --path crates/atomcode-cli --bin jeikcode --locked
jeikcode --version
```

### 3. Configure & Launch

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

## 6. Keybindings & Commands

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

## 7. Multi-Project Knowledge Packs

Placing the following files in your repository dynamically injects rules that **strictly override System instructions**:

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

## 8. License

This project is licensed under the [MIT License](LICENSE).

<p align="center">
  Crafted with Rust, Tree-Sitter, Ratatui, and Passion for Engineering Excellence.
</p>
