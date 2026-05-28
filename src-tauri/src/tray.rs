//! 托盘菜单管理模块
//!
//! 负责系统托盘图标和菜单的创建、更新和事件处理。

use chrono::TimeZone;
use once_cell::sync::Lazy;
use tauri::menu::{CheckMenuItem, Menu, MenuBuilder, MenuItem, Submenu, SubmenuBuilder};
use tauri::{Emitter, Manager};
use tauri_plugin_opener::OpenerExt;

use crate::app_config::AppType;
use crate::error::AppError;
use crate::services::usage_stats::UsageSummary;
use crate::store::AppState;

/// 每个 app 分区的子菜单句柄，用于 usage 更新时就地改 label 而非整菜单重建。
/// `create_tray_menu` 每次重建都会整表覆盖写入，保证句柄始终指向当前活跃菜单。
static TRAY_SECTION_SUBMENUS: Lazy<
    std::sync::Mutex<std::collections::HashMap<AppType, Submenu<tauri::Wry>>>,
> = Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// 托盘菜单文本（国际化）
#[derive(Clone, Copy)]
pub struct TrayTexts {
    pub show_main: &'static str,
    pub open_website: &'static str,
    pub no_providers_label: &'static str,
    pub usage_requests: &'static str,
    pub usage_input: &'static str,
    pub usage_output: &'static str,
    pub usage_cache_creation: &'static str,
    pub usage_cache_read: &'static str,
    pub lightweight_mode: &'static str,
    pub quit: &'static str,
    pub _auto_label: &'static str,
}

impl TrayTexts {
    pub fn from_language(language: &str) -> Self {
        match language {
            "en" => Self {
                show_main: "Open main window",
                open_website: "Open Official Website",
                no_providers_label: "(no providers)",
                usage_requests: "Requests",
                usage_input: "Input",
                usage_output: "Output",
                usage_cache_creation: "Cache Create",
                usage_cache_read: "Cache Hit",
                lightweight_mode: "Lightweight Mode",
                quit: "Quit",
                _auto_label: "Auto (Failover)",
            },
            "ja" => Self {
                show_main: "メインウィンドウを開く",
                open_website: "公式サイトを開く",
                no_providers_label: "(プロバイダーなし)",
                usage_requests: "リクエスト",
                usage_input: "入力",
                usage_output: "出力",
                usage_cache_creation: "キャッシュ作成",
                usage_cache_read: "キャッシュ命中",
                lightweight_mode: "軽量モード",
                quit: "終了",
                _auto_label: "自動 (フェイルオーバー)",
            },
            "zh-TW" => Self {
                show_main: "開啟主介面",
                open_website: "開啟官方網站",
                no_providers_label: "(無供應商)",
                usage_requests: "請求",
                usage_input: "輸入",
                usage_output: "輸出",
                usage_cache_creation: "快取建立",
                usage_cache_read: "快取命中",
                lightweight_mode: "輕量模式",
                quit: "退出",
                _auto_label: "自動 (故障轉移)",
            },
            _ => Self {
                show_main: "打开主界面",
                open_website: "打开官方网站",
                no_providers_label: "(无供应商)",
                usage_requests: "请求",
                usage_input: "输入",
                usage_output: "输出",
                usage_cache_creation: "缓存创建",
                usage_cache_read: "缓存命中",
                lightweight_mode: "轻量模式",
                quit: "退出",
                _auto_label: "自动 (故障转移)",
            },
        }
    }
}

/// 托盘应用分区配置
pub struct TrayAppSection {
    pub app_type: AppType,
    pub prefix: &'static str,
    pub empty_id: &'static str,
    pub header_label: &'static str,
    pub log_name: &'static str,
}

/// Auto 菜单项后缀
pub const AUTO_SUFFIX: &str = "auto";
pub const TRAY_ID: &str = "cc-switch";

