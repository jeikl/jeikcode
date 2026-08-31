//! Supervised concept-root semantic vectors (no model inference).
//!
//! Instead of random feature hashing, each symbol/query is projected onto a
//! fixed set of CONCEPT ROOTS — curated (中文词族, 英文词族) pairs where the
//! two sides share a semantic axis. The resulting sparse vector is
//! L2-normalized and used directly as the "semantic embedding": every
//! dimension aligns with a human-meaningful semantic axis, so cosine
//! similarity between a Chinese query and English code is meaningful without
//! any LLM. Roots can be extended by an optional `.atomcode/concepts.toml`
//! (same `中文词 = en1, en2` syntax as the thesaurus).

use std::collections::HashSet;

use super::super::bilingual_nlp::{is_cjk, split_identifier};

/// A concept root: CJK characters/words and English stems sharing one axis.
struct ConceptRoot {
    cn: &'static [&'static str],
    en: &'static [&'static str],
}

/// Curated concept roots. Extend freely; every entry adds one semantic axis.
/// Order is stable — dimension i always means the same concept.
const CONCEPT_ROOTS: &[ConceptRoot] = &[
    // ── commerce / business domain ────────────────────────────────
    ConceptRoot {
        cn: &["券", "折扣"],
        en: &["coupon", "voucher", "ticket", "discount"],
    },
    ConceptRoot {
        cn: &["购", "买", "订单", "下单"],
        en: &["buy", "purchase", "order", "cart", "checkout"],
    },
    ConceptRoot {
        cn: &["领", "取", "拿"],
        en: &["claim", "receive", "fetch", "get"],
    },
    ConceptRoot {
        cn: &["用", "户", "账号", "会员"],
        en: &["user", "account", "member", "profile"],
    },
    ConceptRoot {
        cn: &["付", "款", "费", "账"],
        en: &["pay", "payment", "charge", "bill", "invoice"],
    },
    ConceptRoot {
        cn: &["退", "撤", "回滚", "冲正"],
        en: &["refund", "revert", "rollback", "void", "cancel"],
    },
    ConceptRoot {
        cn: &["库", "存", "仓", "余量"],
        en: &["stock", "inventory", "warehouse", "quota", "balance"],
    },
    ConceptRoot {
        cn: &["扣", "减", "递减"],
        en: &["deduct", "decrease", "reduce", "decrement", "sub"],
    },
    ConceptRoot {
        cn: &["锁", "互斥"],
        en: &["lock", "mutex", "guard", "acquire", "semaphore"],
    },
    ConceptRoot {
        cn: &["单", "条", "项"],
        en: &["item", "trade", "deal", "entry", "record"],
    },
    ConceptRoot {
        cn: &["结算", "对账", "结账"],
        en: &["settle", "reconcile", "checkout"],
    },
    ConceptRoot {
        cn: &["超卖", "缺货"],
        en: &["oversell", "out_of_stock", "sold_out"],
    },
    ConceptRoot {
        cn: &["审批", "审核", "通过"],
        en: &["approve", "approval", "review", "pass"],
    },
    ConceptRoot {
        cn: &["驳回", "拒绝"],
        en: &["reject", "decline", "refuse"],
    },
    // ── query / storage / backend ──────────────────────────────────
    ConceptRoot {
        cn: &["查", "检", "搜", "查询"],
        en: &["query", "find", "search", "lookup", "select"],
    },
    ConceptRoot {
        cn: &["建", "增", "添", "创建"],
        en: &["create", "add", "insert", "new", "save"],
    },
    ConceptRoot {
        cn: &["改", "更", "编", "更新"],
        en: &["update", "modify", "edit", "patch"],
    },
    ConceptRoot {
        cn: &["删", "除", "移除"],
        en: &["delete", "remove", "drop", "clear"],
    },
    ConceptRoot {
        cn: &["数", "据", "库", "表", "实体"],
        en: &["database", "db", "table", "entity", "schema"],
    },
    ConceptRoot {
        cn: &["事务", "提交", "持久"],
        en: &["transaction", "commit", "persist", "durable"],
    },
    ConceptRoot {
        cn: &["缓", "存"],
        en: &["cache", "redis", "warm"],
    },
    ConceptRoot {
        cn: &["存", "储", "文件"],
        en: &["store", "storage", "file", "fs"],
    },
    ConceptRoot {
        cn: &["读", "取"],
        en: &["read", "load", "open"],
    },
    ConceptRoot {
        cn: &["写", "录", "落盘"],
        en: &["write", "save", "dump", "flush"],
    },
    // ── auth / security / permission ───────────────────────────────
    ConceptRoot {
        cn: &["权", "鉴", "密", "授权"],
        en: &["auth", "perm", "token", "jwt", "access"],
    },
    ConceptRoot {
        cn: &["登", "录", "密钥"],
        en: &["login", "key", "secret", "credential"],
    },
    ConceptRoot {
        cn: &["加", "密", "签名", "哈希"],
        en: &["encrypt", "sign", "hash", "digest"],
    },
    ConceptRoot {
        cn: &["沙", "箱", "隔", "离"],
        en: &["sandbox", "isolate", "container"],
    },
    // ── agent / harness core ───────────────────────────────────────
    ConceptRoot {
        cn: &["智", "能", "体", "代", "理", "助手"],
        en: &["agent", "assistant", "bot", "worker"],
    },
    ConceptRoot {
        cn: &["主", "循", "环", "轮", "次"],
        en: &["loop", "turn", "round", "cycle", "driver"],
    },
    ConceptRoot {
        cn: &["执", "行", "运行", "跑"],
        en: &["execute", "run", "invoke", "dispatch"],
    },
    ConceptRoot {
        cn: &["调", "度", "任务", "作业"],
        en: &["schedule", "task", "job", "scheduler"],
    },
    ConceptRoot {
        cn: &["采", "样", "生成", "推理"],
        en: &["sample", "sampler", "generate", "infer"],
    },
    ConceptRoot {
        cn: &["工", "具", "调用"],
        en: &["tool", "call", "invocation", "function_call"],
    },
    ConceptRoot {
        cn: &["参", "数", "参数", "字段"],
        en: &["arg", "param", "argument", "field", "input"],
    },
    ConceptRoot {
        cn: &["错", "误", "异常", "失败"],
        en: &["error", "fail", "exception", "throw"],
    },
    ConceptRoot {
        cn: &["重", "试", "退", "避"],
        en: &["retry", "backoff", "attempt", "resample"],
    },
    ConceptRoot {
        cn: &["熔", "断", "死循环", "停滞"],
        en: &[
            "circuit",
            "breaker",
            "doom",
            "loop_guard",
            "stall",
            "echo_loop",
        ],
    },
    ConceptRoot {
        cn: &["压", "缩", "紧凑", "摘要"],
        en: &["compact", "summar", "condense", "truncate"],
    },
    ConceptRoot {
        cn: &["会", "话", "对话"],
        en: &["session", "conversation", "chat"],
    },
    ConceptRoot {
        cn: &["子", "代", "理", "分", "支"],
        en: &["subagent", "child", "fork", "spawn"],
    },
    ConceptRoot {
        cn: &["钩", "子", "插件", "扩展"],
        en: &["hook", "plugin", "extension", "middleware"],
    },
    ConceptRoot {
        cn: &["技", "能"],
        en: &["skill"],
    },
    ConceptRoot {
        cn: &["提", "醒", "催", "促", "nudge"],
        en: &["remind", "nudge", "steer", "prompt"],
    },
    ConceptRoot {
        cn: &["流", "式", "事件", "消息"],
        en: &["stream", "event", "message", "chunk", "delta"],
    },
    ConceptRoot {
        cn: &["队", "列", "缓冲"],
        en: &["queue", "buffer", "channel", "pending"],
    },
    ConceptRoot {
        cn: &["超", "时", "取消", "中断"],
        en: &["timeout", "cancel", "abort", "interrupt"],
    },
    ConceptRoot {
        cn: &["等", "待", "轮", "询"],
        en: &["wait", "poll", "sleep", "park"],
    },
    ConceptRoot {
        cn: &["目", "标", "计划", "规划"],
        en: &["goal", "plan", "objective", "strateg"],
    },
    ConceptRoot {
        cn: &["验", "证", "审查", "检查"],
        en: &["verif", "review", "check", "inspect", "audit"],
    },
    ConceptRoot {
        cn: &["测", "试", "基准", "性能"],
        en: &["test", "bench", "benchmark", "perf"],
    },
    ConceptRoot {
        cn: &["内", "存", "堆", "资源"],
        en: &["memory", "heap", "alloc", "resource"],
    },
    ConceptRoot {
        cn: &["恢", "复", "重启", "重连"],
        en: &["recover", "restart", "reconnect", "resume"],
    },
    ConceptRoot {
        cn: &["快", "照", "检查点", "存档"],
        en: &["snapshot", "checkpoint", "archive"],
    },
    ConceptRoot {
        cn: &["日", "志", "遥测", "监控", "指标"],
        en: &["log", "telemetry", "monitor", "metric", "trace"],
    },
    ConceptRoot {
        cn: &["配", "置", "设置", "开关"],
        en: &["config", "setting", "option", "flag", "toggle"],
    },
    ConceptRoot {
        cn: &["上", "传", "同步", "下载"],
        en: &["upload", "sync", "download", "push"],
    },
    // ── code intelligence / retrieval ──────────────────────────────
    ConceptRoot {
        cn: &["索", "引", "检索", "召回"],
        en: &["index", "retriev", "recall", "rank"],
    },
    ConceptRoot {
        cn: &["向", "量", "嵌入", "语义"],
        en: &["vector", "embed", "semantic", "cosine"],
    },
    ConceptRoot {
        cn: &["图", "拓扑", "调用", "依赖"],
        en: &["graph", "topolog", "caller", "callee", "depend"],
    },
    ConceptRoot {
        cn: &["目", "录", "路径", "文件"],
        en: &["dir", "folder", "path", "directory"],
    },
    ConceptRoot {
        cn: &["符", "号", "解析"],
        en: &["symbol", "parse", "token", "ast"],
    },
    ConceptRoot {
        cn: &["片", "段", "切片", "范围"],
        en: &["span", "snippet", "range", "slice"],
    },
    ConceptRoot {
        cn: &["词", "林", "同义", "扩展"],
        en: &["thesaurus", "synonym", "expand"],
    },
    ConceptRoot {
        cn: &["邻", "近", "兄弟", "父子"],
        en: &["adjacent", "sibling", "parent", "subtree"],
    },
];

