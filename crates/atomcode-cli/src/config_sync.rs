use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
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
        relative_path: "prompts/prompts.md",
        content: include_str!("../../atomcode-coding/assets/prompts/prompts.md"),
        description: "提示词指南 (prompts.md)",
    },
    BundledFileEntry {
        relative_path: "prompts/内置工具.yaml",
        content: include_str!("../../atomcode-coding/assets/prompts/内置工具.yaml"),
        description: "内置工具提示词说明 (内置工具.yaml)",
    },
    BundledFileEntry {
        relative_path: "prompts/内置技能.yaml",
        content: include_str!("../../atomcode-coding/assets/prompts/内置技能.yaml"),
        description: "内置技能提示词说明 (内置技能.yaml)",
    },
    BundledFileEntry {
        relative_path: "config.toml",
        content: "[tools.bash]\ndefault_timeout_secs = 900\nmax_timeout_secs = 1800\nsilent_kill_secs = 900\n",
        description: "系统与工具通用配置 (config.toml - 基础工具段)",
    },
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

/// 智能合并 config.toml：严格保留用户的模型自定义配置、默认选择模型、档位等，只同步 tools/系统通用更改
pub fn merge_user_config_preserving_models(existing: &str, new_template: &str) -> String {
    let mut existing_val: toml::Value = match toml::from_str(existing) {
        Ok(v) => v,
        Err(_) => return existing.to_string(),
    };
    let new_val: toml::Value = match toml::from_str(new_template) {
        Ok(v) => v,
        Err(_) => return existing.to_string(),
    };

    if let (toml::Value::Table(ref mut exist_tab), toml::Value::Table(ref new_tab)) =
        (&mut existing_val, &new_val)
    {
        for (k, v) in new_tab {
            // ⚠️ 严格跳过用户的自定义模型配置和档位偏好
            if k == "models"
                || k == "default_model"
                || k == "model"
                || k == "profiles"
                || k == "providers"
                || k == "provider"
                || k == "reasoning_effort"
            {
                continue;
            }
            if k == "tools" {
                if let (Some(toml::Value::Table(exist_tools)), toml::Value::Table(new_tools)) =
                    (exist_tab.get_mut("tools"), v)
                {
                    for (tool_k, tool_v) in new_tools {
                        exist_tools.insert(tool_k.clone(), tool_v.clone());
                    }
                    continue;
                }
            }
            exist_tab.insert(k.clone(), v.clone());
        }
    }

    toml::to_string_pretty(&existing_val).unwrap_or_else(|_| existing.to_string())
}

/// 扫描 ~/.atomcode 目录下所有涉及的内置非模型配置变更项
pub fn scan_atomcode_config_diffs(atomcode_home: &Path) -> Vec<ConfigDiffItem> {
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
        if let Event::Key(KeyEvent { code, modifiers, .. }) = event::read()? {
            match code {
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
                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
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
