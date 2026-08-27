# JeikCode 多会话并发、界面同步、会话接管与 Agent 效能改造报告

> 日期：2026-08-27  
> 状态：设计提案，尚未实施  
> 范围：`atomcode-coding`、`atomcode-capabilities`、`atomcode-daemon`、`atomcode-tuix`、`webui` 以及提示词/工具策略

## 1. 执行摘要

JeikCode 已经分别具备多项实现目标所需的局部能力：

- TUI 的 `BgRuntimeManager` 可以让多个 `CodingRuntime` 同时存活，并缓存后台运行时的展示事件；
- daemon 的 `/chat` 路径支持不同 Session 并发，并为每个已准入的 `/chat` turn 提供 fan-out 与 replay；
- WebUI 已能展示多个 Session 的运行状态，并能通过 `/chat/watch` 观察由 API 发起的后台 turn；
- `CodingRuntime`、Session 持久化和 `SessionLease` 已具备清晰的单 Session 独占边界。

当前问题不是“系统没有并发能力”，而是这些能力分散在不同 Driver/UI 路径中，没有一个由 L2 `atomcode-coding` 持有的统一多会话运行时注册表。`/webui` 的 live sync 仍使用单绑定 `LiveViewHub`，导致以下概念被错误耦合：

1. 当前界面正在展示哪个 Session；
2. TUI 与 WebUI 是否同步选中同一 Session；
3. 哪个 `CodingRuntime` 正在运行；
4. 哪个 Session 持有租约；
5. WebUI 应订阅哪条事件流。

本报告建议新增 L2 `SessionRuntimeRegistry`，把运行时、租约、状态、快照、事件日志和待处理交互请求按 Session 定址管理；TUI 和 WebUI 只持有各自的 `ViewBinding`。在此基础上，`sync=1` 被重新定义为“两个界面是否同步选中项”，不再表示任务绑定或任务同步。

任何直接删除 WebUI `activeTurn` 判断、直接放行 TUI `/resume`、或者仅移除 `/bg` 的 `live_binding` 限制的改动，都必须推迟到统一路由与隔离机制完成以后。否则可能产生跨 Session 串流、旧 turn 丢失、错误释放租约或交互请求投递到错误 Session。

---

## 2. 目标与非目标

### 2.1 目标

1. Session A 执行期间，TUI/WebUI 可以切换到 Session B，A 在后台继续执行。
2. 切回 A 时立即渲染最新快照，并继续流式显示尚未结束的增量事件。
3. 不同 Session 可以并发；同一 Session 内继续保持 single-flight，避免两个 turn 同时修改同一上下文。
4. `sync=1` 只同步 TUI/WebUI 的当前选中 Session，不暂停、不迁移、不重配后台任务。
5. WebUI 侧栏的 running spinner 来自真实的 per-session runtime state。
6. approval、`request_user_input`、provider transition 和取消操作都按 Session 精确路由。
7. 同一进程已经持有目标 Session 时直接 attach，避免重复申请租约造成 self-contention。
8. 外部进程占用 Session 时提供可诊断、可恢复且不会形成 split-brain 的接管流程。
9. 优化模型读取与探索策略时以 telemetry/A-B 数据为依据，而不是用未经验证的 Token 百分比指导默认行为。

### 2.2 非目标

1. 不允许同一 Session 同时运行两个主 Agent turn。
2. 不通过删除 `.lease` 文件绕过操作系统排他锁。
3. 不在 daemon、TUI 或 WebUI 中新建第二套 Agent 生命周期。
4. 不把 `BgRuntimeManager` 原样复制进 daemon。
5. 不在第一阶段引入跨机器分布式调度。
6. 不承诺后台任务在整个 JeikCode 进程退出后继续运行；跨进程持久执行属于独立能力。

---

## 3. 已确认的现状

### 3.1 WebUI 的运行中切换被显式阻断

主要位置：

- [`webui/src/components/Chat.tsx`](../../webui/src/components/Chat.tsx)：调用 `liveSessionSwitchDisposition`，运行中切换失败后恢复旧 Session；
- [`webui/src/lib/chatTerminal.ts`](../../webui/src/lib/chatTerminal.ts)：定义 live detach/switch disposition；
- [`webui/src/api.ts`](../../webui/src/api.ts)：`postLiveSwitchSession`、`watchChatSession`、`streamLive` 等客户端接口；
- [`crates/atomcode-daemon/src/live_api.rs`](../../crates/atomcode-daemon/src/live_api.rs)：`live_switch_session_endpoint` 把 `HubError::ActiveTurn` 返回给前端。

这一限制当前是安全保护，不应单独删除。

### 3.2 `LiveViewHub` 是单运行时绑定

主要位置：[`crates/atomcode-daemon/src/live_hub.rs`](../../crates/atomcode-daemon/src/live_hub.rs)。

当前 `HubState` 只有一组：

```text
binding
snapshot
replay
pending_requests
turn_active
last_runtime_sequence
pending_web_steers
```

