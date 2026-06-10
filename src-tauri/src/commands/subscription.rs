use std::str::FromStr;
use tauri::Emitter;

use crate::app_config::AppType;
use crate::services::subscription::{CredentialStatus, SubscriptionQuota};

/// 查询官方订阅额度
///
/// 读取 CLI 工具已有的 OAuth 凭据并调用官方 API 获取使用额度。
/// 结果（无论业务失败还是 transport 层 Err）都会 emit `usage-cache-updated`，
/// 让前端 React Query 共享同一份最新数据。
/// Err 原样向前端返回，React Query 的 onError 不会被吞掉。
#[tauri::command]
pub async fn get_subscription_quota(
    app: tauri::AppHandle,
    tool: String,
) -> Result<SubscriptionQuota, String> {
    let inner = crate::services::subscription::get_subscription_quota(&tool).await;
    let snapshot = match &inner {
        Ok(q) => q.clone(),
        // transport 层 Err —— 凭据状态不明，用 Valid 表达"凭据没问题，是通信/parse 出错"。
        Err(err_msg) => SubscriptionQuota::error(&tool, CredentialStatus::Valid, err_msg.clone()),
    };
    if let Ok(app_type) = AppType::from_str(&tool) {
        let payload = serde_json::json!({
            "kind": "subscription",
            "appType": app_type.as_str(),
            "data": &snapshot,
        });
        if let Err(e) = app.emit("usage-cache-updated", payload) {
            log::error!("emit usage-cache-updated (subscription) 失败: {e}");
        }
    }
    inner
}
