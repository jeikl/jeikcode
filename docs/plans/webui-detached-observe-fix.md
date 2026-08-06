# 修复方案：WebUI detached 观察 API turn 的流式输出 + yolo 下子代理立即 [done]

## 设计边界（确认用户意图，daemon 策略层已正确，不动）

| 入口 | 行为 |
|------|------|
| API 发起的 turn | Auto 少确认；WebUI 是观察者，不弹审批/用户输入模态 |
| WebUI/TUI 自己发起的 turn | 原生交互（审批、用户输入模态照常） |
| `--yolo` | 所有入口都走 yolo（Auto + 无模态） |

`crates/atomcode-daemon/src/lib.rs` 的 `ChatTurnPolicy::resolve` 已按此实现；本修复不动 daemon 策略层。

---

## P1：WebUI 观察 API turn 时流式不显示 + 自发消息被覆盖丢失

### 根因（三层，全在 `webui/src/components/Chat.tsx`）

1. **broadcast 晚订阅者拿不到订阅前事件**：`tokio::broadcast` 不重放历史。WebUI 切入晚于 API turn 首批 ChatEvent send；之后若 turn 处于长工具阶段静默，watch 长时间无事件。`lib.rs:399` broadcast 语义。
2. **2s tick 兜底用磁盘快照整体覆盖流式增量**（`Chat.tsx:670-674`）：守卫只比消息条数 `loaded.length > messagesRef.current.length`，无法识别"同一 assistant message 内正在累加的流式增量"。流式期间条数不增长但磁盘快照可能 ≥ 画布 → 触发 `setMessages(loaded)` 整体替换，把流式累加成果抹掉。
3. **`ensureAssistantBubbleForWatch` 在整体替换后补空 bubble 割裂**（`Chat.tsx:583-591`）：`setMessages(loaded)` 后调用它，若 loaded 末条非 assistant 会插新空 assistant，后续 text_delta 落进新空 bubble，旧 assistant 留上方 → "看上去没流式"。
4. **持久化滞后让 tick 兜不住**：native runtime 多在 turn 边界才落盘，期间 `getSession` 取到的 messages 不增长 → loaded.length > messagesRef 不成立 → tick 不更新画布 → 转圈无输出；turn 结束 tick 判定 inactive 才最终 hydrate（对应"再刷新提示又消失"）。
5. **"发的消息消失"**：detached 状态 `recoveryPolicy.allowSend=false`，输入框 disabled (`Chat.tsx:2815`)；用户在 busy 解除窗口发的消息被乐观显示后，被后续 tick/watch 磁盘历史整体覆盖抹掉。

daemon 侧 `/chat/watch` fan-out 经验证是通的（`lib.rs:4666-4684` subscribe 成功即挂上 bus），无需改 daemon。

### 修复点（全在 `Chat.tsx`）

**修复 1 — tick 在 watch 流活跃时不再整体覆盖，改为 append-only 合并**（`Chat.tsx:666-676`）

- watch 仍连着（`detachedWatchAbortRef.current != null`）：跳过 `setMessages(loaded)` 整体替换，只判定 `stillActive` 终结。
- watch 断开（poll-only fallback）：保留整体 `setMessages(loaded)` 兜底。
- 仍 active 且 watch 活跃但需要补订阅前事件：改为 **append-only 合并** — 取 `loaded` 尾部比 `messagesRef` 多出的整条 message **追加**；绝不替换末条正在流式的 assistant；仅当 `messagesRef` 末条非 assistant 或 parts 为空时，才允许用 loaded 末条 assistant 覆盖那条空 bubble。

**修复 2 — `ensureAssistantBubbleForWatch` 收紧**（`Chat.tsx:583-591`）

末条已是 assistant 直接 return（已是）；但在 `setMessages(loaded)` 之后调用时，改为只在 loaded 末条非 assistant 且确实有正在进行的活动（busy + watch 活跃）时才补空 bubble，避免割裂。

**修复 3 — detached 状态发消息被拦截的 UX 显式化**（`Chat.tsx:2081` `deliver()`）

`allowSend=false` 时，现仅 `pushCommandNotice(t('chat.recoveryBlocked'))`。补充：显式提示"此会话正被 API 客户端占用，你只能观看"，并确保不误导 busy 转圈（busy 由 detached poll 持有）。输入框 disabled 保留。

### P1 预期效果

- API turn 运行中 WebUI 切入：流式 text/tool/reasoning 正常累加，不被每 2s 抹掉。
- 晚订阅丢失的订阅前事件：由 tick append-only 补帧（仅追加新条，不替流式增量）。
- turn 结束：tick 仍做最终 hydrate（保留现有 `stillActive=false` 分支）。
- detached 期间误发被显式拦截提示，不再"光标转圈没反应"。

