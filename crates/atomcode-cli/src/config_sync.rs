use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

/// 内置全量默认模板条目定义
pub struct BundledFileEntry {
    pub relative_path: &'static str,
    pub content: &'static str,
    pub description: &'static str,
}

/// 全量内置资产列表：包含 ~/.atomcode 下的所有默认配置文件与提示词目录
pub const BUNDLED_ASSETS: &[BundledFileEntry] = &[
    BundledFileEntry {
        relative_path: "prompts/init.yaml",
        content: include_str!("../../atomcode-coding/assets/prompts/init.yaml"),
        description: "提示词与身份定义 (init.yaml)",
    },
    BundledFileEntry {
        relative_path: "prompts/rules.yaml",
        content: include_str!("../../atomcode-coding/assets/prompts/rules.yaml"),
        description: "工作流与执行规范 (rules.yaml)",
    },
    BundledFileEntry {
        relative_path: "prompts/root_docs_prompts.md",
        content: include_str!("../../atomcode-coding/assets/prompts/root_docs_prompts.md"),
        description: "提示词指南（不加载进模型） (root_docs_prompts.md)",
    },
    BundledFileEntry {
        relative_path: "prompts/root_docs_内置工具.yaml",
        content: include_str!("../../atomcode-coding/assets/prompts/root_docs_内置工具.yaml"),
        description: "内置工具说明文档（不加载进模型） (root_docs_内置工具.yaml)",
    },
    BundledFileEntry {
        relative_path: "prompts/root_docs_内置技能.yaml",
        content: include_str!("../../atomcode-coding/assets/prompts/root_docs_内置技能.yaml"),
        description: "内置技能说明文档（不加载进模型） (root_docs_内置技能.yaml)",
    },
    BundledFileEntry {
        relative_path: "config.toml",
        content: include_str!("../assets/default-config.toml"),
        description: "系统与工具通用配置 (config.toml — 保留用户模型/账号)",
    },
    BundledFileEntry {
        relative_path: "config_teachs.md",
        content: include_str!("../assets/config_teachs.md"),
        description: "Agent 友好配置文件教程与配置指南 (config_teachs.md)",
    },
    BundledFileEntry {
        relative_path: "builtin-tools.txt",
        content: include_str!("../../atomcode-capabilities/assets/builtin-tools.txt"),
        description: "内置工具清单 (builtin-tools.txt)",
    },
    BundledFileEntry {
        relative_path: "mcp.json",
        content: include_str!("../../atomcode-capabilities/assets/mcp.json"),
        description: "默认 MCP 服务器 (mcp.json)",
    },
    BundledFileEntry {
        relative_path: "user-wrap.md",
        content: include_str!("../../atomcode-capabilities/assets/user-wrap.md"),
        description: "用户提问包装模板 (user-wrap.md — 支持 {{input}} 动态占位符)",
    },
    BundledFileEntry {
        relative_path: ".codegraphignore",
        content: include_str!("../../atomcode-capabilities/assets/.codegraphignore"),
        description: "代码图谱忽略规则 (.codegraphignore)",
    },
    BundledFileEntry {
        relative_path: "thesaurus/admin_system.txt",
        content: include_str!("../../atomcode-capabilities/assets/thesaurus/admin_system.txt"),
        description: "词林 admin_system.txt",
    },
    BundledFileEntry {
        relative_path: "thesaurus/agent_core.txt",
        content: include_str!("../../atomcode-capabilities/assets/thesaurus/agent_core.txt"),
        description: "词林 agent_core.txt",
    },
    BundledFileEntry {
        relative_path: "thesaurus/ai_agent.txt",
        content: include_str!("../../atomcode-capabilities/assets/thesaurus/ai_agent.txt"),
        description: "词林 ai_agent.txt",
    },
    BundledFileEntry {
        relative_path: "thesaurus/computer_science.txt",
        content: include_str!("../../atomcode-capabilities/assets/thesaurus/computer_science.txt"),
        description: "词林 computer_science.txt",
    },
    BundledFileEntry {
        relative_path: "thesaurus/ailaierp.txt",
        content: include_str!("../../atomcode-capabilities/assets/thesaurus/ailaierp.txt"),
        description: "词林 ailaierp.txt (Ailai ERP与电商)",
    },
    BundledFileEntry {
        relative_path: "thesaurus/fullstack_dev.txt",
        content: include_str!("../../atomcode-capabilities/assets/thesaurus/fullstack_dev.txt"),
        description: "词林 fullstack_dev.txt",
    },
    BundledFileEntry {
        relative_path: "thesaurus/medical.txt",
        content: include_str!("../../atomcode-capabilities/assets/thesaurus/medical.txt"),
        description: "词林 medical.txt",
    },
    BundledFileEntry {
        relative_path: "thesaurus/robotics.txt",
        content: include_str!("../../atomcode-capabilities/assets/thesaurus/robotics.txt"),
        description: "词林 robotics.txt",
    },
    BundledFileEntry {
        relative_path: "thesaurus/web_http.txt",
        content: include_str!("../../atomcode-capabilities/assets/thesaurus/web_http.txt"),
        description: "词林 web_http.txt",
    },
    BundledFileEntry {
        relative_path: "teaches/00_overview_index.md",
        content: include_str!("../../atomcode-capabilities/assets/teaches/00_overview_index.md"),
        description: "配置知识库索引 (teaches/00_overview_index.md)",
    },
    BundledFileEntry {
        relative_path: "teaches/01_prompts_and_context.md",
        content: include_str!("../../atomcode-capabilities/assets/teaches/01_prompts_and_context.md"),
        description: "提示词与上下文指南 (teaches/01_prompts_and_context.md)",
    },
    BundledFileEntry {
        relative_path: "teaches/02_models_and_providers.md",
        content: include_str!("../../atomcode-capabilities/assets/teaches/02_models_and_providers.md"),
        description: "模型与提供商指南 (teaches/02_models_and_providers.md)",
    },
    BundledFileEntry {
        relative_path: "teaches/03_mcp_and_skills.md",
        content: include_str!("../../atomcode-capabilities/assets/teaches/03_mcp_and_skills.md"),
        description: "MCP与Skills指南 (teaches/03_mcp_and_skills.md)",
    },
    BundledFileEntry {
        relative_path: "teaches/04_thesaurus_and_retrieval.md",
        content: include_str!("../../atomcode-capabilities/assets/teaches/04_thesaurus_and_retrieval.md"),
        description: "词林检索相关性指南 (teaches/04_thesaurus_and_retrieval.md)",
    },
    BundledFileEntry {
        relative_path: "teaches/05_tools_and_timeouts.md",
        content: include_str!("../../atomcode-capabilities/assets/teaches/05_tools_and_timeouts.md"),
        description: "工具超时与策略指南 (teaches/05_tools_and_timeouts.md)",
    },
    BundledFileEntry {
        relative_path: "teaches/06_directories_and_system.md",
        content: include_str!("../../atomcode-capabilities/assets/teaches/06_directories_and_system.md"),
        description: "系统目录与文件全景指南 (teaches/06_directories_and_system.md)",
    },
    BundledFileEntry {
        relative_path: "teaches/07_project_constraints_and_rules.md",
        content: include_str!("../../atomcode-capabilities/assets/teaches/07_project_constraints_and_rules.md"),
        description: "项目约束与业务知识包指南 (teaches/07_project_constraints_and_rules.md)",
    },
];