pub const TRAY_SECTIONS: [TrayAppSection; 3] = [
    TrayAppSection {
        app_type: AppType::Claude,
        prefix: "claude_",
        empty_id: "claude_empty",
        header_label: "Claude",
        log_name: "Claude",
    },
    TrayAppSection {
        app_type: AppType::Codex,
        prefix: "codex_",
        empty_id: "codex_empty",
        header_label: "Codex",
        log_name: "Codex",
    },
    TrayAppSection {
        app_type: AppType::Gemini,
        prefix: "gemini_",
        empty_id: "gemini_empty",
        header_label: "Gemini",
        log_name: "Gemini",
    },
];

fn today_usage_range() -> (i64, i64) {
    let now = chrono::Local::now();
    let start = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(
            |midnight| match chrono::Local.from_local_datetime(&midnight) {
                chrono::LocalResult::Single(dt) => Some(dt.timestamp()),
                chrono::LocalResult::Ambiguous(earliest, _) => Some(earliest.timestamp()),
                chrono::LocalResult::None => None,
            },
        )
        .unwrap_or_else(|| now.timestamp());
    (start, now.timestamp())
}

fn collect_today_usage_by_app(
    app_state: &AppState,
) -> Result<std::collections::HashMap<String, UsageSummary>, AppError> {
    let (start, end) = today_usage_range();
    let summaries = app_state
        .db
        .get_usage_summary_by_app(Some(start), Some(end))?;
    Ok(summaries
        .into_iter()
        .map(|item| (item.app_type, item.summary))
        .collect())
}

fn current_provider_name_for_section(
    app_state: &AppState,
    section: &TrayAppSection,
) -> Option<String> {
    let providers = app_state
        .db
        .get_all_providers(section.app_type.as_str())
        .ok()?;
    let current_id =
        crate::settings::get_effective_current_provider(&app_state.db, &section.app_type)
            .ok()
            .flatten()?;
    providers
        .get(&current_id)
        .map(|provider| provider.name.clone())
}

fn format_token_count(tokens: u64) -> String {
    const UNITS: &[(u64, &str)] = &[
        (1, ""),
        (1_000, "K"),
        (1_000_000, "M"),
        (1_000_000_000, "B"),
        (1_000_000_000_000, "T"),
    ];

    let mut idx = UNITS
        .iter()
        .rposition(|(threshold, _)| tokens >= *threshold)
        .unwrap_or(0);

    while idx + 1 < UNITS.len() {
        let value = tokens as f64 / UNITS[idx].0 as f64;
        if value < 999.995 {
            break;
        }
        idx += 1;
    }

    let (threshold, unit) = UNITS[idx];
    if unit.is_empty() {
        tokens.to_string()
    } else {
        format!("{:.2}{unit}", tokens as f64 / threshold as f64)
    }
}

fn format_today_usage_title(
    app_label: &str,
    summary: Option<&UsageSummary>,
    texts: &TrayTexts,
) -> String {
    let zero;
    let summary = match summary {
        Some(summary) => summary,
        None => {
            zero = UsageSummary {
                total_requests: 0,
                total_cost: "0.000000".to_string(),
                total_input_tokens: 0,
                total_output_tokens: 0,
                total_cache_creation_tokens: 0,
                total_cache_read_tokens: 0,
                success_rate: 0.0,
                real_total_tokens: 0,
                cache_hit_rate: 0.0,
            };
            &zero
        }
    };

    format!(
        "{}\n{} {}\n{} {}\n{} {}\n{} {}\n{} {}",
        app_label,
        texts.usage_requests,
        summary.total_requests,
        texts.usage_input,
        format_token_count(summary.total_input_tokens),
        texts.usage_output,
        format_token_count(summary.total_output_tokens),
        texts.usage_cache_creation,
        format_token_count(summary.total_cache_creation_tokens),
        texts.usage_cache_read,
        format_token_count(summary.total_cache_read_tokens)
    )
}

