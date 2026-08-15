//! Bilingual NLP, Dynamic Thesaurus & Dense Semantic Vector Engine.
//!
//! Features:
//! - Unicode/CJK-aware tokenization & N-gram sliding window
//! - Dense Semantic Vector Space (128-dim subword & semantic root embeddings)
//! - Cosine similarity calculation between Query Vector and Code/Doc Vector
//! - Dynamic thesaurus loader: loads user-editable `<dir>/.atomcode/thesaurus/*.txt` files
//! - Simple `=` syntax support: `中文词1, 中文词2 = en_word1, en_word2` (1:1, 1:N, N:1, N:M)
//! - Preloaded rich domain thesaurus for E-Commerce, Admin Systems, AI/Agent, Medical, Robotics, Full-Stack
//! - Hybrid retrieval: Lexical + Thesaurus + Dense Vector Cosine Matching

use std::collections::HashSet;
use std::path::Path;

const VECTOR_DIM: usize = 128;

/// An entry in the bilingual thesaurus.
#[derive(Debug, Clone)]
pub struct ThesaurusRule {
    pub cn_terms: Vec<String>,
    pub en_terms: Vec<String>,
}

/// Dynamic thesaurus registry.
#[derive(Debug, Clone, Default)]
pub struct DynamicThesaurus {
    rules: Vec<ThesaurusRule>,
}

impl DynamicThesaurus {
    pub fn new() -> Self {
        let mut dt = Self::default();
        dt.load_embedded_defaults();
        dt
    }