`bind_with_provider` 在旧 binding 正处于 active turn 时拒绝重新绑定。这个行为解释了为什么当前 `/live/switch_session` 必须阻止切换，也证明“直接删前端判断”无法构成完整修复。

### 3.3 TUI 已有多运行时雏形

主要位置：

- [`crates/atomcode-tuix/src/event_loop/bg_runtime.rs`](../../crates/atomcode-tuix/src/event_loop/bg_runtime.rs)：`BgRuntimeManager`、`BackgroundSlot`、事件缓冲、pending request 恢复；
- [`crates/atomcode-tuix/src/event_loop/commands.rs`](../../crates/atomcode-tuix/src/event_loop/commands.rs)：`/bg`、恢复 Slot、live binding guard；
- [`crates/atomcode-tuix/src/event_loop/mod.rs`](../../crates/atomcode-tuix/src/event_loop/mod.rs)：运行时事件分发、`RuntimeSpawnOverride`、`SessionResumePrepared`、streaming slash 白名单；
- [`crates/atomcode-tuix/src/modals/session_picker.rs`](../../crates/atomcode-tuix/src/modals/session_picker.rs)：磁盘 Session picker 与预获取租约。

`BgRuntimeManager` 已证明单进程内可以同时维护多个 Runtime，但其状态所有权位于 TUI Driver 层，WebUI/daemon 无法把它当作统一事实源。

### 3.4 `/chat/watch` 只覆盖已准入的 `/chat` operation

主要位置：[`crates/atomcode-daemon/src/lib.rs`](../../crates/atomcode-daemon/src/lib.rs)。

`ActiveChatRegistry` 的对象是 `ActiveChatOperation`，注释明确写明它表示一次 admitted `/chat` operation。它维护 per-operation broadcast bus 和 replay，适合复用其算法，但当前不会自动接收 TUI foreground、TUI background 或 native live runtime 的事件。

因此不能把 `watchChatSession(session_id)` 直接当成 TUI/live 多会话改造的完成方案。若目标 Session 仅在 TUI `BgRuntimeManager` 中运行，`/chat/watch` 可能只进入 standby，等不到对应 `/chat` admit。

### 3.5 Session 租约是 OS 排他锁

主要位置：[`crates/atomcode-capabilities/src/session/manager.rs`](../../crates/atomcode-capabilities/src/session/manager.rs)。

`SessionLease` 通过 `Arc<SessionLeaseInner>` 克隆，最后一个实例 Drop 时 unlock；`acquire_lease` 使用 `fs2::FileExt::try_lock_exclusive`，并将 `WouldBlock`/Windows error 33 映射为 `SessionInUse`。

由此得到三条约束：

1. 同进程已有 runtime 持有 Session 时应复用其 lease/runtime，不应再次打开文件抢锁；
2. 进程真正退出后，OS 应释放锁，残留 `.lease` 路径本身不代表仍被占用；
3. sidecar/PID 元数据只能辅助判断，最终所有权必须以成功获得 OS 锁为准。

历史提交 `32b880058` 实现的是 busy continue fork，不是强制夺权。

### 3.6 提示词角色布局总体合理

主要位置：

- [`crates/atomcode-capabilities/src/session/context.rs`](../../crates/atomcode-capabilities/src/session/context.rs)：环境、项目规则和 Git 初始事实作为前部 `Role::System`；
- [`crates/atomcode-coding/src/parts.rs`](../../crates/atomcode-coding/src/parts.rs)：memory、skills 等作为受 `sacred_floor` 保护的 synthetic user；
- [`crates/atomcode-coding/src/persona.rs`](../../crates/atomcode-coding/src/persona.rs)：稳定 persona 与执行规则；
- [`crates/atomcode-coding/assets/prompts/rules.yaml`](../../crates/atomcode-coding/assets/prompts/rules.yaml)：工具探索和执行纪律。

推荐继续采用混合结构：稳定权威规则放 System；动态记忆、技能、任务状态和压缩恢复信息放受保护 synthetic user。`sacred_floor` 解决“是否保留”，System 角色解决“权威层级”，两者不可互相替代。

### 3.7 ReadFile 的硬限制不是“小步爬行”的主要根因

主要位置：[`crates/atomcode-capabilities/src/tools/read.rs`](../../crates/atomcode-capabilities/src/tools/read.rs)。

当前默认读取 1000 行、输出预算约 80 KiB，工具说明已经明确反对 20–70 行微小窗口。对比：

- GrokBuild：最多 1000 行、约 25k tokens，首行和每 10 行输出行号锚点；
- OpenCode：默认 2000 行、约 50 KiB；
- JeikCode：默认 1000 行、约 80 KiB，每行精确行号。

小步读取更可能来自 schema 暴露 `offset/limit`、提示词中反复出现 `hot spans`、强制分轮探索、模型自身的保守工具习惯以及 provider 的 parallel tool call 能力差异。