fn format_tray_section_title(
    section: &TrayAppSection,
    today_usage_by_app: &std::collections::HashMap<String, UsageSummary>,
    current_provider_name: Option<&str>,
    texts: &TrayTexts,
) -> String {
    let label;
    let app_label = if let Some(provider_name) = current_provider_name {
        label = format!(
            "{} [{}]",
            section.header_label,
            sanitize_tray_label(provider_name)
        );
        label.as_str()
    } else {
        section.header_label
    };

    format_today_usage_title(
        app_label,
        today_usage_by_app.get(section.app_type.as_str()),
        texts,
    )
}

fn sanitize_tray_label(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { '?' } else { ch })
        .collect()
}

/// 对供应商列表排序：sort_index → created_at → name
fn sort_providers(
    providers: &indexmap::IndexMap<String, crate::provider::Provider>,
) -> Vec<(&String, &crate::provider::Provider)> {
    let mut sorted: Vec<_> = providers.iter().collect();
    sorted.sort_by(|(_, a), (_, b)| {
        match (a.sort_index, b.sort_index) {
            (Some(idx_a), Some(idx_b)) => return idx_a.cmp(&idx_b),
            (Some(_), None) => return std::cmp::Ordering::Less,
            (None, Some(_)) => return std::cmp::Ordering::Greater,
            _ => {}
        }

        match (a.created_at, b.created_at) {
            (Some(time_a), Some(time_b)) => return time_a.cmp(&time_b),
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            _ => {}
        }

        a.name.cmp(&b.name)
    });
    sorted
}

/// 处理供应商托盘事件
pub fn handle_provider_tray_event(app: &tauri::AppHandle, event_id: &str) -> bool {
    for section in TRAY_SECTIONS.iter() {
        if let Some(suffix) = event_id.strip_prefix(section.prefix) {
            // 处理 Auto 点击
            if suffix == AUTO_SUFFIX {
                log::info!("切换到{} Auto模式", section.log_name);
                let app_handle = app.clone();
                let app_type = section.app_type.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    if let Err(e) = handle_auto_click(&app_handle, &app_type) {
                        log::error!("切换{}Auto模式失败: {e}", section.log_name);
                    }
                });
                return true;
            }

            // 处理供应商点击
            log::info!("切换到{}供应商: {suffix}", section.log_name);
            let app_handle = app.clone();
            let provider_id = suffix.to_string();
            let app_type = section.app_type.clone();
            tauri::async_runtime::spawn_blocking(move || {
                if let Err(e) = handle_provider_click(&app_handle, &app_type, &provider_id) {
                    log::error!("切换{}供应商失败: {e}", section.log_name);
                }
            });
            return true;
        }
    }
    false
}

