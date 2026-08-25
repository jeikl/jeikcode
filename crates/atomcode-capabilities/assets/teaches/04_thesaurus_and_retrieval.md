# 04 - 词林体系与代码检索相关性配置指南 (Thesaurus & Code Retrieval)

## 1. 词林（Thesaurus）机制与作用

在软件工程中，开发者常使用中文自然语言提问（如“用户鉴权在哪里校验”、“订单扣减库存防超卖逻辑”），而代码中的类名、函数名、字段名大多为英文（如 `authenticate()`, `deductStock()`, `preventOversell()`）。

**词林系统（`thesaurus`）** 是 JeikCode 代码智能（`codeintel`）引擎的核心基础设施：
- **双语对齐**：构建中英文概念的多对多同义映射网络；
- **混合检索赋能**：为 `code_explore`、`repo_map` 和语义向量空间提供精准的相关性扩召回与精确匹配；
- **彻底避免漏查**：大幅减少模型因中英文表达差异导致的“未找到对应代码”假阴性误判。

---

## 2. 词林文件位置与加载策略

系统自动动态加载以下路径中的所有 `*.txt` 和 `*.dict` 词林文件：
1. **工作区项目专属词林**：`<workspace>/.atomcode/thesaurus/*.txt`（优先加载，适合业务专有领域术语）。
2. **全局用户词林**：`~/.atomcode/thesaurus/*.txt`（全局生效）。
3. **内嵌默认词库**：二进制内嵌的通用计算机与常见领域基础词林。

---

## 3. 词林映射语法规范

词林文件采用极其简明的人类与 AI 友好格式：

```text
# 注释行以 # 或 // 开头
中文词1, 中文词2 = en_word1, en_word2, en_word3
```

### 规则要点：
- **分隔符**：等号 `=` 或双向符号 `<=>` 分割中英文；
- **词组分隔**：支持英文逗号 `,`、中文逗号 `，`、竖线 `|`、斜杠 `/`；
- **支持关系**：支持 1:1、1:N、N:1、N:M 自由多对多映射。

---

## 4. 默认内置领域词林速查表

`~/.atomcode/thesaurus/` 内置 9 大核心领域词林文件：

| 词林文件名 | 覆盖专业领域 | 核心词条举例 |
| :--- | :--- | :--- |
| `admin_system.txt` | 管理后台 / RBAC 权限 | 权限, 资源权限, 鉴权 = permission, auth, rbac, guard, privilege |
| `agent_core.txt` | AI Agent / 智能体核心 | 提示词, 系统指令, 工具调用 = prompt, tool_call, function_call |
| `ai_agent.txt` | 大模型交互与运行流 | 流式输出, 轮次循环, 记忆 = stream, turn_loop, memory, context |
| `computer_science.txt` | 计算机体系与基础算法 | 事务, 锁, 队列, 堆栈, 协程 = transaction, lock, queue, stack, coroutine |
| `ecommerce.txt` | 电商与交易支付系统 | 订单, 购物车, 库存, 防超卖 = order, cart, stock, inventory, oversell |
| `fullstack_dev.txt` | 全栈开发与 Web 架构 | 控制器, 路由, 拦截器, 持久层 = controller, route, interceptor, repository |
| `medical.txt` | 医疗与健康信息化 | 患者, 就诊, 病历, 处方 = patient, visit, emr, diagnosis, prescription |
| `robotics.txt` | 机器人与具身智能 | 位姿, 机械臂, 运动学, 避障 = pose, manipulator, kinematics, navigation |
| `web_http.txt` | 网络通信与 HTTP 协议 | 鉴权头, 状态码, 请求体, 跨域 = authorization, status_code, payload, cors |

---

## 5. 为您的项目添加专属业务词林

例如在当前项目根目录创建 `.atomcode/thesaurus/biz_terms.txt`：

```text
# 自定义业务名词对照
风控, 反欺诈, 黑名单 = risk_control, anti_fraud, blacklist, risk_engine
积分商城, 积分兑换 = points_mall, point_exchange, credit_reward
```

保存后，`code_explore` 工具在执行中文检索时将自动关联 `risk_control`、`points_mall` 等英文代码路径。