---

## 4. 必须保持的架构不变量

本改造必须遵循项目 `AGENTS.md`：

```text
CLI / TUI / daemon / background / ACP / clix
                    │
                    ▼
       CodingRuntimeHandle / DriverCommand
                    │
                    ▼
          atomcode-coding (CodingRuntime)
                    │
                    ▼
          atomcode-kernel (Neutral Agent)
```

具体约束：

1. 多会话运行时生命周期的唯一所有者必须位于 `atomcode-coding` L2。
2. `atomcode-kernel` 不增加 Session、provider、WebUI 或文件锁知识。
3. `atomcode-capabilities` 只提供中立租约、持久化和可复用事件/工具能力。
4. daemon/TUI/WebUI 只管理连接、展示和输入路由。
5. 不允许 live hub、active chats 和 TUI background 各自成为互不一致的运行时事实源。

---

## 5. 目标状态模型

### 5.1 SessionKey

仅用 UUID 不足以表达物理存储位置，建议使用：

```rust
pub struct SessionKey {
    pub project_bucket: String,
    pub session_id: String,
}
```

所有注册、查询、事件路由和租约诊断均使用完整 `SessionKey`。工作目录属于 Session 元数据，但不能代替规范化 project bucket。

### 5.2 RuntimeEntry

建议在 `atomcode-coding` 新增类似结构：

```rust
pub struct RuntimeEntry {
    pub runtime_id: RuntimeId,
    pub session: SessionKey,
    pub handle: CodingRuntimeHandle,
    pub lease: SessionLease,
    pub state: RuntimeActivity,
    pub generation: u64,
    pub snapshot: Arc<SessionSnapshot>,
    pub journal: RuntimeEventJournal,
    pub pending_request: Option<RuntimeRequest>,
    pub provider: RuntimeProviderIdentity,
    pub working_dir: PathBuf,
}
```

实际实现时应避免对 UI 类型产生依赖。`RuntimeActivity` 使用中立状态，例如：

```text
Starting
Ready
Running
WaitingApproval
WaitingUserInput
Reconfiguring
Stopping
Stopped
Failed
```

TUI 的 `Done/Cancelled/Error` 展示状态由 Driver 投影生成。

### 5.3 SessionRuntimeRegistry

建议新增文件：

- `crates/atomcode-coding/src/session_runtime_registry.rs`
- 可选：`crates/atomcode-coding/src/runtime_journal.rs`
- 修改：`crates/atomcode-coding/src/lib.rs`

核心 API 草案：

```rust
register_prepared(...)
open_or_attach(SessionKey, PreparedSession, SpawnOptions)
lookup(&SessionKey)
list_activity(project_bucket)
submit(&SessionKey, UserInput)
cancel(&SessionKey)
resolve_request(&SessionKey, RequestId, Value)
subscribe(&SessionKey, after_sequence)
snapshot(&SessionKey)
release_if_idle(&SessionKey)
shutdown_all()
```

必须保证：

- Registry 中一个 `SessionKey` 最多对应一个 live runtime；
- 对同一 Session 的 open/register 是原子的；
- Session 已注册时返回 attach outcome，而不是再次 acquire lease；
- 注册失败不会遗留 lease、事件转发 task 或半初始化 entry；
- Runtime terminal 不等于立即删除 entry，应保留有限时间供界面读取 terminal snapshot/replay。

### 5.4 ViewBinding

ViewBinding 属于 Driver/UI，不拥有 Runtime：

```rust
pub struct ViewBinding {
    pub client_id: ViewClientId,
    pub selected: Option<SessionKey>,
    pub cursor: Option<u64>,
}
```

预期语义：

| 场景 | sync=0 | sync=1 |
|---|---|---|
| WebUI 切到 B | 只改变 WebUI ViewBinding | WebUI 与 TUI 都选中 B |
| TUI `/resume B` | 只改变 TUI ViewBinding | TUI 与 WebUI 都选中 B |
| A 正在运行 | 保持运行 | 保持运行 |
| B 正在运行 | attach + replay | attach + replay，并同步另一界面选中项 |

`sync` 绝不修改 RuntimeEntry 的 state，也不拥有或释放 SessionLease。

### 5.5 RuntimeEventJournal

每个 Session 的事件必须具有稳定定址信息：

```rust
pub struct SequencedSessionEvent {
    pub session: SessionKey,
    pub runtime_id: RuntimeId,
    pub generation: u64,
    pub sequence: u64,
    pub payload: RuntimeObservation,
}
```

事件日志要求：

1. sequence 在一个 runtime generation 内严格递增；
2. snapshot 与 subscribe 必须原子衔接，不能存在既不在 snapshot/replay、也收不到 broadcast 的窗口；
3. text/reasoning delta 可相邻合并以限制内存；
4. approval/user-input request 必须完整保留，直到被明确 resolve；
5. terminal event 必须保留到 entry 被回收；
6. 迟到的旧 generation 事件必须丢弃，防止 Session 重开后串入旧流；
7. lagged subscriber 应通过 snapshot + 新 cursor 恢复，而不是静默跳过缺口。