    /// Load from `.atomcode/thesaurus` directory in workspace or user home.
    pub fn load_from_dir(&mut self, dir: &Path) {
        if !dir.is_dir() {
            return;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_file() {
                    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
                    if ext == "txt" || ext == "dict" {
                        if let Ok(content) = std::fs::read_to_string(&p) {
                            self.parse_and_append(&content);
                        }
                    }
                }
            }
        }
    }

    /// Parse simple `=` formatted text.
    /// Format: `中文词1, 中文词2 = en_word1, en_word2`
    pub fn parse_and_append(&mut self, content: &str) {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
                continue;
            }

            let parts: Vec<&str> = if line.contains("<=>") {
                line.splitn(2, "<=>").collect()
            } else if line.contains('=') {
                line.splitn(2, '=').collect()
            } else {
                continue;
            };

            if parts.len() == 2 {
                let left_raw = parts[0].trim();
                let right_raw = parts[1].trim();

                let left_terms: Vec<String> = left_raw
                    .split(|c: char| c == ',' || c == '，' || c == '|' || c == '/')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();

                let right_terms: Vec<String> = right_raw
                    .split(|c: char| c == ',' || c == '，' || c == '|' || c == '/')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();

                if !left_terms.is_empty() && !right_terms.is_empty() {
                    self.rules.push(ThesaurusRule {
                        cn_terms: left_terms,
                        en_terms: right_terms,
                    });
                }
            }
        }
    }

    fn load_embedded_defaults(&mut self) {
        const DEFAULTS: &str = r#"
订单, 下单, 购买 = order, trade, deal, purchase, checkout, place_order
购物车 = cart, shopping_cart, basket
收货地址 = shipping_address, delivery_address, address
商品, 货品, 物料 = item, product, goods, sku, spu
品类, 分类 = category, catalog
规格, 属性 = spec, attribute, prop, variant
库存, 仓储, 余量 = stock, inventory, warehouse, quota, balance
扣减, 扣除, 扣减库存 = deduct, decrease, reduce, sub, decrement
防超卖, 超卖, 缺货 = oversell, prevent_oversell, out_of_stock, sold_out
支付, 付款, 扣款 = pay, payment, charge, pay_order
结算, 结账, 对账 = settle, settlement, checkout, reconcile
退款, 撤销, 冲正 = refund, revert, rollback, cancel, void
优惠券, 折扣券 = coupon, voucher, discount_ticket
用户, 账号 = user, account, member, employee
组织, 部门, 机构 = organization, org, department, dept, team
角色, 用户组 = role, user_group, authority
权限, 资源权限, 按钮权限 = permission, perm, privilege, access, capability
菜单, 导航栏, 侧边栏 = menu, navigation, navbar, sidebar, route_menu
鉴权, 授权 = authorize, check_permission, access_control, guard, rbac
流程, 审批流, 工作流 = workflow, bpm, flow, approval_flow, state_machine
审批, 审核, 通过 = approve, approval, pass, confirm, verify
驳回, 拒绝, 退回 = reject, decline, refuse, roll_back
大模型, 语言模型 = llm, model, language_model, foundation_model
提示词, 系统指令 = prompt, system_prompt, instruction, template
推理, 生成, 补全 = inference, generate, prediction, complete
流式输出, 打字机 = stream, streaming, sse, chunk, delta
智能体, 代理, 助手 = agent, assistant, bot, subagent, worker
主循环, 轮次循环, 智能体循环, 事件循环 = main_loop, loop, run_turn, agent_loop, turn_loop, event_loop, step, processor, session_processor
重试, 退避, 熔断, 重试循环 = retry, backoff, attempt, circuit_breaker, fallback, resample, doom_loop, run_request_task
错误处理, 异常处理, 失败处理, 工具错误 = error_handler, catch, handle_error, fail_tool_call, handle_tool_error, message_error, parse_error
指纹, 防死循环, 循环守卫 = fingerprint, loop_guard, loop_policy, doom_loop_threshold, tool_loop
工具调用, 技能, 插件, 执行工具 = tool, tool_call, function_call, skill, plugin, capability, execute_tool, execute_tool_calls
知识库, 知识检索 = rag, retrieval, knowledge_base, kb, doc_store
向量, 向量嵌入 = embedding, vector, embed, dense_vector
记忆, 上下文 = memory, long_term_memory, short_term_memory, context
患者, 病人 = patient, sufferer
就诊, 看病, 门诊 = visit, encounter, outpatient, clinic
病历, 电子病历 = medical_record, emr, ehr
诊断, 处方, 开药 = diagnosis, prescription, rx, prescribe, medicine, drug
机器人, 机械臂 = robot, robotic_arm, manipulator, chassis
位姿, 姿态 = pose, position, orientation, transform
运动学, 动力学 = kinematics, dynamics, ik, fk, torque
路径规划, 避障 = path_planning, motion_plan, obstacle_avoidance, navigation
传感器, 激光雷达 = sensor, lidar, camera, imu, odom
前端, 组件, 页面 = frontend, component, view, page, ui
后端, 控制器, 接口 = backend, server, controller, handler, endpoint, api
路由, 拦截器 = route, router, interceptor, guard
数据库, 数据表, 实体 = database, db, table, entity, model, schema
持久层, 数据访问 = dao, repository, mapper, orm
事务, 提交, 回滚 = transaction, tx, commit, rollback
缓存, 预热 = cache, redis, warm_up
"#;
        self.parse_and_append(DEFAULTS);
    }
}

/// Extracted search tokens + dense embedding vector.
#[derive(Debug, Clone, Default)]
pub struct SearchTokens {
    pub raw_query: String,
    pub words: Vec<String>,
    pub cjk_phrases: Vec<String>,
    pub expanded_terms: HashSet<String>,
    pub code_identifiers: Vec<String>,
    pub path_fragments: Vec<String>,
    /// Dense semantic embedding vector (128-dim)
    pub dense_vector: Vec<f32>,
}