---

## P1.5：晚订阅（Live）观察者拿不到用户消息 —— daemon 侧 replay（新增）

### 根因

`tokio::broadcast` 不重放历史。WebUI 在 API turn **已 admit 之后**才连上 `/chat/watch`
（如：侧栏看到会话闪动后点进来、刷新页面、切到该会话）时走 `WatchOutcome::Live`，
订阅点在 `ChatEvent::User` 广播之后 → 永远收不到用户消息；而用户消息在 native runtime
**turn 边界才落盘**，2s tick 的 append-only 补帧也拿不到 → 只能看到流式回复，看不到
「自己通过 API 发的消息」。

实测（daemon 4096，standby vs Live）：
- standby 路径：`runtime_info → user → reasoning…`，用户消息正常到达。
- Live 路径（turn 已运行中连接）：只有 `reasoning…`，无 `user`。

### 修复（daemon `lib.rs`）

1. `ActiveChatOperation` 增加 `admitted_user: Option<String>`；`process_chat_request`
   在 `conv.push` 后、broadcast send **之前**调用 `active_chats.record_user_message(...)`
   （先记录后广播，避免「记录与广播之间订阅」漏掉；WebUI 侧对同文本去重，双送无害）。
2. `chat_watch` 的 `WatchOutcome::Live` 分支：在启动 fan-out 转发任务**之前**，
   通过 `operation_for_session` + `admitted_user_message` 取回本 turn 已记录的用户消息，
   以 `ChatEvent::User` 先行注入该 SSE 流 → 晚订阅者第一帧就是用户消息，随后才是实时事件。

### 修复（WebUI `Chat.tsx`）

观察者 `handleEvent` 的 `user` 分支：追加用户消息前，若末条是**空 assistant 占位**
（`ensureAssistantBubbleForWatch` 在更早的 `runtime_info` 上补的），先摘掉它，
保证渲染顺序是「用户消息 → 回复」而非空气泡悬顶。

---

## P1.6：第二轮起失联 —— 回合结束后没有重新待机（新增）

### 根因（全在 `Chat.tsx`）

第一轮能同步、第二轮起不再同步。`startIdleWatch` 的终端分支在收到 `done` 后：

1. `stopDetachedHistoryPoll()` 把当前 watch 连接 abort 掉、清掉 2s tick；
2. `setBusy(false)` 后**同步**调用 `startIdleWatch(...)` —— 但 `busyRef.current` 是
   渲染期同步镜像（`busyRef.current = busy`），此刻仍是上一轮的 `true`，
   `startIdleWatch` 入口守卫 `if (syncRef.current || busyRef.current) return;`
   直接返回 → **待机 watch 没有重挂**。第二轮 API turn admit 时没有任何 watcher → 失联。
3. `startDetachedHistoryPoll`（中途切入）的终端分支更彻底：只清理、从不重挂。

另有一个 Live-dying 竞态：重挂的瞬间若 daemon 上一轮 operation 尚未 `complete()`
清理（broadcast 已无新事件），新 watch 连上 Live、收到重放的 user 后流即关闭、
没有终端事件 → 卡在 detached 状态，直到 2s tick 自愈（但不重挂）。

### 修复（`Chat.tsx`）

1. 新增 `settleToIdleWatch(projectHash, loadId, loadGeneration)`：清 detached
   状态（stop + hint + busy），**同步** `busyRef.current = false`（绕开异步
   `setBusy` 的旧值），再调 `startIdleWatch` 重挂待机。三个回合结束路径统一走它：
   - `startIdleWatch` 终端分支（升级态收 done）；
   - `startDetachedHistoryPoll` 终端分支（中途切入收 done）；
   - `startDetachedTick` 的 `stillActive=false` 分支（watch 死掉、tick 兜底判定回合结束）。
2. `startIdleWatch` 的 `watchChatSession(...).then()`：连接在**未收到终端事件**时被
   服务端关闭（Live-dying / daemon 重启 / 断网）且当前仍持有该 controller → 重新待机
   （`activated && !terminalSeen` 走 `settleToIdleWatch`，纯 standby 直接重挂），
   避免下一个 turn 永久失联。正常收尾（终端已处理、controller 已移交）由 guard 跳过，不重复。

### 验证

- 测试 daemon 上两轮实测：第一轮 watch（standby→升级）收 `user+reply+done`，流关闭；
  重挂新 watch 待机 → 第二轮收 `runtime_info → user(BETA) → text`，顺序正确、无重复。
- WebUI `tsc --noEmit` 通过、101 单测全过。

---

## P1.7：本端 WebUI 发送时消息/回复双份 —— 待机 watch 冗余渲染（新增）

### 根因（`Chat.tsx` + daemon 语义）