/// Old prompt seed names that must be removed after the root_docs_ rename.
const STALE_HOME_FILES: &[&str] = &[
    "prompts/prompts.md",
    "prompts/内置工具.yaml",
    "prompts/内置技能.yaml",
];

#[derive(Clone, Debug)]
pub struct ConfigDiffItem {
    pub relative_path: String,
    pub description: String,
    pub target_path: PathBuf,
    pub new_content: String,
    pub is_new_file: bool,
    pub selected: bool,
}

/// Keys that stay on upgrade: the user's model/account/provider customisation.
const PRESERVE_CONFIG_KEYS: &[&str] = &[
    "models",
    "default_model",
    "default_provider",
    "providers",
    "provider_accounts",
    "provider",
    "model",
    "profiles",
    "reasoning_effort",
    "evaluator_provider",
];

/// New-install defaults for everything except the user's model tables.
pub fn merge_user_config_preserving_models(existing: &str, new_template: &str) -> String {
    let existing_val: toml::Value = match toml::from_str(existing) {
        Ok(v) => v,
        Err(_) => return existing.to_string(),
    };
    let mut new_val: toml::Value = match toml::from_str(new_template) {
        Ok(v) => v,
        Err(_) => return existing.to_string(),
    };

    if let (toml::Value::Table(exist_tab), toml::Value::Table(ref mut new_tab)) =
        (&existing_val, &mut new_val)
    {
        for k in PRESERVE_CONFIG_KEYS {
            if let Some(v) = exist_tab.get(*k) {
                new_tab.insert((*k).to_string(), v.clone());
            }
        }
    }

    toml::to_string_pretty(&new_val).unwrap_or_else(|_| existing.to_string())
}