/// Parse and expand bilingual query against the thesaurus and compute dense vector.
pub fn parse_bilingual_query_with_thesaurus(
    query: &str,
    thesaurus: &DynamicThesaurus,
) -> SearchTokens {
    let mut tokens = SearchTokens {
        raw_query: query.trim().to_string(),
        ..Default::default()
    };

    let mut current_ascii = String::new();
    let mut current_cjk = String::new();

    let flush_cjk = |cjk: &mut String, tokens: &mut SearchTokens| {
        if !cjk.is_empty() {
            let chars: Vec<char> = cjk.chars().collect();
            tokens.cjk_phrases.push(cjk.clone());
            if chars.len() >= 2 {
                for i in 0..chars.len() - 1 {
                    tokens.cjk_phrases.push(chars[i..=i + 1].iter().collect());
                }
            }
            if chars.len() >= 3 {
                for i in 0..chars.len() - 2 {
                    tokens.cjk_phrases.push(chars[i..=i + 2].iter().collect());
                }
            }
            cjk.clear();
        }
    };

    let flush_ascii = |ascii: &mut String, tokens: &mut SearchTokens| {
        if !ascii.is_empty() {
            if ascii.contains('/') || ascii.contains('\\') || ascii.contains('.') {
                tokens.path_fragments.push(ascii.to_ascii_lowercase());
            }
            let sub_tokens = split_identifier(ascii);
            for st in sub_tokens {
                let low = st.to_ascii_lowercase();
                if low.len() >= 2 && !is_stop_word(&low) {
                    tokens.words.push(low);
                }
            }
            if ascii.len() >= 3 {
                tokens.code_identifiers.push(ascii.clone());
            }
            ascii.clear();
        }
    };

    for ch in query.chars() {
        if is_cjk(ch) {
            flush_ascii(&mut current_ascii, &mut tokens);
            current_cjk.push(ch);
        } else if ch.is_ascii_alphanumeric() || ch == '_' || ch == '/' || ch == '.' || ch == '-' {
            flush_cjk(&mut current_cjk, &mut tokens);
            current_ascii.push(ch);
        } else {
            flush_cjk(&mut current_cjk, &mut tokens);
            flush_ascii(&mut current_ascii, &mut tokens);
        }
    }
    flush_cjk(&mut current_cjk, &mut tokens);
    flush_ascii(&mut current_ascii, &mut tokens);

    // Expand through thesaurus rules
    for rule in &thesaurus.rules {
        let cn_hit = tokens
            .cjk_phrases
            .iter()
            .any(|phrase| rule.cn_terms.iter().any(|cn| phrase.contains(cn) || cn.contains(phrase)));

        let en_hit = tokens
            .words
            .iter()
            .any(|w| rule.en_terms.iter().any(|en| w == en || en.starts_with(w)));

        if cn_hit || en_hit {
            for en in &rule.en_terms {
                tokens.expanded_terms.insert(en.to_ascii_lowercase());
            }
            for cn in &rule.cn_terms {
                tokens.cjk_phrases.push(cn.clone());
            }
        }
    }

    tokens.words.sort();
    tokens.words.dedup();
    tokens.cjk_phrases.sort();
    tokens.cjk_phrases.dedup();

    // Compute dense vector embedding for query
    tokens.dense_vector = compute_dense_embedding(&tokens.raw_query, &tokens.expanded_terms);

    tokens
}