---

## 6. Daemon 与协议改造

### 6.1 重构 `LiveViewHub`

涉及文件：

- `crates/atomcode-daemon/src/live_hub.rs`
- `crates/atomcode-daemon/src/native_live.rs`
- `crates/atomcode-daemon/src/live_api.rs`

目标：`LiveViewHub` 不再保存唯一 runtime 的完整状态，而只负责：

- WebUI live client 的连接/订阅；
- 当前 WebUI ViewBinding；
- 可选的 TUI/WebUI selection sync 广播；
- 将输入路由到 L2 Registry 中明确的 SessionKey；
- 将 L2 `SequencedSessionEvent` 投影为 `LiveObservation`。

应删除或迁移的单例状态：

```text
snapshot
replay
pending_requests
turn_active
last_runtime_sequence
pending_web_steers
```

这些状态应下沉到 per-session RuntimeEntry/EventJournal，或成为 per-client、per-session 投影视图。

### 6.2 统一 `ActiveChatRegistry` 的角色

涉及文件：`crates/atomcode-daemon/src/lib.rs`。

不建议让 `ActiveChatRegistry` 继续作为另一套 runtime registry。可选迁移方式：

1. 保留 `/chat` operation 的 request-id/cancellation alias 管理；
2. 把 replay、pending interactive 和 active session 查询改为读取 L2 Registry；
3. `/chat` admit 时调用 Registry 的 `open_or_attach/submit`；
4. `/chat/watch` 改为订阅统一 SessionEventJournal；
5. API request 生命周期结束只完成 operation，不自动销毁仍被其他 View 观察的 RuntimeEntry。

### 6.3 新 API 草案

可以先增加新接口并保留旧接口兼容：

```text
GET  /runtime/sessions
GET  /runtime/sessions/:project/:id/snapshot
GET  /runtime/sessions/:project/:id/events?after=<sequence>
POST /runtime/sessions/:project/:id/messages
POST /runtime/sessions/:project/:id/stop
POST /runtime/sessions/:project/:id/requests/:request_id/resolve
PUT  /live/view
GET  /live/view
PUT  /live/sync
```

若 project bucket 不适合直接出现在路径中，可以放在 JSON/query 中，但服务端必须完整校验，不能只按裸 session UUID 查找。

旧接口迁移：

- `/live/switch_session` → 内部改为 `set_view`，不调用 runtime resume/reconfigure；
- `/live/stop` → 取消 ViewBinding 指向的 Session；
- `/live/provider` → 明确携带 SessionKey，并只修改目标 runtime；
- `/chat/watch` → 适配统一订阅；
- `/chat/active`、`/chat/pending` → 读取统一 runtime activity/pending state。

---

## 7. TUI 改造

### 7.1 `BgRuntimeManager` 的迁移方式

涉及文件：

- `crates/atomcode-tuix/src/event_loop/bg_runtime.rs`
- `crates/atomcode-tuix/src/event_loop/mod.rs`
- `crates/atomcode-tuix/src/event_loop/commands.rs`
- `crates/atomcode-tuix/src/modals/session_picker.rs`

推荐渐进迁移，而不是一次删除：

1. 第一阶段让 `BgRuntimeManager` 的 slot 仅保存 View/投影信息和 `SessionKey`；
2. runtime handle、lease、真实 activity、pending request、journal 改由 L2 Registry 持有；
3. `buffered_events` 最终由统一 EventJournal/cursor 取代；
4. `/bg N` 与 `/resume` 最终统一为 `select_session(SessionKey)`；
5. 保留 `/bg` 作为快捷命令：把当前选中 Session 留在 Registry 后台，然后创建/选择一个新 Session。

### 7.2 `/resume` 新流程

```text
打开 picker
  → 选择 SessionKey
  → Registry lookup
      ├─ 已注册：返回 Attached(existing runtime)
      └─ 未注册：prepare + lease + spawn + atomic register
  → 更新 TUI ViewBinding
  → snapshot + cursor replay
  → 恢复该 Session 的 pending approval/user input
```

`SessionResumePrepared` 当前携带了预处理结果和 lease。改造时必须把这份 lease 原样交给 Registry/runtime，不能先 Drop 再重新申请，否则会制造竞态窗口。

当前 `RuntimeSpawnOverride` 只接收 `Config + working_dir + Session`。预计需要改为接收一个拥有明确 lease 所有权的 prepared runtime input，例如：

```rust
pub struct PreparedRuntimeSpawn {
    pub session: Session,
    pub working_dir: PathBuf,
    pub prepared_resume: Option<PreparedCatalogSessionResume>,
}
```

具体类型应放在不会让 L2 依赖 daemon 的位置；必要时将通用 prepared session 类型从 daemon compatibility 层下沉到 capabilities/coding。