/// 扫描 ~/.atomcode 目录下所有涉及的内置非模型配置变更项
pub fn scan_atomcode_config_diffs(atomcode_home: &Path) -> Vec<ConfigDiffItem> {
    remove_stale_home_files(atomcode_home);
    let mut diffs = Vec::new();

    for entry in BUNDLED_ASSETS {
        let target = atomcode_home.join(entry.relative_path);
        let bundled_content = entry.content;

        if !target.exists() {
            diffs.push(ConfigDiffItem {
                relative_path: entry.relative_path.to_string(),
                description: entry.description.to_string(),
                target_path: target,
                new_content: bundled_content.to_string(),
                is_new_file: true,
                selected: true, // 默认全选
            });
            continue;
        }

        let existing_content = fs::read_to_string(&target).unwrap_or_default();

        if entry.relative_path == "config.toml" {
            let merged = merge_user_config_preserving_models(&existing_content, bundled_content);
            if merged.trim() != existing_content.trim() {
                diffs.push(ConfigDiffItem {
                    relative_path: entry.relative_path.to_string(),
                    description: entry.description.to_string(),
                    target_path: target,
                    new_content: merged,
                    is_new_file: false,
                    selected: true, // 默认全选
                });
            }
        } else {
            // 对 prompts/*.yaml 等文件进行全文比对
            if existing_content.trim() != bundled_content.trim() {
                diffs.push(ConfigDiffItem {
                    relative_path: entry.relative_path.to_string(),
                    description: entry.description.to_string(),
                    target_path: target,
                    new_content: bundled_content.to_string(),
                    is_new_file: false,
                    selected: true, // 默认全选
                });
            }
        }
    }

    diffs
}

fn remove_stale_home_files(atomcode_home: &Path) {
    for rel in STALE_HOME_FILES {
        let p = atomcode_home.join(rel);
        if p.exists() {
            let _ = fs::remove_file(&p);
        }
    }
}