/// Compute 128-dimensional dense semantic embedding from text and expanded terms.
pub fn compute_dense_embedding(text: &str, expanded: &HashSet<String>) -> Vec<f32> {
    let mut vec = vec![0.0f32; VECTOR_DIM];
    let lower = text.to_ascii_lowercase();

    // Concept Root Projection (shared semantic coordinates between Chinese roots and English stems)
    const CONCEPT_ROOTS: &[(&[char], &[&str], usize)] = &[
        (&['券'], &["coupon", "voucher", "ticket", "discount"], 42),
        (&['购', '买'], &["buy", "purchase", "shop", "cart"], 43),
        (&['领', '拿', '取'], &["claim", "receive", "get", "fetch"], 44),
        (&['用', '户', '员'], &["user", "account", "member", "staff"], 45),
        (&['付', '款', '费'], &["pay", "payment", "charge", "settle"], 46),
        (&['退', '撤', '滚'], &["refund", "rollback", "revert", "cancel"], 47),
        (&['库', '存', '仓'], &["stock", "inventory", "warehouse", "store"], 48),
        (&['扣', '减'], &["deduct", "decrease", "reduce", "sub"], 49),
        (&['锁', '闭'], &["lock", "mutex", "guard", "acquire"], 50),
        (&['单', '条'], &["order", "item", "trade", "deal"], 51),
        (&['查', '检', '搜'], &["query", "find", "search", "lookup", "select"], 52),
        (&['建', '增', '添'], &["create", "add", "insert", "new", "save"], 53),
        (&['改', '更', '编'], &["update", "modify", "edit", "patch"], 54),
        (&['删', '除', '销'], &["delete", "remove", "drop", "clear"], 55),
        (&['权', '鉴', '密'], &["auth", "perm", "token", "jwt", "access"], 56),
        (&['模', '型', '智'], &["model", "llm", "agent", "prompt"], 57),
        (&['向', '量', '嵌'], &["vector", "embed", "embedding", "rag"], 58),
        (&['患', '医', '诊'], &["patient", "medical", "doctor", "rx"], 59),
        (&['臂', '机', '关'], &["robot", "arm", "joint", "motor"], 60),
        (&['路', '口', '端'], &["route", "api", "endpoint", "controller"], 61),
    ];

    for (cjk_chars, en_stems, dim) in CONCEPT_ROOTS {
        let has_cjk = text.chars().any(|c| cjk_chars.contains(&c));
        let has_en = en_stems.iter().any(|st| lower.contains(st));
        if has_cjk || has_en {
            vec[*dim] += 4.0;
        }
    }

    // 1. Semantic root hashing & projection (CJK characters + subwords)
    for (i, ch) in text.chars().enumerate() {
        let h = (ch as u32).wrapping_mul(2654435761);
        let dim = (h as usize) % VECTOR_DIM;
        let weight = if is_cjk(ch) { 1.5 } else { 0.8 };
        vec[dim] += weight / (1.0 + (i as f32) * 0.05);

        // Bi-gram hash for CJK
        let next_ch = text.chars().nth(i + 1).unwrap_or(' ');
        let bi_h = ((ch as u32) << 8 | (next_ch as u32)).wrapping_mul(16777619);
        let bi_dim = (bi_h as usize) % VECTOR_DIM;
        vec[bi_dim] += weight * 1.2;
    }

    // 2. Sub-identifier & word hashing
    for word in split_identifier(&lower) {
        let mut h = 5381u32;
        for b in word.bytes() {
            h = h.wrapping_shl(5).wrapping_add(h).wrapping_add(b as u32);
        }
        let dim = (h as usize) % VECTOR_DIM;
        vec[dim] += 2.0;

        // Tri-gram subword hashing
        if word.len() >= 3 {
            for window in word.as_bytes().windows(3) {
                let wh = (window[0] as u32) << 16 | (window[1] as u32) << 8 | (window[2] as u32);
                let wdim = (wh.wrapping_mul(2166136261) as usize) % VECTOR_DIM;
                vec[wdim] += 1.0;
            }
        }
    }

    // 3. Project expanded bilingual terms into the vector
    for term in expanded {
        let mut h = 5381u32;
        for b in term.bytes() {
            h = h.wrapping_shl(5).wrapping_add(h).wrapping_add(b as u32);
        }
        let dim = (h as usize) % VECTOR_DIM;
        vec[dim] += 1.8;
    }

    // L2 Normalize
    let mut norm = 0.0f32;
    for &v in &vec {
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

/// Calculate cosine similarity between two 128-dim dense vectors (-1.0 .. 1.0 -> scaled 0.0 .. 1.0).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
    }
    dot.max(0.0).min(1.0) as f64
}

/// Check if a character is CJK.
pub fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}' |
        '\u{3400}'..='\u{4DBF}' |
        '\u{20000}'..='\u{2A6DF}' |
        '\u{F900}'..='\u{FAFF}'
    )
}