`POST /chat` 也走 `ActiveChatRegistry::admit` → 会把该会话的 **standby 待机 watch
drain 进 fan-out**。于是用户在 WebUI 里（API 创建的、正被待机 watch 观察的存量会话）
发消息时，**同一条事件同时走两个流**到达本端：

1. `/chat` POST 主流（`streamChat` → `handleEvent`，渲染方）；
2. 待机 watch 流（`handleEvent(observerOnly)`，原本只为观察 API turn 而设）。

后果：
- `user` 回显：主流用 `pendingSelfEchoRef` 去重并**清空 ref**；watch 流随后到达时
  ref 已空 → 再 append 一条用户消息（+ 乐观插入 = 2~3 条）。
- `text` 增量：两个流都调 `appendToLastAssistant` → 回复文本翻倍。
- 升级路径还会把 recovery 置 `detached_active`（allowSend=false）。

新建会话没有待机 watch，所以只在「API 创建的存量会话」这类场景复现。

### 修复（`Chat.tsx`）

1. `startIdleWatch` 回调顶部：**本端自己的 `/chat` turn 在途时**（`abortRef.current`
   非空 = 本端 `POST /chat` 流式未结束）直接 `return` —— 不渲染、不升级、不碰
   recovery，主流 `streamChat` 是唯一渲染方；本回合结束 forwarder 断开 → 流关闭 →
   `.then()` 重新待机，不丢下一个 API turn。API/对端 turn（`abortRef` 为空）行为不变。
2. `settleToIdleWatch` 补 `transitionChatRecovery({ type: 'authoritative_terminal' })`：
   观察结束回空闲时把 recovery 复位为 `ready`（之前漏了，API turn 观察结束后
   allowSend 可能一直停在 false，输入框被锁）。
3. `.then()` 未激活分支绕过 `busyRef` 守卫重挂（本端 turn 刚结束时 busy 复位与
   watch 流关闭存在竞态，待机连接不干扰 busy 状态管理）。

### 验证

- 本端发送（abortRef 在途）→ watch 跳过 → 仅 `/chat` 主流渲染：用户消息 1 条、回复 1 份。
- API turn（abortRef 为空）→ watch 照常 standby→升级渲染，行为不变。
- WebUI `tsc --noEmit` 通过、101 单测全过。

---

## P1.8：本端发送「2~3 条 + 回复不流式」—— 普通路径漏了 pendingSelfEchoRef（新增）

### 根因（`Chat.tsx` `deliver` 普通路径 + round-0 引入的 `/chat` 流 `user` 回显）

round-0 起 `POST /chat` 的 SSE 流也带 `ChatEvent::User` 回显，但 **`deliver` 的
普通（非 sync）路径只做了乐观插入、没有设置 `pendingSelfEchoRef`**（它只在 sync
路径设置）。于是：

1. 乐观插入 `[U(opt), A(空)]`；
2. `/chat` 流 `user` 回显 → `pendingSelfEchoRef` 为 null → 去重失败 → 追加 `U2`
   并把末尾 `A(空)` 占位挤掉；
3. 随后的 `text` 增量 → `appendToLastAssistant` 见末条是 **user** → 直接丢弃 →
   **回复不流式**；
4. round-3 的 abortRef 跳过又移除了 idle watch 的兜底渲染（round-2 里回复是靠
   idle watch 才显示的）→ 症状彻底暴露：2~3 条用户消息、回复空白；刷新后从历史
   加载 1 条用户消息 + 回复，正常。

### 修复（`Chat.tsx` `deliver` 普通路径）

1. 乐观插入后、POST 前设置 `pendingSelfEchoRef.current = text`（与 sync 路径一致）：
   `/chat` 流 `user` 回显据此去重，不再追加 U2，`A(空)` 保持末位 → `text` 正常累加。
2. `finally` 兜底 `pendingSelfEchoRef.current = null`：回显匹配已清空；失败/中止从未
   收到回显时清掉，避免残留值误吞后续对端（API/别的 tab）同文本用户消息。

两处修复必须同时存在：pendingSelfEcho 修 `/chat` 主流回显去重，abortRef 跳过（P1.7）
修 idle watch 冗余渲染。二者缺一仍会双份或丢回复。

### 验证

- 本端发送：1 条用户消息（乐观，回显去重）+ 流式回复正常渲染。
- 失败/中止路径：`finally` 清 ref，对端同文本消息不被误吞。
- WebUI `tsc --noEmit` 通过、101 单测全过。

---

## P1.9：交互审批死锁 —— permission_request 在等待决策之后才发出（daemon bug）

### 根因（`live_api.rs` `run_chat_turn_v2` APPROVAL_KIND 分支）

顺序颠倒导致死锁：