/// 交互式多选列表渲染与交互引擎：
/// - 空格键：勾选/取消当前项
/// - 'a' 键：全选 / 全取消
/// - 上下方向键 / j / k：移动光标
/// - 回车键 (Enter)：确认并应用所有选中更新
/// - ESC / 'q'：跳过所有配置文件更新
pub fn prompt_interactive_config_sync(mut items: Vec<ConfigDiffItem>) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }

    println!("\n🔍 检测到默认配置（已自动保护用户模型、默认选择模型与档位等自定义项）发生更改：");
    println!("   覆盖文件或目录项如下（使用 ↑/↓ 导航，[空格] 选择/取消，[a] 全选，[Enter] 确认更新，[ESC] 跳过）：\n");

    let mut cursor = 0;
    enable_raw_mode()?;
    let mut stdout = io::stdout();

    // 核心修复 1: 清空进入交互模式前控制台缓冲区残留的按键事件（如输入 upgrade 命令时的回车残余）
    while event::poll(std::time::Duration::from_millis(20)).unwrap_or(false) {
        let _ = event::read();
    }

    let render = |stdout: &mut io::Stdout, items: &[ConfigDiffItem], cursor: usize| -> Result<()> {
        crossterm::execute!(stdout, crossterm::cursor::Hide)?;
        // 清除行并重绘选项
        for (i, item) in items.iter().enumerate() {
            let pointer = if i == cursor { "👉 " } else { "   " };
            let checkbox = if item.selected { "[✔] " } else { "[ ] " };
            let status = if item.is_new_file { " (新增文件)" } else { " (有更新)" };
            println!("\r{}{}{}{}\x1b[K", pointer, checkbox, item.description, status);
        }
        stdout.flush()?;
        Ok(())
    };

    let clear_lines = |stdout: &mut io::Stdout, count: usize| -> Result<()> {
        for _ in 0..count {
            crossterm::execute!(
                stdout,
                crossterm::cursor::MoveUp(1),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::CurrentLine)
            )?;
        }
        Ok(())
    };

    render(&mut stdout, &items, cursor)?;

    let mut confirmed = false;

    loop {
        if let Event::Key(key_event) = event::read()? {
            // 核心修复 2: 过滤 Windows 控制台产生的 Release 事件，防止启动命令时的回车释放事件瞬间触发确认
            if key_event.kind == crossterm::event::KeyEventKind::Release {
                continue;
            }
            match key_event.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    if cursor > 0 {
                        cursor -= 1;
                        clear_lines(&mut stdout, items.len())?;
                        render(&mut stdout, &items, cursor)?;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if cursor + 1 < items.len() {
                        cursor += 1;
                        clear_lines(&mut stdout, items.len())?;
                        render(&mut stdout, &items, cursor)?;
                    }
                }
                KeyCode::Char(' ') => {
                    items[cursor].selected = !items[cursor].selected;
                    clear_lines(&mut stdout, items.len())?;
                    render(&mut stdout, &items, cursor)?;
                }
                KeyCode::Char('a') | KeyCode::Char('A') => {
                    let any_unselected = items.iter().any(|it| !it.selected);
                    for it in &mut items {
                        it.selected = any_unselected;
                    }
                    clear_lines(&mut stdout, items.len())?;
                    render(&mut stdout, &items, cursor)?;
                }
                KeyCode::Enter => {
                    confirmed = true;
                    break;
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    confirmed = false;
                    break;
                }
                KeyCode::Char('c') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                    confirmed = false;
                    break;
                }
                _ => {}
            }
        }
    }

    disable_raw_mode()?;
    crossterm::execute!(stdout, crossterm::cursor::Show)?;
    println!();

    if !confirmed {
        println!("⏩ 已按 [ESC] 跳过默认配置文件更新，保持当前本地配置不变。");
        return Ok(());
    }

    let mut applied_count = 0;
    for item in items.into_iter().filter(|it| it.selected) {
        if let Some(parent) = item.target_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if fs::write(&item.target_path, &item.new_content).is_ok() {
            println!("  ✔ 已更新: {}", item.relative_path);
            applied_count += 1;
        }
    }

    if applied_count > 0 {
        println!("✨ 成功同步了 {} 个配置文件！", applied_count);
    } else {
        println!("ℹ️ 未选择任何更新项，配置保持不变。");
    }

    Ok(())
}

/// 非交互式直接写入/更新全量内置资产（供 `atomcode setup --defaults` / `atomcode setup -y` 及自动化脚本使用）
pub fn apply_all_bundled_assets(atomcode_home: &Path, force: bool) -> Result<usize> {
    remove_stale_home_files(atomcode_home);
    let mut applied_count = 0;

    for entry in BUNDLED_ASSETS {
        let target = atomcode_home.join(entry.relative_path);
        if let Some(parent) = target.parent() {
            let _ = fs::create_dir_all(parent);
        }

        if !target.exists() {
            fs::write(&target, entry.content)?;
            println!("  ✔ [新增] {}", entry.relative_path);
            applied_count += 1;
        } else if entry.relative_path == "config.toml" {
            let existing_content = fs::read_to_string(&target).unwrap_or_default();
            let merged = merge_user_config_preserving_models(&existing_content, entry.content);
            if merged.trim() != existing_content.trim() || force {
                fs::write(&target, &merged)?;
                println!("  ✔ [更新] {} (已保留用户模型/账号配置)", entry.relative_path);
                applied_count += 1;
            }
        } else if force {
            fs::write(&target, entry.content)?;
            println!("  ✔ [覆盖] {}", entry.relative_path);
            applied_count += 1;
        } else {
            let existing_content = fs::read_to_string(&target).unwrap_or_default();
            if existing_content.trim() != entry.content.trim() {
                fs::write(&target, entry.content)?;
                println!("  ✔ [同步] {}", entry.relative_path);
                applied_count += 1;
            }
        }
    }

    Ok(applied_count)
}