### 7.3 Streaming slash 放行时机

只有在以下条件全部满足后，才能把 `/resume`、`/session` 放入 `streaming_executable_slash`：

- 当前 runtime 不会因切换 ViewBinding 被 stop/reconfigure/drop；
- 旧 Session 后续事件有 per-session 路由；
- 新 Session 能原子 attach 或 spawn；
- live sync 会同步 selected Session，而不是重新绑定唯一 runtime；
- pending provider transition/approval 不会投递到错误界面。

之后才能移除 `ensure_bg_foreground_switch_allowed` 中的 live binding 拦截，并更新相应单元测试。

---

## 8. WebUI 改造

### 8.1 前端状态从单流改为 per-session

涉及文件：

- `webui/src/components/Chat.tsx`
- `webui/src/api.ts`
- `webui/src/lib/chatTerminal.ts`
- `webui/src/lib/chatTerminal.test.ts`
- 可能涉及 Session sidebar/activity 状态组件

建议将以下状态按 SessionKey 保存：

```ts
type SessionStreamState = {
  runtimeId?: string;
  generation?: number;
  cursor?: number;
  running: boolean;
  phase: RuntimeActivity;
  messages: SessionMessage[];
  pendingPermission?: PermissionRequest;
  pendingUserInput?: UserInputRequestEvent;
  controller?: AbortController;
  lastError?: string;
};
```

不再用一个全局 `running/busy` 决定能否点击侧栏。

### 8.2 切换算法

```text
用户选择 B
  → 立即更新 WebUI ViewBinding
  → sync=1 时广播 selection changed 给 TUI
  → 请求 B snapshot + cursor
  → 订阅 B after=cursor
  → A 的订阅可继续保留，或由服务端 journal 缓存后在切回时重连
  → 任何事件写入前校验 session/runtime/generation
```

为控制浏览器资源，可以只保持当前 Session 的 SSE，后台状态通过轻量 activity stream 更新；切回时依靠 replay 补齐。若并发 Session 数量较少，也可以维持多个 SSE，但必须设置连接上限和清理策略。

### 8.3 何时删除回弹逻辑

只有当新 snapshot/events API 和事件身份校验测试通过后，才删除或改写：

- `liveSessionSwitchDisposition(running)`；
- `onSessionId(prevId)` 回弹；
- `postLiveSwitchSession` 对 active turn 的拒绝分支。

新的 busy 语义只约束“向同一 Session 提交第二个主 turn”，不约束查看或切换 Session。

---

## 9. Session 占用、恢复与安全接管

### 9.1 同进程 attach 优先

申请磁盘 lease 前，必须先查询 L2 Registry：

```text
Registry 存在 SessionKey
  ├─ runtime 可用 → attach
  ├─ runtime 正在停止 → 等待有界时间后重试
  └─ runtime 已失败但 lease 尚未释放 → 完成清理后再 acquire
```

这会解决最重要的“Session 已在本进程后台运行，但 `/resume` 又重新抢自己的锁”问题。

### 9.2 Owner sidecar

涉及文件建议：

- `crates/atomcode-capabilities/src/session/manager.rs`
- 可新增 `crates/atomcode-capabilities/src/session/lease_owner.rs`
- daemon/TUI 增加交互展示与控制请求

建议记录：

```text
schema_version
session_id
project_bucket
lease_token
runtime_id
pid
process_start_identity
hostname
started_at
last_heartbeat_at（可选）
control_endpoint（可选）
```

sidecar 写入必须原子、限制大小、拒绝符号链接，并避免把敏感凭据写入文件。

### 9.3 占用分类

```text
try_lock_exclusive 成功
  → 当前进程成为 owner，写 sidecar

try_lock_exclusive 失败
  → 读取 sidecar，仅用于诊断
      ├─ owner 是本进程且 Registry 命中 → attach
      ├─ owner 可通过 IPC 联系 → 请求 graceful release
      ├─ owner 活跃但不释放 → fork / cancel / 显式强制终止
      ├─ owner 看似已死 → 有界重试 OS lock
      └─ owner 身份不可信 → fail closed / fork
```

禁止行为：

- 仅因 PID 不存在就删除 lease 文件并宣告获得所有权；
- 未校验进程启动时间就终止 PID；
- 在 OS lock 尚未成功时启动写入同一 Session 的 runtime；
- 通过改名/重建锁文件绕过仍持有旧 inode/handle 的进程。

### 9.4 强制接管的产品语义

建议默认选项顺序：

1. Attach：同一 JeikCode daemon/runtime 可联系时首选；
2. Fork：外部 owner 活跃或身份不明时的安全默认值；
3. Request release：通过 IPC 请求旧 runtime 保存并退出；
4. Force terminate：用户明确确认后才执行，并在终止后等待 OS lock；
5. Cancel：不做任何修改。

“强制接管”不能等同于“忽略锁”。

---

## 10. 提示词与上下文角色建议

### 10.1 保持混合角色结构