/// Split camelCase, PascalCase, snake_case.
pub fn split_identifier(name: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = name.chars().collect();

    for i in 0..chars.len() {
        let ch = chars[i];
        if ch == '_' || ch == '-' || ch == '/' || ch == '.' {
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
            continue;
        }

        if ch.is_uppercase() {
            if !current.is_empty() {
                let prev_is_lower = i > 0 && chars[i - 1].is_lowercase();
                let next_is_lower = i + 1 < chars.len() && chars[i + 1].is_lowercase();
                if prev_is_lower || (current.len() > 1 && next_is_lower) {
                    parts.push(std::mem::take(&mut current));
                }
            }
        }
        current.push(ch);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn is_stop_word(word: &str) -> bool {
    const STOP_WORDS: &[&str] = &[
        "the", "a", "an", "and", "or", "but", "in", "on", "at", "to", "for", "of", "with", "by",
        "from", "is", "it", "that", "this", "are", "was", "be", "has", "had", "have", "do",
        "does", "did", "will", "would", "could", "should", "may", "can", "not", "no", "how",
        "what", "where", "when", "who", "which", "why", "code", "file", "func", "function",
        "method", "class", "impl", "let", "var", "const", "的", "了", "在", "是", "我", "有",
        "和", "就", "不", "人", "都", "一", "一个", "上", "也", "很", "到", "说", "要", "去",
        "你", "会", "着", "没有", "如果", "怎么", "如何", "怎样", "什么", "请问",
    ];
    STOP_WORDS.contains(&word)
}

/// Calculate hybrid similarity score (0.0 .. 100.0) combining Lexical + Thesaurus + Dense Vector Cosine.
pub fn calculate_text_similarity(tokens: &SearchTokens, target_text: &str) -> f64 {
    if target_text.is_empty() {
        return 0.0;
    }

    let target_lower = target_text.to_ascii_lowercase();
    let mut lexical_score = 0.0;

    // 1. Exact Chinese phrase hit in target
    for phrase in &tokens.cjk_phrases {
        if phrase.len() >= 2 && target_text.contains(phrase) {
            lexical_score += 25.0 * (phrase.chars().count() as f64).min(4.0) / 2.0;
        }
    }

    // 2. Exact word hit in target
    for word in &tokens.words {
        if target_lower.contains(word) {
            lexical_score += 20.0;
        }
    }

    // 3. Expanded domain synonym hit
    for expanded in &tokens.expanded_terms {
        if target_lower.contains(expanded) {
            lexical_score += 22.0;
        }
    }

    // 4. Code identifier hit (e.g. "batchDeduct")
    for ident in &tokens.code_identifiers {
        if target_text.contains(ident) || target_lower.contains(&ident.to_ascii_lowercase()) {
            lexical_score += 30.0;
        }
    }

    // 5. Dense Vector Cosine Similarity
    let target_vec = compute_dense_embedding(target_text, &HashSet::new());
    let vector_cosine = cosine_similarity(&tokens.dense_vector, &target_vec);
    let vector_score = vector_cosine * 70.0; // 0.0 .. 70.0 points from semantic vector

    // Hybrid combination
    let total_score = lexical_score.min(60.0) + vector_score;
    total_score.min(100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dense_vector_semantic_matching() {
        let dt = DynamicThesaurus::default();
        let q = parse_bilingual_query_with_thesaurus("用户领取购物券", &dt);

        let code_target = "function claimDiscountCoupon(userId, couponId) { ... }";
        let score = calculate_text_similarity(&q, code_target);
        assert!(score > 20.0, "Score was {score}, expected > 20.0 from vector + subword matching");
    }

    #[test]
    fn test_snake_and_camel_case_cross_matching() {
        let dt = DynamicThesaurus::default();

        // 1. User inputs snake_case: "pay_order"
        let q1 = parse_bilingual_query_with_thesaurus("pay_order", &dt);

        // Target is camelCase in symbol name and comment
        let target_sym = "public Response payOrder(Long orderId)";
        let target_comment = "// Execute PayOrder workflow";

        let score_sym = calculate_text_similarity(&q1, target_sym);
        let score_doc = calculate_text_similarity(&q1, target_comment);

        assert!(score_sym > 50.0, "pay_order -> payOrder score was {score_sym}, expected > 50");
        assert!(score_doc > 40.0, "pay_order -> PayOrder comment score was {score_doc}, expected > 40");

        // 2. User inputs camelCase: "deductStock"
        let q2 = parse_bilingual_query_with_thesaurus("deductStock", &dt);

        // Target is snake_case in Rust/Python: "def batch_deduct_stock(item_id):"
        let target_snake = "def batch_deduct_stock(item_id):";
        let score_snake = calculate_text_similarity(&q2, target_snake);
        assert!(score_snake > 50.0, "deductStock -> batch_deduct_stock score was {score_snake}, expected > 50");
    }
}