/// 处理 Auto 点击：启用 proxy 和 auto_failover
fn handle_auto_click(app: &tauri::AppHandle, app_type: &AppType) -> Result<(), AppError> {
    if let Some(app_state) = app.try_state::<AppState>() {
        let app_type_str = app_type.as_str();

        // 强一致语义：Auto 模式开启后立即切到队列 P1（P1→P2→...）
        // 若队列为空，则尝试把“当前供应商”自动加入队列作为 P1，避免用户陷入无法开启的死锁。
        let mut queue = app_state.db.get_failover_queue(app_type_str)?;
        if queue.is_empty() {
            let current_id =
                crate::settings::get_effective_current_provider(&app_state.db, app_type)?;
            let Some(current_id) = current_id else {
                return Err(AppError::Message(
                    "故障转移队列为空，且未设置当前供应商，无法启用 Auto 模式".to_string(),
                ));
            };
            app_state
                .db
                .add_to_failover_queue(app_type_str, &current_id)?;
            queue = app_state.db.get_failover_queue(app_type_str)?;
        }

        let p1_provider_id = queue
            .first()
            .map(|item| item.provider_id.clone())
            .ok_or_else(|| AppError::Message("故障转移队列为空，无法启用 Auto 模式".to_string()))?;

        // 真正启用 failover：启动代理服务 + 执行接管 + 开启 auto_failover
        let proxy_service = &app_state.proxy_service;

        // 1) 确保代理服务运行（会自动设置 proxy_enabled = true）
        let is_running = futures::executor::block_on(proxy_service.is_running());
        if !is_running {
            log::info!("[Tray] Auto 模式：启动代理服务");
            if let Err(e) = futures::executor::block_on(proxy_service.start()) {
                log::error!("[Tray] 启动代理服务失败: {e}");
                return Err(AppError::Message(format!("启动代理服务失败: {e}")));
            }
        }

        // 2) 执行 Live 配置接管（确保该 app 被代理接管）
        log::info!("[Tray] Auto 模式：对 {app_type_str} 执行接管");
        if let Err(e) =
            futures::executor::block_on(proxy_service.set_takeover_for_app(app_type_str, true))
        {
            log::error!("[Tray] 执行接管失败: {e}");
            return Err(AppError::Message(format!("执行接管失败: {e}")));
        }

        // 3) 设置 auto_failover_enabled = true
        app_state
            .db
            .set_proxy_flags_sync(app_type_str, true, true)?;

        // 3.1) 立即切到队列 P1（热切换：不写 Live，仅更新 DB/settings/备份）
        if let Err(e) = futures::executor::block_on(
            proxy_service.switch_proxy_target(app_type_str, &p1_provider_id),
        ) {
            log::error!("[Tray] Auto 模式切换到队列 P1 失败: {e}");
            return Err(AppError::Message(format!(
                "Auto 模式切换到队列 P1 失败: {e}"
            )));
        }

        // 4) 更新托盘菜单
        if let Ok(new_menu) = create_tray_menu(app, app_state.inner()) {
            if let Some(tray) = app.tray_by_id(TRAY_ID) {
                let _ = tray.set_menu(Some(new_menu));
            }
        }

        // 5) 发射事件到前端
        let event_data = serde_json::json!({
            "appType": app_type_str,
            "proxyEnabled": true,
            "autoFailoverEnabled": true,
            "providerId": p1_provider_id
        });
        if let Err(e) = app.emit("proxy-flags-changed", event_data.clone()) {
            log::error!("发射 proxy-flags-changed 事件失败: {e}");
        }
        // 发射 provider-switched 事件（保持向后兼容，Auto 切换也算一种切换）
        if let Err(e) = app.emit("provider-switched", event_data) {
            log::error!("发射 provider-switched 事件失败: {e}");
        }
    }
    Ok(())
}

/// 处理供应商点击：关闭 auto_failover + 切换供应商
fn handle_provider_click(
    app: &tauri::AppHandle,
    app_type: &AppType,
    provider_id: &str,
) -> Result<(), AppError> {
    if let Some(app_state) = app.try_state::<AppState>() {
        let app_type_str = app_type.as_str();

        // 获取当前 proxy 状态，保持 enabled 不变，只关闭 auto_failover
        let (proxy_enabled, _) = app_state.db.get_proxy_flags_sync(app_type_str);
        app_state
            .db
            .set_proxy_flags_sync(app_type_str, proxy_enabled, false)?;

        // 切换供应商。需要本地路由的供应商也不在这里自动启动代理，
        // 由用户在页面/设置中手动开启。
        crate::services::ProviderService::switch(app_state.inner(), app_type.clone(), provider_id)?;

        // 更新托盘菜单
        if let Ok(new_menu) = create_tray_menu(app, app_state.inner()) {
            if let Some(tray) = app.tray_by_id(TRAY_ID) {
                let _ = tray.set_menu(Some(new_menu));
            }
        }

        // 发射事件到前端
        let event_data = serde_json::json!({
            "appType": app_type_str,
            "proxyEnabled": proxy_enabled,
            "autoFailoverEnabled": false,
            "providerId": provider_id
        });
        if let Err(e) = app.emit("proxy-flags-changed", event_data.clone()) {
            log::error!("发射 proxy-flags-changed 事件失败: {e}");
        }
        // 发射 provider-switched 事件（保持向后兼容）
        if let Err(e) = app.emit("provider-switched", event_data) {
            log::error!("发射 provider-switched 事件失败: {e}");
        }
    }
    Ok(())
}