/// Number of semantic axes (dimension of the projection vectors).
pub const CONCEPT_DIM: usize = CONCEPT_ROOTS.len();

/// Project text + expanded thesaurus terms onto the concept axes.
/// Returns an L2-normalized vector; dim = [`CONCEPT_DIM`].
pub fn concept_projection(text: &str, expanded: &HashSet<String>) -> Vec<f32> {
    let mut vec = vec![0.0f32; CONCEPT_DIM];
    let lower = text.to_ascii_lowercase();

    for (i, root) in CONCEPT_ROOTS.iter().enumerate() {
        let mut hit = false;
        // CJK side: any char of the root appears in the text.
        for cn in root.cn {
            if cn.chars().count() == 1 {
                if text.contains(*cn) {
                    hit = true;
                    break;
                }
            } else if text.contains(*cn) {
                hit = true;
                break;
            }
        }
        // EN side: any stem is a substring (stem prefix matches identifiers).
        if !hit {
            for en in root.en {
                if lower.contains(en) {
                    hit = true;
                    break;
                }
            }
        }
        // Expanded thesaurus terms also project (bilingual bridge).
        if !hit {
            for en in root.en {
                if expanded.iter().any(|e| e.contains(en)) {
                    hit = true;
                    break;
                }
            }
        }
        if hit {
            vec[i] = 1.0;
        }
    }

    // Identifier subword projection: camelCase/snake_case parts also fire roots
    // (e.g. `execute_tool_calls` → "execute" + "tool" + "call").
    for word in split_identifier(&lower) {
        if word.len() < 3 {
            continue;
        }
        for (i, root) in CONCEPT_ROOTS.iter().enumerate() {
            if root.en.iter().any(|en| word == *en || word.starts_with(en)) {
                vec[i] += 0.5;
            }
        }
    }

    // L2 normalize.
    let mut norm = 0.0f32;
    for v in &vec {
        norm += v * v;
    }
    let norm = norm.sqrt();
    if norm > 1e-6 {
        for v in &mut vec {
            *v /= norm;
        }
    }
    vec
}