System 适合：

- 核心身份和安全边界；
- 工具语义与不可破坏架构约束；
- AGENTS/ATOMCODE/rules 等项目规范；
- 稳定环境事实和初始 Git 快照；
- 项目规则优先级声明。

受保护 synthetic user 适合：

- memory；
- skills catalog；
- todo/plan；
- compaction summary；
- 动态恢复提醒；
- 运行时产生的 bounded continuation。

Real user 只承载用户真实问题及 `user-wrap.md` 包装。

### 10.2 缓存准确表述

目标应是：

> 当配置文件未发生变化时，稳定前缀保持字节一致；当用户明确修改热重载规则时，允许 System context 更新并接受一次缓存失效。

不能同时要求“AGENTS 热重载立即生效”和“System 永远字节不可变”。二者只能通过“无修改时稳定、修改时有意识失效”协调。

### 10.3 Provider adapter 验证

应针对 OpenAI Responses、Anthropic Messages 及第三种兼容协议分别验证：

- 多个内部 System message 如何在 wire 层合并；
- synthetic user 是否保留正确顺序和标记；
- compaction 后 sacred floor 是否仍包含项目规则和 memory；
- provider/model 切换后是否出现重复 System、顺序变化或缓存前缀漂移。

可能涉及：

- `crates/atomcode-coding/src/parts.rs`
- `crates/atomcode-coding/src/controllers.rs`
- 各 provider adapter 的消息序列化代码
- `crates/atomcode-coding/src/telemetry.rs`

---

## 11. ReadFile 与 Agent 探索效率改造

### 11.1 先测量，再改默认值

建议增加以下 telemetry：

```text
read_file_calls_per_turn
read_file_requested_lines
read_file_returned_lines
small_read_ratio_20_70
read_batch_width
tool_round_trips
repo_map_calls
code_explore_calls
model_ttft_ms
model_generation_ms
tool_execution_ms
turn_wall_time_ms
```

按 provider/model/reasoning effort 聚合，避免把模型推理延迟误判为 ReadFile 延迟。

### 11.2 Prompt 调整

涉及文件：

- `crates/atomcode-coding/assets/prompts/rules.yaml`
- `crates/atomcode-coding/src/persona.rs`
- 必要时 `crates/atomcode-capabilities/assets/teaches/05_tools_and_timeouts.md`

建议：

1. 删除重复的“hot spans”约束，保留一句明确规则；
2. 已知目标文件时允许跳过 `repo_map`；
3. 精确字符串查询允许直接使用一次 `rg`；
4. 跨模块陌生链路才要求 `repo_map/code_explore`；
5. 同一响应中推测性并行读取 2–6 个高概率相关文件；
6. 禁止连续 20–70 行窗口爬行，但允许读取由编译错误/精确符号定位出的窄区间；
7. 不强制“第一轮只能 repo_map”，避免人为增加模型往返。

### 11.3 Schema 与输出格式实验

涉及文件：`crates/atomcode-capabilities/src/tools/read.rs`。

候选方案按风险从低到高排列：

1. 仅修改 `offset/limit` 描述，强调省略即默认大页；
2. 针对易小步模型提供只突出 `file_path` 的简化 schema；
3. 新增 `read_file_range`，普通 `read_file` 不突出范围参数；
4. 增加 `line_number_mode = exact | decade`；
5. 经 A/B 验证后，才考虑按模型默认启用十行锚点。

十行锚点可以减少行号字符，但不能预设“节省 40% Token”。验收应同时测量：

- 实际输入 token；
- 模型错误引用行号的比例；
- 为重新定位而增加的工具调用；
- 修改成功率和测试通过率。

### 11.4 三执行档位

可在 telemetry 稳定后引入：

| 档位 | 探索策略 | 验证策略 |
|---|---|---|
| Fast | 已知目标直接查读；小范围改动 | 单项测试或最小 check |
| Balanced | 必要时图谱；批量读主要调用方 | 相关 crate 测试/check |
| Thorough | 跨层调用图、配置与文档审计 | workspace check、回归矩阵、diff review |

如果新增用户配置项，必须同步修改 `crates/atomcode-capabilities/assets/teaches/` 对应文档，遵守项目同变同更约束。

---

## 12. 分阶段实施计划

### 阶段 0：建立基线与回归护栏

目标：在改变行为前固定现状，并获得可比较数据。

修改位置：

- `crates/atomcode-coding/src/telemetry.rs`
- daemon runtime/API 指标汇总位置
- TUI/WebUI 现有切换与 live hub 测试
- ReadFile 单元测试

工作项：

1. 增加 per-session runtime/activity 诊断快照，但暂不改变行为；
2. 增加 event identity、late event、replay gap 测试夹具；
3. 记录工具轮次和读取规模基线；
4. 固定 WebUI active-turn 回弹、TUI live guard、lease contention 的现状测试；
5. 建立两个 Session 并发的集成测试骨架。