```rust
let decision = match &mut perm_rx {
    Some(rx) => tokio::select! { decision = rx.recv() => ... },  // ① 先等用户决策
};
if perm_rx.is_some() {
    runtime_event_tx.send(Request);  // ② 决策后才把 permission_request 发给 WebUI
}
```

`permission_request`（WebUI 弹审批卡的 SSE 事件）在 `tokio::select!` **之后**才发送——
但用户必须看到卡片才会 POST `/chat/permission` 产生决策 → **永久死锁**：kernel 请求审批、
daemon 一直等、WebUI 只有 busy 光标（「光标在闪、不弹窗」）。

日志佐证（新增诊断日志后）：`approval round-trip (has_interactive_responder=true)` 到达，
但 `PERMISSION REQUEST emitted` 缺失、turn 无 done → 卡死。user_input 分支顺序正确
（先注册 + 发事件再等），故仅 APPROVAL 分支有此 bug。

### 修复（`live_api.rs`）

把 `if perm_rx.is_some() { runtime_event_tx.send(Request) }` **移到 `tokio::select!` 之前**：
先发 `permission_request` → WebUI 弹卡 → 用户决策 → `/chat/permission` → `rx` 解析 → 放行。
Auto/YOLO/无 responder（`perm_rx=None`）仍走 fallback，不弹卡，行为不变。

### 验证

- 日志确认：策略 `interactive_permission=true`、`registered_permission_responder=true`；
  round-trip 到达后 `PERMISSION REQUEST emitted` 先出现，然后才等待决策。
- 实测（部署后）：Build 模式要求 `rm -rf` → WebUI 弹审批卡 → allow → 工具执行。

---

## P2：yolo 下子代理立即 [done] —— 真相澄清（非 daemon bug）

`[done] session=1fcd1523… user=demo_1 model=newnew/auto` 后回到 `PS E:\code\agents\atomcode-sdk>` —— 这条 `[done]` 日志来自 **`atomcode-sdk/examples/_common.py:361`**，是 SDK **单回合客户端脚本**在收到 DONE 事件后正常打印并 return 退出。**不是 `atomcode serve --yolo` daemon 进程崩溃**。

链路：`newnew/auto`（auto-routing 弱模型）把"正在启动四个子代理检索文件"写成 assistant 文本 content，但在 OpenAI `tool_calls` 字段里 **没有产出 `task` 调用** → kernel `pending_calls.is_empty()` (`agent.rs:2753`) → `StopReason::Stopped` → daemon 发 DONE → SDK 客户端打 `[done]` 退。

`yolo` vs 纯 `api_automation` 字段完全相同，唯一差别是 yolo 卸载 `request_user_input`（与 `task` 描述无关、子代理也不挂该工具）。**不带 yolo 用 API 大概率复现同一现象**。daemon 侧无 bug 修复点。

### P2 可选缓解（opt-in，默认关，防回归）

daemon 已支持 `ToolChoice::Required`（`openai_compat.rs:984-1019`，调用方主动指定时生效）。新增 config 项，让 API/yolo 路径对指定弱模型强制 `tool_choice=required`：

- `atomcode-config`：新增 `[automation] force_tool_choice_models = ["newnew/auto"]`（或全局开关，仅作用于 api/yolo origin）。
- `compat_api.rs`：turn 准入后若 origin=Api/yolo 且模型匹配，注入 `tool_choice=required`。
- **风险**：强制 required 会让模型每回合必须调工具、不能纯文本回答 → 仅对边界弱模型 opt-in，不默认开。

此项可后续单独做，不阻塞 P1 修复。

---

## 不做什么

- 不改 daemon `ChatTurnPolicy` / `compat_api.rs` 策略层（设计边界已正确）。
- `/chat/watch` 端点 fan-out 语义不变；仅 `Live` 分支补了「已记录用户消息的 replay」。
- 不改 SDK examples（`_common.py` 单回合退是设计）。
- P2 daemon 侧缓解默认关，防回归。

## 测试计划

- [ ] `atomcode serve`（不带 yolo），用 OpenAI 兼容 API 发起长 turn（多工具/长输出）；中途 WebUI 切入该会话：流式 text/tool 行正常累加，不被 2s tick 抹掉，不出现空 bubble 割裂。
- [ ] API turn 结束后 WebUI 自动停止 busy，最终历史 hydrate 与磁盘一致。
- [ ] detached 期间点发送：输入框 disabled，显式提示被占用，不误导"光标转圈没反应"。
- [ ] `--yolo` 下同上回归（webui 观察行为应一致）。
- [ ] WebUI 自己发起的 turn（不经 API）：审批/用户输入模态照常（原生），不受 detached 修复影响。
- [ ] （可选）给 `newnew/auto` 配 `force_tool_choice_models`，验证 yolo 下不再"叙述完即 done"。