/// Cosine similarity between two concept vectors (0..1 after clipping).
pub fn concept_cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
    }
    dot.max(0.0).min(1.0) as f64
}

/// Whether text contains CJK (helper for tests / query shape detection).
pub fn contains_cjk(text: &str) -> bool {
    text.chars().any(is_cjk)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chinese_query_and_english_code_share_semantic_axis() {
        let mut expanded = HashSet::new();
        expanded.insert("loop".to_string());
        expanded.insert("run_turn".to_string());
        let q = concept_projection("主循环", &expanded);
        let code = concept_projection("fn run_turn_via_sampler", &HashSet::new());
        let sim = concept_cosine(&q, &code);
        assert!(sim > 0.05, "chinese loop query vs run_turn code: {sim}");
    }

    #[test]
    fn unrelated_text_has_lower_similarity() {
        let q = concept_projection("优惠券", &HashSet::new());
        let loop_code = concept_projection("fn run_turn", &HashSet::new());
        let coupon_code = concept_projection("claim_discount_coupon", &HashSet::new());
        let sim_loop = concept_cosine(&q, &loop_code);
        let sim_coupon = concept_cosine(&q, &coupon_code);
        assert!(
            sim_coupon > sim_loop,
            "coupon query must be closer to coupon code ({sim_coupon}) than loop code ({sim_loop})"
        );
    }

    #[test]
    fn dimension_is_stable() {
        assert_eq!(CONCEPT_DIM, CONCEPT_ROOTS.len());
        let v = concept_projection("agent loop", &HashSet::new());
        assert_eq!(v.len(), CONCEPT_DIM);
    }
}
