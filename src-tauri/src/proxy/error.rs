use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;

pub(crate) const PROVIDER_REQUEST_FAILED_MESSAGE: &str = "Provider request failed";
pub(crate) const PROXY_REQUEST_FAILED_MESSAGE: &str = "Proxy request failed";

#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("服务器已在运行")]
    AlreadyRunning,

    #[error("服务器未运行")]
    NotRunning,

    #[error("地址绑定失败: {0}")]
    BindFailed(String),

    #[error("停止超时")]
    StopTimeout,

    #[error("停止失败: {0}")]
    StopFailed(String),

    #[error("请求转发失败: {0}")]
    ForwardFailed(String),

    #[error("无可用的Provider")]
    NoAvailableProvider,

    #[error("所有供应商已熔断，无可用渠道")]
    AllProvidersCircuitOpen,

    #[error("未配置供应商")]
    NoProvidersConfigured,

    #[allow(dead_code)]
    #[error("Provider不健康: {0}")]
    ProviderUnhealthy(String),

    #[error("上游错误 (状态码 {status}): {body:?}")]
    UpstreamError { status: u16, body: Option<String> },

    #[error("超过最大重试次数")]
    MaxRetriesExceeded,

    #[error("数据库错误: {0}")]
    DatabaseError(String),

    #[error("配置错误: {0}")]
    ConfigError(String),

    #[allow(dead_code)]
    #[error("格式转换错误: {0}")]
    TransformError(String),

    #[allow(dead_code)]
    #[error("无效的请求: {0}")]
    InvalidRequest(String),

    #[error("超时: {0}")]
    Timeout(String),

    /// 流式响应空闲超时
    #[allow(dead_code)]
    #[error("流式响应空闲超时: {0}秒无数据")]
    StreamIdleTimeout(u64),

    /// 认证错误
    #[error("认证失败: {0}")]
    AuthError(String),

    #[allow(dead_code)]
    #[error("内部错误: {0}")]
    Internal(String),
}

impl ProxyError {
    pub(crate) fn is_provider_request_failure(&self) -> bool {
        matches!(
            self,
            ProxyError::ForwardFailed(_)
                | ProxyError::NoAvailableProvider
                | ProxyError::AllProvidersCircuitOpen
                | ProxyError::NoProvidersConfigured
                | ProxyError::ProviderUnhealthy(_)
                | ProxyError::UpstreamError { .. }
                | ProxyError::MaxRetriesExceeded
                | ProxyError::Timeout(_)
                | ProxyError::StreamIdleTimeout(_)
        )
    }

    pub(crate) fn redacted_client_message(&self) -> &'static str {
        if self.is_provider_request_failure() {
            PROVIDER_REQUEST_FAILED_MESSAGE
        } else {
            PROXY_REQUEST_FAILED_MESSAGE
        }
    }
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        let (status, body) = match &self {
            ProxyError::UpstreamError {
                status: upstream_status,
                body: _,
            } => {
                let http_status =
                    StatusCode::from_u16(*upstream_status).unwrap_or(StatusCode::BAD_GATEWAY);
                let error_body = json!({
                    "error": {
                        "message": PROVIDER_REQUEST_FAILED_MESSAGE,
                        "type": "upstream_error",
                    }
                });
                (http_status, error_body)
            }
            _ => {
                let http_status = match &self {
                    ProxyError::AlreadyRunning => StatusCode::CONFLICT,
                    ProxyError::NotRunning => StatusCode::SERVICE_UNAVAILABLE,
                    ProxyError::BindFailed(_) => StatusCode::INTERNAL_SERVER_ERROR,
                    ProxyError::StopTimeout => StatusCode::INTERNAL_SERVER_ERROR,
                    ProxyError::StopFailed(_) => StatusCode::INTERNAL_SERVER_ERROR,
                    ProxyError::ForwardFailed(_) => StatusCode::BAD_GATEWAY,
                    ProxyError::NoAvailableProvider => StatusCode::SERVICE_UNAVAILABLE,
                    ProxyError::AllProvidersCircuitOpen => StatusCode::SERVICE_UNAVAILABLE,
                    ProxyError::NoProvidersConfigured => StatusCode::SERVICE_UNAVAILABLE,
                    ProxyError::ProviderUnhealthy(_) => StatusCode::SERVICE_UNAVAILABLE,
                    ProxyError::MaxRetriesExceeded => StatusCode::SERVICE_UNAVAILABLE,
                    ProxyError::DatabaseError(_) => StatusCode::INTERNAL_SERVER_ERROR,
                    ProxyError::ConfigError(_) => StatusCode::BAD_REQUEST,
                    ProxyError::TransformError(_) => StatusCode::UNPROCESSABLE_ENTITY,
                    ProxyError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
                    ProxyError::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
                    ProxyError::StreamIdleTimeout(_) => StatusCode::GATEWAY_TIMEOUT,
                    ProxyError::AuthError(_) => StatusCode::UNAUTHORIZED,
                    ProxyError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
                    ProxyError::UpstreamError { .. } => unreachable!(),
                };

                let error_body = json!({
                    "error": {
                        "message": self.redacted_client_message(),
                        "type": "proxy_error",
                    }
                });

                (http_status, error_body)
            }
        };

        (status, Json(body)).into_response()
    }
}

/// 错误分类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    /// 可重试错误（网络问题、5xx）
    Retryable, // 网络超时、5xx 错误
    /// 不可重试错误（4xx、认证失败）
    NonRetryable, // 认证失败、参数错误、4xx 错误
    #[allow(dead_code)]
    ClientAbort, // 客户端主动中断
}

/// 判断错误是否可重试
#[allow(dead_code)]
pub fn categorize_error(error: &reqwest::Error) -> ErrorCategory {
    if error.is_timeout() || error.is_connect() {
        return ErrorCategory::Retryable;
    }

    if let Some(status) = error.status() {
        if status.is_server_error() {
            ErrorCategory::Retryable
        } else if status.is_client_error() {
            ErrorCategory::NonRetryable
        } else {
            ErrorCategory::Retryable
        }
    } else {
        ErrorCategory::Retryable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use http_body_util::BodyExt;
    use serde_json::Value;

    async fn proxy_error_response_json(error: ProxyError) -> (StatusCode, Value) {
        let response = error.into_response();
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body should collect")
            .to_bytes();
        let body = serde_json::from_slice::<Value>(&body).expect("response body should be JSON");
        (status, body)
    }

    #[tokio::test]
    async fn upstream_error_response_preserves_status_and_redacts_provider_body() {
        let (status, body) = proxy_error_response_json(ProxyError::UpstreamError {
            status: 429,
            body: Some(
                r#"{"error":{"message":"quota exhausted for sk-live-secret","code":"rate_limit"}}"#
                    .to_string(),
            ),
        })
        .await;

        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["error"]["message"], "Provider request failed");
        assert_eq!(body["error"]["type"], "upstream_error");

        let serialized = body.to_string();
        assert!(!serialized.contains("quota exhausted"));
        assert!(!serialized.contains("sk-live-secret"));
        assert!(!serialized.contains("rate_limit"));
    }

    #[tokio::test]
    async fn provider_forward_failure_response_preserves_status_and_redacts_internal_message() {
        let (status, body) = proxy_error_response_json(ProxyError::ForwardFailed(
            "连接失败: dns lookup failed for api.provider.example".to_string(),
        ))
        .await;

        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["error"]["message"], "Provider request failed");
        assert_eq!(body["error"]["type"], "proxy_error");

        let serialized = body.to_string();
        assert!(!serialized.contains("dns lookup failed"));
        assert!(!serialized.contains("api.provider.example"));
    }
}