/// 创建动态托盘菜单
pub fn create_tray_menu(
    app: &tauri::AppHandle,
    app_state: &AppState,
) -> Result<Menu<tauri::Wry>, AppError> {
    let app_settings = crate::settings::get_settings();
    let tray_texts = TrayTexts::from_language(app_settings.language.as_deref().unwrap_or("zh"));

    // Get visible apps setting, default to all visible
    let visible_apps = app_settings.visible_apps.unwrap_or_default();

    let mut menu_builder = MenuBuilder::new(app);
    let mut section_handles: std::collections::HashMap<AppType, Submenu<tauri::Wry>> =
        std::collections::HashMap::new();
    let today_usage_by_app = collect_today_usage_by_app(app_state).unwrap_or_else(|e| {
        log::warn!("[Tray] 读取今日 Token 使用统计失败: {e}");
        std::collections::HashMap::new()
    });

    // 顶部：打开主界面 / 打开官方网站
    let show_main_item =
        MenuItem::with_id(app, "show_main", tray_texts.show_main, true, None::<&str>)
            .map_err(|e| AppError::Message(format!("创建打开主界面菜单失败: {e}")))?;
    let open_website_item = MenuItem::with_id(
        app,
        "open_website",
        tray_texts.open_website,
        true,
        None::<&str>,
    )
    .map_err(|e| AppError::Message(format!("创建打开官方网站菜单失败: {e}")))?;
    menu_builder = menu_builder
        .item(&show_main_item)
        .item(&open_website_item)
        .separator();

    // Pre-compute proxy running state (used to disable official providers in tray menu)
    let is_proxy_running = futures::executor::block_on(app_state.proxy_service.is_running());

    // 每个应用类型折叠为子菜单，避免供应商过多时菜单过长
    for section in TRAY_SECTIONS.iter() {
        if !visible_apps.is_visible(&section.app_type) {
            continue;
        }

        let app_type_str = section.app_type.as_str();
        let providers = app_state.db.get_all_providers(app_type_str)?;

        let current_id =
            crate::settings::get_effective_current_provider(&app_state.db, &section.app_type)?
                .unwrap_or_default();

        let current_provider_name = providers
            .get(&current_id)
            .map(|provider| provider.name.as_str());
        let submenu_label = format_tray_section_title(
            section,
            &today_usage_by_app,
            current_provider_name,
            &tray_texts,
        );
        let submenu_id = format!("submenu_{}", app_type_str);

        if providers.is_empty() {
            let empty_item = MenuItem::with_id(
                app,
                section.empty_id,
                tray_texts.no_providers_label,
                false,
                None::<&str>,
            )
            .map_err(|e| AppError::Message(format!("创建{}空提示失败: {e}", section.log_name)))?;
            let submenu = SubmenuBuilder::with_id(app, &submenu_id, &submenu_label)
                .item(&empty_item)
                .build()
                .map_err(|e| {
                    AppError::Message(format!("构建{}子菜单失败: {e}", section.log_name))
                })?;
            section_handles.insert(section.app_type.clone(), submenu.clone());
            menu_builder = menu_builder.item(&submenu);
        } else {
            // Check if this app is under proxy takeover (for disabling official providers)
            let is_app_taken_over = is_proxy_running
                && (futures::executor::block_on(app_state.db.get_live_backup(app_type_str))
                    .ok()
                    .flatten()
                    .is_some()
                    || app_state
                        .proxy_service
                        .detect_takeover_in_live_config_for_app(&section.app_type));

            let mut submenu_builder = SubmenuBuilder::with_id(app, &submenu_id, &submenu_label);

            for (id, provider) in sort_providers(&providers) {
                let is_current = current_id == *id;
                let is_official_blocked =
                    is_app_taken_over && provider.category.as_deref() == Some("official");
                let label = if is_official_blocked {
                    format!("{} \u{26D4}", &provider.name) // ⛔ emoji
                } else {
                    provider.name.clone()
                };
                let item = CheckMenuItem::with_id(
                    app,
                    format!("{}{}", section.prefix, id),
                    &label,
                    !is_official_blocked, // disabled when blocked
                    is_current,
                    None::<&str>,
                )
                .map_err(|e| {
                    AppError::Message(format!("创建{}菜单项失败: {e}", section.log_name))
                })?;
                submenu_builder = submenu_builder.item(&item);
            }

            let submenu = submenu_builder.build().map_err(|e| {
                AppError::Message(format!("构建{}子菜单失败: {e}", section.log_name))
            })?;
            section_handles.insert(section.app_type.clone(), submenu.clone());
            menu_builder = menu_builder.item(&submenu);
        }

        menu_builder = menu_builder.separator();
    }

    let lightweight_item = CheckMenuItem::with_id(
        app,
        "lightweight_mode",
        tray_texts.lightweight_mode,
        true,
        crate::lightweight::is_lightweight_mode(),
        None::<&str>,
    )
    .map_err(|e| AppError::Message(format!("创建轻量模式菜单失败: {e}")))?;

    menu_builder = menu_builder.item(&lightweight_item).separator();

    // 退出菜单（分隔符已在上面的 section 循环中添加）
    let quit_item = MenuItem::with_id(app, "quit", tray_texts.quit, true, None::<&str>)
        .map_err(|e| AppError::Message(format!("创建退出菜单失败: {e}")))?;

    menu_builder = menu_builder.item(&quit_item);

    let menu = menu_builder
        .build()
        .map_err(|e| AppError::Message(format!("构建菜单失败: {e}")))?;

    *TRAY_SECTION_SUBMENUS
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = section_handles;

    Ok(menu)
}