退出条件：现有行为有稳定测试覆盖，指标能区分模型耗时和工具耗时。

### 阶段 1：实现 L2 Registry 与 EventJournal

目标：建立唯一多会话运行时事实源，但 UI 暂时仍保持旧限制。

主要改动：

- 新增 `crates/atomcode-coding/src/session_runtime_registry.rs`
- 新增或抽取 `crates/atomcode-coding/src/runtime_journal.rs`
- 修改 `crates/atomcode-coding/src/lib.rs`
- 复用 `CodingRuntimeHandle`、`SequencedRuntimeEvent` 与 Runtime 状态接口

测试：

- 同 Session 并发 register 只有一个成功；
- 不同 Session 可同时 Running；
- attach 不重复申请 lease；
- snapshot+subscribe 无事件缝隙；
- lagged subscriber 可从 snapshot/cursor 恢复；
- 旧 generation 事件被拒绝；
- pending request 按 Session 隔离；
- shutdown/drop 最终释放 lease。

退出条件：Registry 可以在无 UI 的测试中独立管理至少两个并发 Runtime。

### 阶段 2：统一 daemon 事件与 API 路由

目标：让 `/chat`、native live、TUI bridge 使用同一 Session 事件源。

主要改动：

- `crates/atomcode-daemon/src/lib.rs`
- `crates/atomcode-daemon/src/live_hub.rs`
- `crates/atomcode-daemon/src/native_live.rs`
- `crates/atomcode-daemon/src/live_api.rs`
- daemon 路由注册与协议类型

工作项：

1. `ActiveChatRegistry` 只保留 operation/request alias，事件 replay 转交 Registry；
2. `LiveViewHub` 退化为 ViewBinding/connection hub；
3. 新增 Session 定址 snapshot/events/activity API；
4. 旧 `/chat/watch` 适配统一 EventJournal；
5. `/live/switch_session` 改成纯 selection 操作；
6. 输入、停止、approval、user input 全部显式携带 SessionKey。

退出条件：API 创建的 A 与 native live 创建的 B 可以同时运行，观察者不会串流。

### 阶段 3：迁移 TUI 多会话选择

目标：运行中安全使用 `/resume`、`/session`、`/bg`。

主要改动：

- `crates/atomcode-tuix/src/event_loop/bg_runtime.rs`
- `crates/atomcode-tuix/src/event_loop/mod.rs`
- `crates/atomcode-tuix/src/event_loop/commands.rs`
- `crates/atomcode-tuix/src/modals/session_picker.rs`
- `RuntimeSpawnOverride` 与 CLI 注入点

工作项：

1. TUI foreground/background 只保存 Registry 引用与 View cursor；
2. `/resume` 优先 attach Registry 中已有 Session；
3. 新 Session 使用 prepared lease 原子注册；
4. 切换后 snapshot+cursor replay；
5. 恢复 per-session approval/user input；
6. 放行 streaming `/resume` 和 `/session`；
7. 最后删除 live binding 对 `/bg` 的阻断。

退出条件：A 输出期间切到 B、向 B 发消息、再切回 A，A 连续完成且内容无重复、无缺失、无错误终止。

### 阶段 4：迁移 WebUI per-session 状态

目标：WebUI 可完整观察和控制多个并发 Session。

主要改动：

- `webui/src/components/Chat.tsx`
- `webui/src/api.ts`
- `webui/src/lib/chatTerminal.ts`
- 对应前端测试

工作项：

1. 建立 per-session stream state map；
2. sidebar activity 使用统一 runtime activity；
3. selection 与 stream subscription 解耦；
4. 所有事件校验 SessionKey/runtime/generation；
5. sync=1 只同步 TUI/WebUI selection；
6. 新链路稳定后删除 active-turn 回弹；
7. 刷新页面后依靠 snapshot+cursor 恢复。

退出条件：至少三个 Session 并发时可任意切换；刷新、断线重连、切回均不丢失流和交互请求。

### 阶段 5：租约 owner 与安全接管

目标：解决同进程误判占用，并为外部占用提供安全恢复。

主要改动：

- `crates/atomcode-capabilities/src/session/manager.rs`
- 新的 owner sidecar 模块
- daemon/TUI/WebUI 接管请求与交互 UI
- CLI busy continue/fork 路径

工作项：

1. Registry attach 优先于 acquire lease；
2. sidecar owner identity；
3. bounded retry；
4. daemon/runtime graceful release IPC；
5. fork 默认路径；
6. 显式 force terminate 与 PID reuse 防护；
7. 终止后必须重新成功获取 OS lock。

退出条件：同进程切回不报占用；外部活进程不能被静默覆盖；崩溃恢复不会形成双 writer。

### 阶段 6：Prompt/ReadFile 效能实验

目标：减少工具轮次，同时不降低修改正确率。

主要改动：

- `crates/atomcode-coding/assets/prompts/rules.yaml`
- `crates/atomcode-coding/src/persona.rs`
- `crates/atomcode-capabilities/src/tools/read.rs`
- telemetry 与 teaches 文档

