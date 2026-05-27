use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};
use tauri_plugin_store::StoreExt;

use crate::error::AppError;

/// Store 中的键名
const STORE_KEY_APP_CONFIG_DIR: &str = "app_config_dir_override";
const APP_IDENTIFIER: &str = "com.ccswitch.desktop";
const APP_PATHS_STORE_FILE: &str = "app_paths.json";

/// 缓存当前的 app_config_dir 覆盖路径，避免存储 AppHandle
static APP_CONFIG_DIR_OVERRIDE: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();

fn override_cache() -> &'static RwLock<Option<PathBuf>> {
    APP_CONFIG_DIR_OVERRIDE.get_or_init(|| RwLock::new(None))
}

fn update_cached_override(value: Option<PathBuf>) {
    if let Ok(mut guard) = override_cache().write() {
        *guard = value;
    }
}

/// 获取缓存中的 app_config_dir 覆盖路径
pub fn get_app_config_dir_override() -> Option<PathBuf> {
    override_cache().read().ok()?.clone()
}

fn read_override_from_store(app: &tauri::AppHandle) -> Option<PathBuf> {
    let store = match app.store_builder(APP_PATHS_STORE_FILE).build() {
        Ok(store) => store,
        Err(e) => {
            log::warn!("无法创建 Store: {e}");
            return None;
        }
    };

    match store.get(STORE_KEY_APP_CONFIG_DIR) {
        Some(Value::String(path_str)) => resolve_override_path(&path_str),
        Some(_) => {
            log::warn!("Store 中的 {STORE_KEY_APP_CONFIG_DIR} 类型不正确，应为字符串");
            None
        }
        None => None,
    }
}

/// 从 Store 刷新 app_config_dir 覆盖值并更新缓存
pub fn refresh_app_config_dir_override(app: &tauri::AppHandle) -> Option<PathBuf> {
    let value = read_override_from_store(app);
    update_cached_override(value.clone());
    value
}

/// CLI 启动早于 Tauri AppHandle，可直接读取 tauri-plugin-store 在 AppData 下的文件。
pub fn refresh_app_config_dir_override_from_disk_for_cli() -> Option<PathBuf> {
    let store_path = dirs::data_dir()?
        .join(APP_IDENTIFIER)
        .join(APP_PATHS_STORE_FILE);
    let content = fs::read_to_string(&store_path).ok()?;
    let value: Value = serde_json::from_str(&content).ok()?;
    let override_path = value
        .get(STORE_KEY_APP_CONFIG_DIR)
        .and_then(Value::as_str)
        .and_then(resolve_override_path);
    update_cached_override(override_path.clone());
    override_path
}

/// 写入 app_config_dir 到 Tauri Store
pub fn set_app_config_dir_to_store(
    app: &tauri::AppHandle,
    path: Option<&str>,
) -> Result<(), AppError> {
    let store = app
        .store_builder(APP_PATHS_STORE_FILE)
        .build()
        .map_err(|e| AppError::Message(format!("创建 Store 失败: {e}")))?;

    match path {
        Some(p) => {
            let trimmed = p.trim();
            if !trimmed.is_empty() {
                store.set(STORE_KEY_APP_CONFIG_DIR, Value::String(trimmed.to_string()));
                log::info!("已将 app_config_dir 写入 Store: {trimmed}");
            } else {
                store.delete(STORE_KEY_APP_CONFIG_DIR);
                log::info!("已从 Store 中删除 app_config_dir 配置");
            }
        }
        None => {
            store.delete(STORE_KEY_APP_CONFIG_DIR);
            log::info!("已从 Store 中删除 app_config_dir 配置");
        }
    }

    store
        .save()
        .map_err(|e| AppError::Message(format!("保存 Store 失败: {e}")))?;

    refresh_app_config_dir_override(app);
    Ok(())
}

/// 解析路径，支持 ~ 开头的相对路径
fn resolve_path(raw: &str) -> PathBuf {
    if raw == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    } else if let Some(stripped) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    } else if let Some(stripped) = raw.strip_prefix("~\\") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    }

    PathBuf::from(raw)
}

fn resolve_override_path(path_str: &str) -> Option<PathBuf> {
    let path_str = path_str.trim();
    if path_str.is_empty() {
        return None;
    }

    let path = resolve_path(path_str);
    if !path.exists() {
        log::warn!(
            "Store 中配置的 app_config_dir 不存在: {path:?}\n\
             将使用默认路径。"
        );
        return None;
    }

    log::info!("使用 Store 中的 app_config_dir: {path:?}");
    Some(path)
}

/// 从旧的 settings.json 迁移 app_config_dir 到 Store
pub fn migrate_app_config_dir_from_settings(app: &tauri::AppHandle) -> Result<(), AppError> {
    // app_config_dir 已从 settings.json 移除，此函数保留但不再执行迁移
    // 如果用户在旧版本设置过 app_config_dir，需要在 Store 中手动配置
    log::info!("app_config_dir 迁移功能已移除，请在设置中重新配置");

    let _ = refresh_app_config_dir_override(app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    #[test]
    #[serial]
    fn cli_refresh_reads_app_config_override_from_store_file() {
        let data_home = TempDir::new().expect("create data home");
        let override_dir = TempDir::new().expect("create override dir");
        let original_xdg_data_home = std::env::var("XDG_DATA_HOME").ok();
        std::env::set_var("XDG_DATA_HOME", data_home.path());
        update_cached_override(None);

        let store_dir = data_home.path().join(APP_IDENTIFIER);
        fs::create_dir_all(&store_dir).expect("create store dir");
        fs::write(
            store_dir.join(APP_PATHS_STORE_FILE),
            serde_json::json!({ STORE_KEY_APP_CONFIG_DIR: override_dir.path() }).to_string(),
        )
        .expect("write store file");

        let resolved = refresh_app_config_dir_override_from_disk_for_cli();

        assert_eq!(resolved.as_deref(), Some(override_dir.path()));
        assert_eq!(
            get_app_config_dir_override().as_deref(),
            Some(override_dir.path())
        );

        match original_xdg_data_home {
            Some(value) => std::env::set_var("XDG_DATA_HOME", value),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
        update_cached_override(None);
    }
}