/// 就地更新各 app 分区子菜单的标题（usage 后缀变化时走这条），
/// 避免 `set_menu` 导致用户打开中的菜单被关闭。
/// 句柄由上一次 `create_tray_menu` 填充；为空（从未构建过菜单）时无事发生。
fn update_tray_usage_labels(app: &tauri::AppHandle) {
    let Some(app_state) = app.try_state::<AppState>() else {
        return;
    };
    let app_settings = crate::settings::get_settings();
    let tray_texts = TrayTexts::from_language(app_settings.language.as_deref().unwrap_or("zh"));
    let handles = match TRAY_SECTION_SUBMENUS.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let today_usage_by_app = collect_today_usage_by_app(&app_state).unwrap_or_else(|e| {
        log::warn!("[Tray] 读取今日 Token 使用统计失败: {e}");
        std::collections::HashMap::new()
    });

    for section in TRAY_SECTIONS.iter() {
        let Some(submenu) = handles.get(&section.app_type) else {
            continue;
        };
        let current_provider_name = current_provider_name_for_section(&app_state, section);
        let new_label = format_tray_section_title(
            section,
            &today_usage_by_app,
            current_provider_name.as_deref(),
            &tray_texts,
        );
        if let Err(e) = submenu.set_text(&new_label) {
            log::debug!("[Tray] 更新{}子菜单标题失败: {e}", section.log_name);
        }
    }
}