工作项：

1. 先精简探索规则和取消强制分轮；
2. 再实验简化 schema；
3. 最后实验十行锚点；
4. 按模型记录 A/B 数据；
5. 数据支持后再提供 Fast/Balanced/Thorough 配置。

退出条件：平均工具轮次下降，任务成功率、行号准确率和测试完成率不下降。

---

## 13. 核心验收场景

至少覆盖以下集成测试：

1. TUI Session A 运行中 `/resume B`，A 后台自然完成。
2. 切回 A 时先看到当前完整状态，再继续收到增量，不重复 token。
3. sync=1 时 WebUI 选择 B，TUI 跟随选中 B，但 A 不停止。
4. sync=0 时 WebUI 和 TUI 可以分别查看 A/B。
5. A、B 同时 Running，侧栏均显示 spinner。
6. A 等待 approval，切到 B 后 B 可继续运行；切回 A 能继续处理原 request id。
7. WebUI 刷新后重新订阅 running Session，不缺失已生成文本。
8. SSE 断线并携带 cursor 重连，无重复、无缺口。
9. 旧 runtime generation 的迟到事件不会污染新 runtime。
10. 同一 Session 第二个主 turn 被 single-flight 拒绝或按既有 steer 语义处理。
11. `/resume` 选中本进程后台 Session 时不重新申请 lease。
12. 外部进程持锁时默认 fork/拒绝，不出现双 writer。
13. owner sidecar 陈旧但 OS lock 可得时正常恢复。
14. owner sidecar 声称 PID 已死但 OS lock 仍不可得时 fail closed。
15. daemon/TUI 退出后所有 Runtime 正确 shutdown，租约最终释放。
16. OpenAI、Anthropic 和第三种兼容协议发起的不同 Session turn 可并发并被 WebUI 观察。

---

## 14. 风险与回滚策略

### 14.1 主要风险

- Runtime 事件在迁移期间同时进入旧 replay 和新 journal，造成重复；
- Session 切换时 snapshot/cursor 非原子，造成缺口；
- pending approval 路由错误，批准了另一个 Session 的工具调用；
- Runtime terminal 与 Registry 清理竞态导致 lease 过早释放；
- provider/session reconfigure 仍假设只有一个 runtime；
- WebUI stale closure 把旧 Session 事件写入当前 state；
- 强制接管按 PID 误杀重用后的无关进程；
- 十行锚点降低通用模型的行号定位准确率。

### 14.2 回滚原则

1. 在阶段 1–2 保留旧 live API，通过内部 feature gate 选择新旧实现；
2. 新协议先只读观察，再开放 submit/stop/approval；
3. TUI 和 WebUI 分开迁移，任何一端异常可退回旧 ViewBinding；
4. 在新事件链路稳定前保留旧的 active-turn guard；
5. 强制接管初期只实现 owner 诊断、attach 和 fork，不急于提供 terminate；
6. ReadFile 输出格式必须可配置回 exact line number。

---

## 15. 验证命令建议

按阶段优先执行聚焦测试，再扩大范围：

```text
cargo test -p atomcode-coding session_runtime_registry
cargo test -p atomcode-daemon live_hub
cargo test -p atomcode-daemon active_chat
cargo test -p atomcode-tuix bg_runtime
cargo test -p atomcode-tuix streaming_slash
cargo test -p atomcode-capabilities session::manager
cargo test -p atomcode-capabilities tools::read
cargo check --workspace
```

WebUI 使用项目已有测试命令运行 `chatTerminal`、Session 切换、SSE reconnect 与 activity 状态测试。最终执行：

```text
git diff --check
```

Windows 环境应额外覆盖 error 33、进程退出释放 lease、同进程 attach 和 PID identity 测试。测试必须通过真实可执行的进程夹具验证，不应只 mock `SessionInUse`。

---

## 16. 推荐决策

建议批准以下总体方向：

1. 以 L2 `SessionRuntimeRegistry` 为首要工程，而不是先删除 UI 限制；
2. 抽取并复用 `ActiveChatRegistry` 的 fan-out/replay 思路，但不把它误认为已经覆盖 TUI/live runtime；
3. 将 TUI `BgRuntimeManager` 逐步降级为 View/投影层；
4. 把 `sync=1` 正式定义为 selection synchronization；
5. 同进程优先 attach，跨进程接管始终以 OS lock 成功为最终依据；
6. 保持 AGENTS/项目规则位于 System，动态上下文位于 protected synthetic user；
7. ReadFile 十行锚点作为可测实验，不作为“零风险、固定节省 40%”的直接默认改动；
8. 用 telemetry 驱动模型专属策略和三执行档位。

推荐从阶段 0 和阶段 1 开始。它们不会提前破坏现有 live sync 行为，却能为后续 TUI/WebUI 无限制切换提供正确且可测试的底座。