pub fn refresh_tray_menu(app: &tauri::AppHandle) {
    use crate::store::AppState;

    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(new_menu) = create_tray_menu(app, state.inner()) {
            if let Some(tray) = app.tray_by_id(TRAY_ID) {
                if let Err(e) = tray.set_menu(Some(new_menu)) {
                    log::error!("刷新托盘菜单失败: {e}");
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub fn apply_tray_policy(app: &tauri::AppHandle, dock_visible: bool) {
    use tauri::ActivationPolicy;

    let desired_policy = if dock_visible {
        ActivationPolicy::Regular
    } else {
        ActivationPolicy::Accessory
    };

    if let Err(err) = app.set_dock_visibility(dock_visible) {
        log::warn!("设置 Dock 显示状态失败: {err}");
    }

    if let Err(err) = app.set_activation_policy(desired_policy) {
        log::warn!("设置激活策略失败: {err}");
    }
}

/// 处理托盘菜单事件
pub fn handle_tray_menu_event(app: &tauri::AppHandle, event_id: &str) {
    log::info!("处理托盘菜单事件: {event_id}");

    match event_id {
        "show_main" => {
            if let Some(window) = app.get_webview_window("main") {
                #[cfg(target_os = "windows")]
                {
                    let _ = window.set_skip_taskbar(false);
                }
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
                #[cfg(target_os = "linux")]
                {
                    crate::linux_fix::nudge_main_window(window.clone());
                }
                #[cfg(target_os = "macos")]
                {
                    apply_tray_policy(app, true);
                }
            } else if crate::lightweight::is_lightweight_mode() {
                if let Err(e) = crate::lightweight::exit_lightweight_mode(app) {
                    log::error!("退出轻量模式重建窗口失败: {e}");
                }
            }
        }
        "open_website" => {
            if let Err(e) = app.opener().open_url("https://ccswitch.io", None::<String>) {
                log::error!("打开官方网站失败: {e}");
            }
        }
        "lightweight_mode" => {
            if crate::lightweight::is_lightweight_mode() {
                if let Err(e) = crate::lightweight::exit_lightweight_mode(app) {
                    log::error!("退出轻量模式失败: {e}");
                }
            } else if let Err(e) = crate::lightweight::enter_lightweight_mode(app) {
                log::error!("进入轻量模式失败: {e}");
            }
        }
        "quit" => {
            log::info!("退出应用");
            app.exit(0);
        }
        _ => {
            if handle_provider_tray_event(app, event_id) {
                return;
            }
            log::warn!("未处理的菜单事件: {event_id}");
        }
    }
}

static LAST_TRAY_USAGE_REFRESH: std::sync::Mutex<Option<std::time::Instant>> =
    std::sync::Mutex::new(None);
const MIN_TRAY_USAGE_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// 合并多次快速触发的"usage 标题软更新"：批量刷新期间多个 usage 命令
/// 同时成功时，只会产生一次就地 `set_text` 批量调用。走软更新而不是
/// `refresh_tray_menu` 整建，避免用户打开中的菜单被 macOS 系统关闭。
static TRAY_REBUILD_SCHEDULED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub fn schedule_tray_refresh(app: &tauri::AppHandle) {
    use std::sync::atomic::Ordering;
    if TRAY_REBUILD_SCHEDULED.swap(true, Ordering::AcqRel) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        // 50ms 合窗：让同一轮 React Query / 托盘批量刷新触发的多个写入
        // 共享一次标题更新。
        std::thread::sleep(std::time::Duration::from_millis(50));
        TRAY_REBUILD_SCHEDULED.store(false, Ordering::Release);
        update_tray_usage_labels(&app);
    });
}

/// 刷新托盘标题里的今日本地 Token 统计。内部 10 秒节流防止鼠标悬停
/// 反复进出时频繁查询数据库；不触发 provider usage 外部请求。
pub(crate) async fn refresh_all_usage_in_tray(app: &tauri::AppHandle) {
    {
        let mut guard = LAST_TRAY_USAGE_REFRESH
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = std::time::Instant::now();
        if let Some(last) = *guard {
            if now.duration_since(last) < MIN_TRAY_USAGE_REFRESH_INTERVAL {
                return;
            }
        }
        *guard = Some(now);
    }
    update_tray_usage_labels(app);
}

#[cfg(test)]
mod tests {
    use super::{
        format_today_usage_title, format_token_count, format_tray_section_title, TrayTexts,
        TRAY_ID, TRAY_SECTIONS,
    };
    use crate::services::usage_stats::UsageSummary;

    #[test]
    fn tray_id_is_unique_to_app() {
        assert_eq!(TRAY_ID, "cc-switch");
        assert_ne!(TRAY_ID, "main");
    }

    fn usage_summary(
        requests: u64,
        input: u64,
        output: u64,
        cache_creation: u64,
        cache_read: u64,
    ) -> UsageSummary {
        UsageSummary {
            total_requests: requests,
            total_cost: "0.000000".to_string(),
            total_input_tokens: input,
            total_output_tokens: output,
            total_cache_creation_tokens: cache_creation,
            total_cache_read_tokens: cache_read,
            success_rate: 100.0,
            real_total_tokens: input + output + cache_creation + cache_read,
            cache_hit_rate: 0.0,
        }
    }

    #[test]
    fn token_count_units_auto_scale() {
        assert_eq!(format_token_count(0), "0");
        assert_eq!(format_token_count(999), "999");
        assert_eq!(format_token_count(1_000), "1.00K");
        assert_eq!(format_token_count(999_999), "1.00M");
        assert_eq!(format_token_count(1_250_000), "1.25M");
        assert_eq!(format_token_count(999_999_999), "1.00B");
        assert_eq!(format_token_count(3_456_789_000), "3.46B");
        assert_eq!(format_token_count(999_999_999_999), "1.00T");
        assert_eq!(format_token_count(9_876_543_210_000), "9.88T");
    }

    #[test]
    fn today_usage_title_uses_localized_labels() {
        let texts = TrayTexts::from_language("en");
        let title = format_today_usage_title(
            "Codex",
            Some(&usage_summary(2, 1_000, 2_000, 3_000, 4_000)),
            &texts,
        );

        assert_eq!(
            title,
            "Codex\nRequests 2\nInput 1.00K\nOutput 2.00K\nCache Create 3.00K\nCache Hit 4.00K"
        );
    }

    #[test]
    fn today_usage_title_is_app_scoped_multiline_and_auto_scales_units() {
        let title = format_today_usage_title(
            "Claude",
            Some(&usage_summary(12, 1_250_000, 20_000, 1_000, 3_456_789_000)),
            &TrayTexts::from_language("zh"),
        );

        assert_eq!(
            title,
            "Claude\n请求 12\n输入 1.25M\n输出 20.00K\n缓存创建 1.00K\n缓存命中 3.46B"
        );
    }

    #[test]
    fn today_usage_title_renders_zeroes_when_app_has_no_usage() {
        let title = format_today_usage_title("Codex", None, &TrayTexts::from_language("zh"));

        assert_eq!(
            title,
            "Codex\n请求 0\n输入 0\n输出 0\n缓存创建 0\n缓存命中 0"
        );
    }

    #[test]
    fn tray_section_title_uses_app_usage_without_requiring_providers() {
        let mut usage_by_app = std::collections::HashMap::new();
        usage_by_app.insert(
            "claude".to_string(),
            usage_summary(7, 1_000, 2_000, 3_000, 4_000),
        );

        let title = format_tray_section_title(
            &TRAY_SECTIONS[0],
            &usage_by_app,
            None,
            &TrayTexts::from_language("zh"),
        );

        assert_eq!(
            title,
            "Claude\n请求 7\n输入 1.00K\n输出 2.00K\n缓存创建 3.00K\n缓存命中 4.00K"
        );
    }

    #[test]
    fn tray_section_title_includes_current_provider_name() {
        let usage_by_app = std::collections::HashMap::new();

        let title = format_tray_section_title(
            &TRAY_SECTIONS[0],
            &usage_by_app,
            Some("Claude One"),
            &TrayTexts::from_language("zh"),
        );

        assert_eq!(
            title,
            "Claude [Claude One]\n请求 0\n输入 0\n输出 0\n缓存创建 0\n缓存命中 0"
        );
    }
}
