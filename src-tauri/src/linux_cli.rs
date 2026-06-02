use std::io::{BufRead, BufReader, Write};
use std::str::FromStr;

use crate::app_config::AppType;
use crate::database::Database;
use crate::provider::Provider;
use crate::services::{ProviderService, SwitchResult};
use crate::store::AppState;

const CLI_IPC_SOCKET_FILE: &str = "cc-switch-cli.sock";
const CLI_IPC_DIR: &str = "cc-switch";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliCommand {
    List {
        app: Option<AppType>,
        json: bool,
    },
    Switch {
        app: AppType,
        provider_id: String,
        json: bool,
    },
    UpdateKey {
        app: AppType,
        provider_id: String,
        key: String,
        json: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliParseOutcome {
    Command(CliCommand),
    NotCli,
    Help,
    Error(CliError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError {
    pub code: &'static str,
    pub message: String,
    pub exit_code: i32,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CliIpcRequest {
    args: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CliIpcResponse {
    handled: bool,
    stdout: String,
    stderr: String,
    exit_code: i32,
}

pub fn parse_cli_args<I>(args: I) -> CliParseOutcome
where
    I: IntoIterator<Item = String>,
{
    let args: Vec<String> = args.into_iter().collect();

    if args.len() <= 1 {
        return CliParseOutcome::NotCli;
    }

    let command = &args[1];
    if command.starts_with("ccswitch://") {
        return CliParseOutcome::NotCli;
    }

    match command.as_str() {
        "--help" | "-h" | "help" => CliParseOutcome::Help,
        "list" => parse_list_args(&args[2..]),
        "switch" => parse_switch_args(&args[2..]),
        "update-key" => parse_update_key_args(&args[2..]),
        unknown if !unknown.starts_with('-') => CliParseOutcome::Error(CliError {
            code: "unknown_command",
            message: format!("Unknown command: {unknown}"),
            exit_code: 2,
            json: args[2..].iter().any(|a| a == "--json"),
        }),
        _ => {
            let help = help_text();
            usage_error(help.trim_end(), args[2..].iter().any(|a| a == "--json"))
        }
    }
}

fn parse_update_key_args(args: &[String]) -> CliParseOutcome {
    let mut json = false;
    let mut positional = Vec::new();

    for arg in args {
        if arg == "--json" {
            json = true;
        } else if arg.starts_with('-') {
            return usage_error(
                "Usage: cc-switch update-key <app> <provider-id> <key> [--json]",
                json,
            );
        } else {
            positional.push(arg);
        }
    }

    if positional.len() != 3 {
        return usage_error(
            "Usage: cc-switch update-key <app> <provider-id> <key> [--json]",
            json,
        );
    }

    let app = match AppType::from_str(positional[0]) {
        Ok(parsed) => parsed,
        Err(err) => return unsupported_app_error(err.to_string(), json),
    };

    CliParseOutcome::Command(CliCommand::UpdateKey {
        app,
        provider_id: positional[1].clone(),
        key: positional[2].clone(),
        json,
    })
}

fn parse_list_args(args: &[String]) -> CliParseOutcome {
    let mut json = false;
    let mut app = None;

    for arg in args {
        if arg == "--json" {
            json = true;
        } else if arg.starts_with('-') || app.is_some() {
            return usage_error("Usage: cc-switch list [app] [--json]", json);
        } else {
            match AppType::from_str(arg) {
                Ok(parsed) => app = Some(parsed),
                Err(err) => return unsupported_app_error(err.to_string(), json),
            }
        }
    }

    CliParseOutcome::Command(CliCommand::List { app, json })
}

fn parse_switch_args(args: &[String]) -> CliParseOutcome {
    let mut json = false;
    let mut positional = Vec::new();

    for arg in args {
        if arg == "--json" {
            json = true;
        } else if arg.starts_with('-') {
            return usage_error("Usage: cc-switch switch <app> <provider-id> [--json]", json);
        } else {
            positional.push(arg);
        }
    }

    if positional.len() != 2 {
        return usage_error("Usage: cc-switch switch <app> <provider-id> [--json]", json);
    }

    let app = match AppType::from_str(positional[0]) {
        Ok(parsed) => parsed,
        Err(err) => return unsupported_app_error(err.to_string(), json),
    };

    CliParseOutcome::Command(CliCommand::Switch {
        app,
        provider_id: positional[1].clone(),
        json,
    })
}

fn unsupported_app_error(message: String, json: bool) -> CliParseOutcome {
    CliParseOutcome::Error(CliError {
        code: "unsupported_app",
        message,
        exit_code: 2,
        json,
    })
}

fn usage_error(message: &str, json: bool) -> CliParseOutcome {
    CliParseOutcome::Error(CliError {
        code: "usage",
        message: message.to_string(),
        exit_code: 2,
        json,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliProviderRecord {
    pub app: String,
    pub id: String,
    pub name: String,
    pub current: bool,
}

pub fn collect_provider_records(
    db: &Database,
    app: Option<AppType>,
) -> Result<Vec<CliProviderRecord>, crate::error::AppError> {
    let apps: Vec<AppType> = match app {
        Some(app) => vec![app],
        None => AppType::all().collect(),
    };

    let mut records = Vec::new();
    for app in apps {
        let providers = db.get_all_providers(app.as_str())?;
        let local_current_provider_id = crate::settings::get_current_provider(&app);
        let current_provider_id = match local_current_provider_id {
            Some(local_id) if providers.contains_key(&local_id) => Some(local_id),
            _ => db.get_current_provider(app.as_str())?,
        };
        records.extend(
            providers
                .into_iter()
                .map(|(id, provider)| CliProviderRecord {
                    app: app.as_str().to_string(),
                    current: current_provider_id.as_deref() == Some(id.as_str()),
                    id,
                    name: provider.name,
                }),
        );
    }

    Ok(records)
}

pub fn format_list_text(records: &[CliProviderRecord]) -> String {
    let mut output = String::new();
    let mut current_app: Option<&str> = None;

    for record in records {
        if current_app != Some(record.app.as_str()) {
            if current_app.is_some() {
                output.push('\n');
            }
            current_app = Some(record.app.as_str());
            output.push_str(&format!("{}\n", sanitize_text_output(&record.app)));
        }

        let marker = if record.current { '*' } else { ' ' };
        output.push_str(&format!(
            "{marker} {}  {}\n",
            sanitize_text_output(&record.id),
            sanitize_text_output(&record.name)
        ));
    }

    output
}

pub fn format_list_json(records: &[CliProviderRecord]) -> Result<String, crate::error::AppError> {
    serde_json::to_string(records)
        .map_err(|source| crate::error::AppError::JsonSerialize { source })
}

pub fn format_switch_text(app: &AppType, provider_id: &str) -> String {
    format!(
        "Switched {} to {}\n",
        sanitize_text_output(app.as_str()),
        sanitize_text_output(provider_id)
    )
}

pub fn format_switch_json(
    app: &AppType,
    provider_id: &str,
    warnings: &[String],
) -> Result<String, crate::error::AppError> {
    serde_json::to_string(&serde_json::json!({
        "ok": true,
        "app": app.as_str(),
        "providerId": provider_id,
        "warnings": warnings,
    }))
    .map_err(|source| crate::error::AppError::JsonSerialize { source })
}

pub fn format_update_key_text(app: &AppType, provider_id: &str) -> String {
    format!(
        "Updated API key for {} provider {}\n",
        sanitize_text_output(app.as_str()),
        sanitize_text_output(provider_id)
    )
}

pub fn format_update_key_json(
    app: &AppType,
    provider_id: &str,
) -> Result<String, crate::error::AppError> {
    serde_json::to_string(&serde_json::json!({
        "ok": true,
        "app": app.as_str(),
        "providerId": provider_id,
    }))
    .map_err(|source| crate::error::AppError::JsonSerialize { source })
}

pub fn execute_switch(
    state: &AppState,
    app: AppType,
    provider_id: &str,
) -> Result<SwitchResult, CliError> {
    let providers = state
        .db
        .get_all_providers(app.as_str())
        .map_err(|err| CliError {
            code: "switch_failed",
            message: err.to_string(),
            exit_code: 1,
            json: false,
        })?;

    if !providers.contains_key(provider_id) {
        return Err(CliError {
            code: "provider_not_found",
            message: format!("Provider not found: {provider_id} for {}", app.as_str()),
            exit_code: 1,
            json: false,
        });
    }

    ProviderService::switch(state, app, provider_id).map_err(|err| CliError {
        code: "switch_failed",
        message: err.to_string(),
        exit_code: 1,
        json: false,
    })
}

pub fn execute_update_key(
    state: &AppState,
    app: AppType,
    provider_id: &str,
    key: &str,
) -> Result<(), CliError> {
    let key = key.trim();
    if key.is_empty() {
        return Err(CliError {
            code: "empty_api_key",
            message: "API key cannot be empty".to_string(),
            exit_code: 2,
            json: false,
        });
    }

    let app_key = app.as_str().to_string();

    let mut provider = state
        .db
        .get_provider_by_id(provider_id, &app_key)
        .map_err(|err| CliError {
            code: "update_key_failed",
            message: err.to_string(),
            exit_code: 1,
            json: false,
        })?
        .ok_or_else(|| CliError {
            code: "provider_not_found",
            message: format!("Provider not found: {provider_id} for {}", app.as_str()),
            exit_code: 1,
            json: false,
        })?;
    let original_provider = provider.clone();

    if provider.uses_managed_account_auth() || codex_provider_uses_managed_auth(&app, &provider) {
        return Err(CliError {
            code: "unsupported_provider_auth",
            message: format!(
                "Provider {} for {} uses managed account authentication and cannot be updated with update-key",
                provider_id,
                app.as_str()
            ),
            exit_code: 2,
            json: false,
        });
    }

    let api_key_field = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.api_key_field.as_deref());
    set_provider_api_key(&mut provider.settings_config, &app, key, api_key_field);
    if let Err(err) = ProviderService::update(state, app.clone(), Some(provider_id), provider) {
        let message = rollback_update_key_after_failure(state, &app, &app_key, &original_provider)
            .map_or_else(
                |rollback_err| {
                    format!(
                        "{}; additionally failed to roll back provider state: {}",
                        err, rollback_err
                    )
                },
                |()| err.to_string(),
            );
        return Err(CliError {
            code: "update_key_failed",
            message,
            exit_code: 1,
            json: false,
        });
    }
    Ok(())
}

fn rollback_update_key_after_failure(
    state: &AppState,
    app: &AppType,
    app_key: &str,
    original_provider: &Provider,
) -> Result<(), crate::error::AppError> {
    state.db.save_provider(app_key, original_provider)?;

    let effective_current = crate::settings::get_effective_current_provider(&state.db, app)?;
    let is_current = effective_current.as_deref() == Some(original_provider.id.as_str());
    if !is_current {
        return Ok(());
    }

    if app.is_additive_mode() {
        rollback_additive_live_config(state, app, original_provider)
    } else {
        ProviderService::sync_current_provider_for_app(state, app.clone())
    }
}

fn rollback_additive_live_config(
    state: &AppState,
    app: &AppType,
    original_provider: &Provider,
) -> Result<(), crate::error::AppError> {
    match app {
        AppType::OpenCode => match original_provider.category.as_deref() {
            Some("omo") => {
                if state.db.is_omo_provider_current(
                    app.as_str(),
                    &original_provider.id,
                    crate::services::omo::STANDARD.category,
                )? {
                    crate::services::OmoService::write_provider_config_to_file(
                        original_provider,
                        &crate::services::omo::STANDARD,
                    )?;
                }
                Ok(())
            }
            Some("omo-slim") => {
                if state.db.is_omo_provider_current(
                    app.as_str(),
                    &original_provider.id,
                    crate::services::omo::SLIM.category,
                )? {
                    crate::services::OmoService::write_provider_config_to_file(
                        original_provider,
                        &crate::services::omo::SLIM,
                    )?;
                }
                Ok(())
            }
            _ => crate::opencode_config::set_provider(
                &original_provider.id,
                original_provider.settings_config.clone(),
            ),
        },
        AppType::OpenClaw => crate::openclaw_config::set_provider(
            &original_provider.id,
            original_provider.settings_config.clone(),
        )
        .map(|_| ()),
        AppType::Hermes => crate::hermes_config::set_provider(
            &original_provider.id,
            original_provider.settings_config.clone(),
        )
        .map(|_| ()),
        _ => Ok(()),
    }
}

fn codex_provider_uses_managed_auth(app: &AppType, provider: &Provider) -> bool {
    if !matches!(app, AppType::Codex) {
        return false;
    }

    provider.category.as_deref() == Some("official")
        || provider
            .settings_config
            .get("auth")
            .is_some_and(crate::codex_config::codex_auth_has_oauth_login_material)
}

fn set_provider_api_key(
    config: &mut serde_json::Value,
    app: &AppType,
    key: &str,
    api_key_field: Option<&str>,
) {
    match app {
        AppType::OpenCode => set_nested_api_key(config, &["options", "apiKey"], key),
        AppType::OpenClaw => set_top_level_api_key(config, key),
        AppType::Hermes => set_hermes_api_key(config, key),
        AppType::Codex => set_codex_api_key(config, key),
        AppType::Gemini => set_env_api_key(config, "GEMINI_API_KEY", key),
        AppType::Claude | AppType::ClaudeDesktop => {
            set_anthropic_api_key(config, key, api_key_field)
        }
    }
}

fn set_top_level_api_key(config: &mut serde_json::Value, key: &str) {
    if !config.is_object() {
        *config = serde_json::json!({});
    }
    if let Some(obj) = config.as_object_mut() {
        obj.insert(
            "apiKey".to_string(),
            serde_json::Value::String(key.to_string()),
        );
    }
}

fn set_hermes_api_key(config: &mut serde_json::Value, key: &str) {
    if !config.is_object() {
        *config = serde_json::json!({});
    }
    if let Some(obj) = config.as_object_mut() {
        obj.remove("apiKey");
        obj.insert(
            "api_key".to_string(),
            serde_json::Value::String(key.to_string()),
        );
    }
}

fn set_codex_api_key(config: &mut serde_json::Value, key: &str) {
    if !config.is_object() {
        *config = serde_json::json!({});
    }

    let Some(obj) = config.as_object_mut() else {
        return;
    };

    {
        let auth = obj
            .entry("auth".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !auth.is_object() {
            *auth = serde_json::json!({});
        }
        if let Some(auth_obj) = auth.as_object_mut() {
            auth_obj.insert(
                "OPENAI_API_KEY".to_string(),
                serde_json::Value::String(key.to_string()),
            );
        }
    }

    if let Some(updated_config) = obj
        .get("config")
        .and_then(|value| value.as_str())
        .and_then(|text| update_codex_existing_experimental_bearer_token(text, key))
    {
        obj.insert(
            "config".to_string(),
            serde_json::Value::String(updated_config),
        );
    }
}

fn update_codex_existing_experimental_bearer_token(config_text: &str, key: &str) -> Option<String> {
    if !config_text.contains("experimental_bearer_token") {
        return None;
    }

    let mut doc = config_text.parse::<toml_edit::DocumentMut>().ok()?;
    let provider_id = doc
        .get("model_provider")
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);

    if let Some(provider_id) = provider_id
        .as_deref()
        .filter(|id| crate::codex_config::is_custom_codex_model_provider_id(id))
    {
        if let Some(provider_table) = doc
            .get_mut("model_providers")
            .and_then(|item| item.as_table_mut())
            .and_then(|table| table.get_mut(provider_id))
            .and_then(|item| item.as_table_mut())
        {
            if provider_table.get("experimental_bearer_token").is_some() {
                provider_table["experimental_bearer_token"] = toml_edit::value(key);
                return Some(doc.to_string());
            }
        }
    }

    if doc.get("experimental_bearer_token").is_some() {
        doc["experimental_bearer_token"] = toml_edit::value(key);
        return Some(doc.to_string());
    }

    None
}

fn set_anthropic_api_key(config: &mut serde_json::Value, key: &str, api_key_field: Option<&str>) {
    if let Some(obj) = config.as_object_mut() {
        if obj.contains_key("apiKey") {
            obj.insert(
                "apiKey".to_string(),
                serde_json::Value::String(key.to_string()),
            );
            return;
        }
    }

    if !config.is_object() {
        *config = serde_json::json!({});
    }

    let Some(obj) = config.as_object_mut() else {
        return;
    };
    let env = obj
        .entry("env".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !env.is_object() {
        *env = serde_json::json!({});
    }
    let Some(env_obj) = env.as_object_mut() else {
        return;
    };

    let field = if env_obj.contains_key("ANTHROPIC_AUTH_TOKEN") {
        "ANTHROPIC_AUTH_TOKEN"
    } else if env_obj.contains_key("ANTHROPIC_API_KEY") {
        "ANTHROPIC_API_KEY"
    } else if api_key_field == Some("ANTHROPIC_API_KEY") {
        "ANTHROPIC_API_KEY"
    } else {
        "ANTHROPIC_AUTH_TOKEN"
    };
    env_obj.insert(
        field.to_string(),
        serde_json::Value::String(key.to_string()),
    );
}

fn set_env_api_key(config: &mut serde_json::Value, field: &str, key: &str) {
    if !config.is_object() {
        *config = serde_json::json!({});
    }

    let Some(obj) = config.as_object_mut() else {
        return;
    };
    let env = obj
        .entry("env".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !env.is_object() {
        *env = serde_json::json!({});
    }
    if let Some(env_obj) = env.as_object_mut() {
        env_obj.insert(
            field.to_string(),
            serde_json::Value::String(key.to_string()),
        );
    }
}

fn set_nested_api_key(config: &mut serde_json::Value, path: &[&str], key: &str) {
    if !config.is_object() {
        *config = serde_json::json!({});
    }

    let mut current = config;
    for segment in &path[..path.len().saturating_sub(1)] {
        if !current.is_object() {
            *current = serde_json::json!({});
        }
        let Some(obj) = current.as_object_mut() else {
            return;
        };
        current = obj
            .entry((*segment).to_string())
            .or_insert_with(|| serde_json::json!({}));
    }

    if let Some(field) = path.last() {
        if !current.is_object() {
            *current = serde_json::json!({});
        }
        if let Some(obj) = current.as_object_mut() {
            obj.insert(
                (*field).to_string(),
                serde_json::Value::String(key.to_string()),
            );
        }
    }
}

pub fn run_cli_args_with_state<I>(state: &AppState, args: I) -> Option<CliOutput>
where
    I: IntoIterator<Item = String>,
{
    match parse_cli_args(args) {
        CliParseOutcome::Command(command) => Some(run_cli_command(state, command)),
        CliParseOutcome::Help => Some(CliOutput {
            stdout: help_text(),
            stderr: String::new(),
            exit_code: 0,
        }),
        CliParseOutcome::Error(error) => Some(error_output(&error, error.json)),
        CliParseOutcome::NotCli => None,
    }
}

pub fn run_if_cli_args<I>(args: I) -> Option<i32>
where
    I: IntoIterator<Item = String>,
{
    let args: Vec<String> = args.into_iter().collect();
    if matches!(parse_cli_args(args.clone()), CliParseOutcome::NotCli) {
        return None;
    }

    let _ = crate::app_store::refresh_app_config_dir_override_from_disk_for_cli();

    #[cfg(target_os = "linux")]
    match run_cli_via_gui_ipc(&args) {
        CliIpcAttempt::Handled(output) => {
            print_cli_output(&output);
            return Some(output.exit_code);
        }
        CliIpcAttempt::Failed(message) => {
            let json = cli_args_request_json(&args);
            let output = error_output(
                &CliError {
                    code: "ipc_failed",
                    message,
                    exit_code: 1,
                    json,
                },
                json,
            );
            print_cli_output(&output);
            return Some(output.exit_code);
        }
        CliIpcAttempt::Unavailable | CliIpcAttempt::NotHandled => {}
    }

    let output = match parse_cli_args(args) {
        CliParseOutcome::Command(command) => {
            let json = match &command {
                CliCommand::List { json, .. } => *json,
                CliCommand::Switch { json, .. } => *json,
                CliCommand::UpdateKey { json, .. } => *json,
            };
            let db = match Database::init() {
                Ok(db) => std::sync::Arc::new(db),
                Err(err) => {
                    let output = error_output(
                        &CliError {
                            code: "startup_failed",
                            message: err.to_string(),
                            exit_code: 1,
                            json,
                        },
                        json,
                    );
                    print_cli_output(&output);
                    return Some(output.exit_code);
                }
            };
            let state = AppState::new(db);
            run_cli_command(&state, command)
        }
        CliParseOutcome::Help => CliOutput {
            stdout: help_text(),
            stderr: String::new(),
            exit_code: 0,
        },
        CliParseOutcome::Error(error) => error_output(&error, error.json),
        CliParseOutcome::NotCli => return None,
    };
    print_cli_output(&output);
    Some(output.exit_code)
}

#[cfg(target_os = "linux")]
enum CliIpcAttempt {
    Handled(CliOutput),
    NotHandled,
    Unavailable,
    Failed(String),
}

fn cli_args_request_json(args: &[String]) -> bool {
    match parse_cli_args(args.to_vec()) {
        CliParseOutcome::Command(CliCommand::List { json, .. })
        | CliParseOutcome::Command(CliCommand::Switch { json, .. })
        | CliParseOutcome::Command(CliCommand::UpdateKey { json, .. }) => json,
        CliParseOutcome::Error(error) => error.json,
        _ => args.iter().any(|arg| arg == "--json"),
    }
}

#[cfg(target_os = "linux")]
fn run_cli_via_gui_ipc(args: &[String]) -> CliIpcAttempt {
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let socket_path = cli_ipc_socket_path();
    let mut stream = match UnixStream::connect(socket_path) {
        Ok(stream) => stream,
        Err(_) => return CliIpcAttempt::Unavailable,
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    let request = CliIpcRequest {
        args: args.to_vec(),
    };
    let payload = match serde_json::to_string(&request) {
        Ok(payload) => payload,
        Err(err) => {
            return CliIpcAttempt::Failed(format!("Failed to serialize CLI IPC request: {err}"))
        }
    };
    if let Err(err) = writeln!(stream, "{payload}") {
        return CliIpcAttempt::Failed(format!("Failed to send CLI IPC request: {err}"));
    }

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let bytes_read = match reader.read_line(&mut line) {
        Ok(bytes_read) => bytes_read,
        Err(err) => {
            return CliIpcAttempt::Failed(format!("Failed to read CLI IPC response: {err}"))
        }
    };
    if bytes_read == 0 {
        return CliIpcAttempt::Failed(
            "CLI IPC server closed the connection without a response".to_string(),
        );
    }

    let response: CliIpcResponse = match serde_json::from_str(line.trim_end()) {
        Ok(response) => response,
        Err(err) => {
            return CliIpcAttempt::Failed(format!("Failed to parse CLI IPC response: {err}"))
        }
    };
    if !response.handled {
        return CliIpcAttempt::NotHandled;
    }

    CliIpcAttempt::Handled(CliOutput {
        stdout: response.stdout,
        stderr: response.stderr,
        exit_code: response.exit_code,
    })
}

#[cfg(target_os = "linux")]
pub fn start_gui_ipc_server(app_handle: tauri::AppHandle) {
    if let Err(err) = std::thread::Builder::new()
        .name("cc-switch-cli-ipc".to_string())
        .spawn(move || run_gui_ipc_server(app_handle))
    {
        log::warn!("Failed to spawn CLI IPC server: {err}");
    }
}

#[cfg(target_os = "linux")]
fn run_gui_ipc_server(app_handle: tauri::AppHandle) {
    use std::os::unix::net::UnixListener;

    let socket_path = cli_ipc_socket_path();
    if let Some(parent) = socket_path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            log::warn!("Failed to create CLI IPC directory: {err}");
            return;
        }
        if let Err(err) = set_owner_only_dir_permissions(parent) {
            log::warn!("Failed to secure CLI IPC directory: {err}");
            return;
        }
    }

    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }

    let listener = match UnixListener::bind(&socket_path) {
        Ok(listener) => listener,
        Err(err) => {
            log::warn!("Failed to bind CLI IPC socket {:?}: {err}", socket_path);
            return;
        }
    };
    if let Err(err) = set_owner_only_socket_permissions(&socket_path) {
        log::warn!("Failed to secure CLI IPC socket {:?}: {err}", socket_path);
        let _ = std::fs::remove_file(&socket_path);
        return;
    }

    log::info!("CLI IPC server listening at {:?}", socket_path);
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let app_handle = app_handle.clone();
                std::thread::spawn(move || handle_gui_ipc_connection(&app_handle, stream));
            }
            Err(err) => log::debug!("CLI IPC accept failed: {err}"),
        }
    }
}

#[cfg(target_os = "linux")]
fn handle_gui_ipc_connection(
    app_handle: &tauri::AppHandle,
    mut stream: std::os::unix::net::UnixStream,
) {
    let response = match read_gui_ipc_request(&stream) {
        Ok(request) => execute_gui_ipc_request(app_handle, request),
        Err(message) => CliIpcResponse {
            handled: true,
            stdout: String::new(),
            stderr: format!("{message}\n"),
            exit_code: 1,
        },
    };

    if let Ok(payload) = serde_json::to_string(&response) {
        let _ = writeln!(stream, "{payload}");
    }
}

#[cfg(target_os = "linux")]
fn read_gui_ipc_request(stream: &std::os::unix::net::UnixStream) -> Result<CliIpcRequest, String> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|err| format!("Failed to read CLI IPC request: {err}"))?;
    serde_json::from_str(line.trim_end())
        .map_err(|err| format!("Failed to parse CLI IPC request: {err}"))
}

#[cfg(target_os = "linux")]
fn execute_gui_ipc_request(
    app_handle: &tauri::AppHandle,
    request: CliIpcRequest,
) -> CliIpcResponse {
    use tauri::Manager;

    let parsed = parse_cli_args(request.args.clone());
    let provider_change_event = match &parsed {
        CliParseOutcome::Command(CliCommand::Switch {
            app, provider_id, ..
        }) => Some((
            "provider-switched",
            app.as_str().to_string(),
            provider_id.clone(),
        )),
        CliParseOutcome::Command(CliCommand::UpdateKey {
            app, provider_id, ..
        }) => Some((
            "provider-updated",
            app.as_str().to_string(),
            provider_id.clone(),
        )),
        _ => None,
    };

    let Some(app_state) = app_handle.try_state::<AppState>() else {
        return CliIpcResponse::not_handled();
    };

    let output = match parsed {
        CliParseOutcome::NotCli => return CliIpcResponse::not_handled(),
        CliParseOutcome::Command(command) => run_cli_command(app_state.inner(), command),
        CliParseOutcome::Help => CliOutput {
            stdout: help_text(),
            stderr: String::new(),
            exit_code: 0,
        },
        CliParseOutcome::Error(error) => error_output(&error, error.json),
    };

    if output.exit_code == 0 {
        if let Some((event_name, app_type, provider_id)) = provider_change_event {
            emit_provider_changed(app_handle, event_name, &app_type, &provider_id);
        }
    }

    CliIpcResponse {
        handled: true,
        stdout: output.stdout,
        stderr: output.stderr,
        exit_code: output.exit_code,
    }
}

#[cfg(target_os = "linux")]
impl CliIpcResponse {
    fn not_handled() -> Self {
        Self {
            handled: false,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
        }
    }
}

#[cfg(target_os = "linux")]
fn emit_provider_changed(
    app_handle: &tauri::AppHandle,
    event_name: &str,
    app_type: &str,
    provider_id: &str,
) {
    use tauri::{Emitter, Manager};

    if let Some(app_state) = app_handle.try_state::<AppState>() {
        if let Ok(new_menu) = crate::tray::create_tray_menu(app_handle, app_state.inner()) {
            if let Some(tray) = app_handle.tray_by_id(crate::tray::TRAY_ID) {
                if let Err(err) = tray.set_menu(Some(new_menu)) {
                    log::debug!("Failed to refresh tray menu after CLI provider change: {err}");
                }
            }
        }
    }

    let event_data = serde_json::json!({
        "appType": app_type,
        "providerId": provider_id,
    });
    if let Err(err) = app_handle.emit(event_name, event_data) {
        log::debug!("Failed to emit {event_name} after CLI provider change: {err}");
    }
}

#[cfg(target_os = "linux")]
fn cli_ipc_socket_path() -> std::path::PathBuf {
    cli_ipc_dir().join(CLI_IPC_SOCKET_FILE)
}

#[cfg(target_os = "linux")]
fn cli_ipc_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .and_then(|value| {
            let path = std::path::PathBuf::from(value);
            if path.as_os_str().is_empty() || !path.is_absolute() {
                None
            } else {
                Some(path)
            }
        })
        .unwrap_or_else(|| {
            crate::config::get_home_dir()
                .join(".cc-switch")
                .join("runtime")
        })
        .join(CLI_IPC_DIR)
}

#[cfg(target_os = "linux")]
fn set_owner_only_dir_permissions(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(target_os = "linux")]
fn set_owner_only_socket_permissions(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

fn print_cli_output(output: &CliOutput) {
    if !output.stdout.is_empty() {
        print!("{}", output.stdout);
    }
    if !output.stderr.is_empty() {
        eprint!("{}", output.stderr);
    }
}

fn help_text() -> String {
    "Usage: cc-switch list [app] [--json]\nUsage: cc-switch switch <app> <provider-id> [--json]\nUsage: cc-switch update-key <app> <provider-id> <key> [--json]\n"
        .to_string()
}

pub fn run_cli_command(state: &AppState, command: CliCommand) -> CliOutput {
    match command {
        CliCommand::List { app, json } => match collect_provider_records(state.db.as_ref(), app) {
            Ok(records) => {
                let formatted = if json {
                    format_list_json(&records)
                } else {
                    Ok(format_list_text(&records))
                };
                match formatted {
                    Ok(stdout) => CliOutput {
                        stdout: ensure_trailing_newline(stdout),
                        stderr: String::new(),
                        exit_code: 0,
                    },
                    Err(err) => error_output(
                        &CliError {
                            code: "list_failed",
                            message: err.to_string(),
                            exit_code: 1,
                            json,
                        },
                        json,
                    ),
                }
            }
            Err(err) => error_output(
                &CliError {
                    code: "list_failed",
                    message: err.to_string(),
                    exit_code: 1,
                    json,
                },
                json,
            ),
        },
        CliCommand::Switch {
            app,
            provider_id,
            json,
        } => match execute_switch(state, app.clone(), &provider_id) {
            Ok(result) => {
                let formatted = if json {
                    format_switch_json(&app, &provider_id, &result.warnings)
                } else {
                    Ok(format_switch_text(&app, &provider_id))
                };
                match formatted {
                    Ok(stdout) => CliOutput {
                        stdout: ensure_trailing_newline(stdout),
                        stderr: String::new(),
                        exit_code: 0,
                    },
                    Err(err) => error_output(
                        &CliError {
                            code: "switch_failed",
                            message: err.to_string(),
                            exit_code: 1,
                            json,
                        },
                        json,
                    ),
                }
            }
            Err(err) => error_output(
                &CliError {
                    code: err.code,
                    message: err.message,
                    exit_code: err.exit_code,
                    json,
                },
                json,
            ),
        },
        CliCommand::UpdateKey {
            app,
            provider_id,
            key,
            json,
        } => match execute_update_key(state, app.clone(), &provider_id, &key) {
            Ok(()) => {
                let formatted = if json {
                    format_update_key_json(&app, &provider_id)
                } else {
                    Ok(format_update_key_text(&app, &provider_id))
                };
                match formatted {
                    Ok(stdout) => CliOutput {
                        stdout: ensure_trailing_newline(stdout),
                        stderr: String::new(),
                        exit_code: 0,
                    },
                    Err(err) => error_output(
                        &CliError {
                            code: "update_key_failed",
                            message: err.to_string(),
                            exit_code: 1,
                            json,
                        },
                        json,
                    ),
                }
            }
            Err(err) => error_output(
                &CliError {
                    code: err.code,
                    message: err.message,
                    exit_code: err.exit_code,
                    json,
                },
                json,
            ),
        },
    }
}

pub fn error_output(error: &CliError, json: bool) -> CliOutput {
    let stderr = if json {
        serde_json::json!({
            "ok": false,
            "code": error.code,
            "error": error.message,
        })
        .to_string()
            + "\n"
    } else {
        format!("{}\n", sanitize_text_output(&error.message))
    };

    CliOutput {
        stdout: String::new(),
        stderr,
        exit_code: error.exit_code,
    }
}

fn ensure_trailing_newline(mut output: String) -> String {
    if !output.ends_with('\n') {
        output.push('\n');
    }
    output
}

fn sanitize_text_output(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { '?' } else { ch })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use crate::provider::{Provider, ProviderMeta};
    use serial_test::serial;
    use std::env;
    use std::sync::{Arc, Mutex, OnceLock};
    use tempfile::TempDir;

    fn parse(args: &[&str]) -> CliParseOutcome {
        parse_cli_args(args.iter().map(|arg| arg.to_string()))
    }

    struct CurrentProviderSettingsSnapshot {
        values: Vec<(AppType, Option<String>)>,
    }

    impl CurrentProviderSettingsSnapshot {
        fn capture() -> Self {
            Self {
                values: AppType::all()
                    .map(|app| {
                        let current_provider = crate::settings::get_current_provider(&app);
                        (app, current_provider)
                    })
                    .collect(),
            }
        }
    }

    impl Drop for CurrentProviderSettingsSnapshot {
        fn drop(&mut self) {
            for (app, current_provider) in &self.values {
                crate::settings::set_current_provider(app, current_provider.as_deref())
                    .expect("restore current provider setting");
            }
        }
    }

    fn settings_test_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|err| err.into_inner())
    }

    struct TempHome {
        _dir: TempDir,
        original_home: Option<String>,
        original_userprofile: Option<String>,
        original_test_home: Option<String>,
    }

    impl TempHome {
        fn new() -> Self {
            let dir = TempDir::new().expect("create temp home");
            let original_home = env::var("HOME").ok();
            let original_userprofile = env::var("USERPROFILE").ok();
            let original_test_home = env::var("CC_SWITCH_TEST_HOME").ok();

            env::set_var("HOME", dir.path());
            env::set_var("USERPROFILE", dir.path());
            env::set_var("CC_SWITCH_TEST_HOME", dir.path());
            crate::settings::reload_settings().expect("reload temp settings");

            Self {
                _dir: dir,
                original_home,
                original_userprofile,
                original_test_home,
            }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            match &self.original_home {
                Some(value) => env::set_var("HOME", value),
                None => env::remove_var("HOME"),
            }
            match &self.original_userprofile {
                Some(value) => env::set_var("USERPROFILE", value),
                None => env::remove_var("USERPROFILE"),
            }
            match &self.original_test_home {
                Some(value) => env::set_var("CC_SWITCH_TEST_HOME", value),
                None => env::remove_var("CC_SWITCH_TEST_HOME"),
            }
            crate::settings::reload_settings().expect("restore settings");
        }
    }

    fn provider(id: &str, name: &str) -> Provider {
        Provider::with_id(
            id.to_string(),
            name.to_string(),
            serde_json::json!({}),
            None,
        )
    }

    fn claude_provider(id: &str, name: &str, api_key: &str) -> Provider {
        Provider::with_id(
            id.to_string(),
            name.to_string(),
            serde_json::json!({
                "env": {
                    "ANTHROPIC_API_KEY": api_key,
                },
            }),
            None,
        )
    }

    fn provider_api_key(provider: &Provider) -> Option<&str> {
        provider
            .settings_config
            .get("env")
            .and_then(|env| env.get("ANTHROPIC_API_KEY"))
            .and_then(|value| value.as_str())
    }

    fn auth_token(provider: &Provider) -> Option<&str> {
        provider
            .settings_config
            .get("env")
            .and_then(|env| env.get("ANTHROPIC_AUTH_TOKEN"))
            .and_then(|value| value.as_str())
    }

    fn codex_auth_key(provider: &Provider) -> Option<&str> {
        provider
            .settings_config
            .get("auth")
            .and_then(|auth| auth.get("OPENAI_API_KEY"))
            .and_then(|value| value.as_str())
    }

    #[test]
    #[serial]
    fn collect_provider_records_lists_one_app_and_marks_effective_current() {
        let _guard = settings_test_guard();
        crate::settings::reload_settings().expect("reload settings");
        let _settings_snapshot = CurrentProviderSettingsSnapshot::capture();
        crate::settings::set_current_provider(&AppType::Claude, None)
            .expect("clear local current provider");

        let db = Database::memory().expect("create memory db");
        db.save_provider("claude", &provider("p1", "PackyCode"))
            .expect("save p1");
        db.save_provider("claude", &provider("p2", "OpenRouter"))
            .expect("save p2");
        db.set_current_provider("claude", "p2")
            .expect("set db current provider");

        let records =
            collect_provider_records(&db, Some(AppType::Claude)).expect("collect records");

        assert_eq!(
            records,
            vec![
                CliProviderRecord {
                    app: "claude".to_string(),
                    id: "p1".to_string(),
                    name: "PackyCode".to_string(),
                    current: false,
                },
                CliProviderRecord {
                    app: "claude".to_string(),
                    id: "p2".to_string(),
                    name: "OpenRouter".to_string(),
                    current: true,
                },
            ]
        );
    }

    #[test]
    #[serial]
    fn collect_provider_records_lists_all_apps_in_app_type_order() {
        let _guard = settings_test_guard();
        crate::settings::reload_settings().expect("reload settings");
        let _settings_snapshot = CurrentProviderSettingsSnapshot::capture();
        crate::settings::set_current_provider(&AppType::Claude, None)
            .expect("clear claude current provider");
        crate::settings::set_current_provider(&AppType::Gemini, None)
            .expect("clear gemini current provider");

        let db = Database::memory().expect("create memory db");
        db.save_provider("claude", &provider("claude-p1", "Claude Provider"))
            .expect("save claude provider");
        db.save_provider("gemini", &provider("gemini-p1", "Gemini Provider"))
            .expect("save gemini provider");

        let records = collect_provider_records(&db, None).expect("collect records");

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].app, "claude");
        assert_eq!(records[0].id, "claude-p1");
        assert_eq!(records[1].app, "gemini");
        assert_eq!(records[1].id, "gemini-p1");
    }

    #[test]
    #[serial]
    fn collect_provider_records_does_not_clear_stale_local_current_provider() {
        let _guard = settings_test_guard();
        crate::settings::reload_settings().expect("reload settings");
        let _settings_snapshot = CurrentProviderSettingsSnapshot::capture();
        crate::settings::set_current_provider(&AppType::Claude, Some("stale"))
            .expect("set stale local current provider");

        let db = Database::memory().expect("create memory db");
        db.save_provider("claude", &provider("p1", "PackyCode"))
            .expect("save p1");
        db.set_current_provider("claude", "p1")
            .expect("set db current provider");

        let records =
            collect_provider_records(&db, Some(AppType::Claude)).expect("collect records");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, "p1");
        assert!(records[0].current);
        assert_eq!(
            crate::settings::get_current_provider(&AppType::Claude),
            Some("stale".to_string())
        );
    }

    #[test]
    fn format_list_text_groups_by_app_and_marks_current() {
        let records = vec![
            CliProviderRecord {
                app: "claude".to_string(),
                id: "claude-1".to_string(),
                name: "Claude One".to_string(),
                current: true,
            },
            CliProviderRecord {
                app: "claude".to_string(),
                id: "claude-2".to_string(),
                name: "Claude Two".to_string(),
                current: false,
            },
            CliProviderRecord {
                app: "codex".to_string(),
                id: "codex-1".to_string(),
                name: "Codex One".to_string(),
                current: true,
            },
        ];

        assert_eq!(
            format_list_text(&records),
            "claude\n* claude-1  Claude One\n  claude-2  Claude Two\n\ncodex\n* codex-1  Codex One\n"
        );
    }

    #[test]
    fn format_list_json_uses_camel_case_records() {
        let records = vec![CliProviderRecord {
            app: "claude".to_string(),
            id: "provider-1".to_string(),
            name: "Provider One".to_string(),
            current: true,
        }];

        assert_eq!(
            format_list_json(&records).unwrap(),
            r#"[{"app":"claude","id":"provider-1","name":"Provider One","current":true}]"#
        );
    }

    #[test]
    fn format_switch_text_includes_app_and_provider_id() {
        assert_eq!(
            format_switch_text(&AppType::Claude, "provider-1"),
            "Switched claude to provider-1\n"
        );
    }

    #[test]
    fn format_switch_json_includes_warnings() {
        let warnings = vec!["config warning".to_string()];

        assert_eq!(
            format_switch_json(&AppType::Codex, "provider-2", &warnings).unwrap(),
            r#"{"ok":true,"app":"codex","providerId":"provider-2","warnings":["config warning"]}"#
        );
    }

    #[test]
    fn format_update_key_text_includes_app_and_provider_id() {
        assert_eq!(
            format_update_key_text(&AppType::Claude, "provider-1"),
            "Updated API key for claude provider provider-1\n"
        );
    }

    #[test]
    fn format_update_key_json_reports_success_without_key_value() {
        assert_eq!(
            format_update_key_json(&AppType::Codex, "provider-2").unwrap(),
            r#"{"ok":true,"app":"codex","providerId":"provider-2"}"#
        );
    }

    #[test]
    fn set_provider_api_key_updates_opencode_options_api_key() {
        let mut config = serde_json::json!({
            "options": {
                "apiKey": "old-key",
                "baseURL": "https://example.test"
            }
        });

        set_provider_api_key(&mut config, &AppType::OpenCode, "new-key", None);

        assert_eq!(config["options"]["apiKey"], "new-key");
        assert!(config.get("env").is_none());
    }

    #[test]
    fn set_provider_api_key_updates_top_level_api_key_for_openclaw() {
        let mut config = serde_json::json!({
            "apiKey": "old-key",
            "baseUrl": "https://example.test"
        });

        set_provider_api_key(&mut config, &AppType::OpenClaw, "new-key", None);

        assert_eq!(config["apiKey"], "new-key");
    }

    #[test]
    fn set_provider_api_key_updates_codex_auth_openai_api_key() {
        let mut config = serde_json::json!({
            "auth": {
                "OPENAI_API_KEY": "old-key"
            },
            "config": "model_provider = \"openrouter\"\n[model_providers.openrouter]\nbase_url = \"https://openrouter.ai/api/v1\"\n"
        });

        set_provider_api_key(&mut config, &AppType::Codex, "new-key", None);

        assert_eq!(config["auth"]["OPENAI_API_KEY"], "new-key");
        assert!(config.get("env").is_none());
    }

    #[test]
    fn set_provider_api_key_updates_codex_existing_experimental_bearer_token() {
        let mut config = serde_json::json!({
            "auth": {},
            "config": "model_provider = \"openrouter\"\n[model_providers.openrouter]\nbase_url = \"https://openrouter.ai/api/v1\"\nexperimental_bearer_token = \"old-key\"\n"
        });

        set_provider_api_key(&mut config, &AppType::Codex, "new-key", None);

        let config_text = config["config"].as_str().expect("codex config text");
        assert!(config_text.contains("experimental_bearer_token = \"new-key\""));
        assert!(config.get("env").is_none());
    }

    #[test]
    fn set_provider_api_key_updates_hermes_snake_case_api_key() {
        let mut config = serde_json::json!({
            "base_url": "https://example.test",
            "api_key": "old-key"
        });

        set_provider_api_key(&mut config, &AppType::Hermes, "new-key", None);

        assert_eq!(config["api_key"], "new-key");
        assert!(config.get("apiKey").is_none());
    }

    #[test]
    #[serial]
    fn execute_update_key_rejects_managed_account_provider() {
        let _guard = settings_test_guard();
        let _home = TempHome::new();

        let db = Arc::new(Database::memory().expect("create memory db"));
        let state = AppState::new(db.clone());
        let mut provider = claude_provider("copilot", "Copilot", "old-key");
        provider.meta = Some(ProviderMeta {
            provider_type: Some("github_copilot".to_string()),
            ..Default::default()
        });
        db.save_provider("claude", &provider)
            .expect("save provider");

        let error = execute_update_key(&state, AppType::Claude, "copilot", "new-key")
            .expect_err("managed account provider should be rejected");

        assert_eq!(error.code, "unsupported_provider_auth");
        assert_eq!(error.exit_code, 2);
    }

    #[test]
    #[serial]
    fn execute_update_key_rejects_codex_official_provider() {
        let _guard = settings_test_guard();
        let _home = TempHome::new();

        let db = Arc::new(Database::memory().expect("create memory db"));
        let state = AppState::new(db.clone());
        let mut provider = Provider::with_id(
            "official".to_string(),
            "OpenAI Official".to_string(),
            serde_json::json!({
                "auth": {
                    "auth_mode": "chatgpt",
                    "tokens": {
                        "access_token": "oauth-access"
                    }
                },
                "config": ""
            }),
            None,
        );
        provider.category = Some("official".to_string());
        db.save_provider("codex", &provider).expect("save provider");

        let error = execute_update_key(&state, AppType::Codex, "official", "new-key")
            .expect_err("official codex provider should be rejected");

        assert_eq!(error.code, "unsupported_provider_auth");
        assert_eq!(error.exit_code, 2);
    }

    #[test]
    #[serial]
    fn execute_update_key_uses_meta_api_key_field_when_claude_env_key_missing() {
        let _guard = settings_test_guard();
        let _home = TempHome::new();

        let db = Arc::new(Database::memory().expect("create memory db"));
        let state = AppState::new(db.clone());
        let mut provider = Provider::with_id(
            "p1".to_string(),
            "Claude One".to_string(),
            serde_json::json!({ "env": {} }),
            None,
        );
        provider.meta = Some(ProviderMeta {
            api_key_field: Some("ANTHROPIC_API_KEY".to_string()),
            ..Default::default()
        });
        db.save_provider("claude", &provider)
            .expect("save provider");

        execute_update_key(&state, AppType::Claude, "p1", "new-key").expect("update api key");

        let updated = db
            .get_provider_by_id("p1", "claude")
            .expect("read provider")
            .expect("provider exists");
        assert_eq!(provider_api_key(&updated), Some("new-key"));
        assert_eq!(auth_token(&updated), None);
    }

    #[test]
    #[serial]
    fn execute_update_key_updates_codex_auth_key_used_by_ui() {
        let _guard = settings_test_guard();
        let _home = TempHome::new();

        let db = Arc::new(Database::memory().expect("create memory db"));
        let state = AppState::new(db.clone());
        let provider = Provider::with_id(
            "p1".to_string(),
            "Codex One".to_string(),
            serde_json::json!({
                "auth": {
                    "OPENAI_API_KEY": "old-key"
                },
                "config": "model_provider = \"openrouter\"\n[model_providers.openrouter]\nbase_url = \"https://openrouter.ai/api/v1\"\n"
            }),
            None,
        );
        db.save_provider("codex", &provider).expect("save provider");

        execute_update_key(&state, AppType::Codex, "p1", "new-key").expect("update api key");

        let updated = db
            .get_provider_by_id("p1", "codex")
            .expect("read provider")
            .expect("provider exists");
        assert_eq!(updated.settings_config["auth"]["OPENAI_API_KEY"], "new-key");
        assert!(updated.settings_config.get("env").is_none());
    }

    #[test]
    #[serial]
    fn execute_update_key_updates_hermes_snake_case_key_used_by_ui() {
        let _guard = settings_test_guard();
        let _home = TempHome::new();

        let db = Arc::new(Database::memory().expect("create memory db"));
        let state = AppState::new(db.clone());
        let provider = Provider::with_id(
            "p1".to_string(),
            "Hermes One".to_string(),
            serde_json::json!({
                "base_url": "https://example.test",
                "api_key": "old-key"
            }),
            None,
        );
        db.save_provider("hermes", &provider)
            .expect("save provider");

        execute_update_key(&state, AppType::Hermes, "p1", "new-key").expect("update api key");

        let updated = db
            .get_provider_by_id("p1", "hermes")
            .expect("read provider")
            .expect("provider exists");
        assert_eq!(updated.settings_config["api_key"], "new-key");
        assert!(updated.settings_config.get("apiKey").is_none());
    }

    #[test]
    #[serial]
    fn execute_switch_updates_current_provider_by_id() {
        let _guard = settings_test_guard();
        let _home = TempHome::new();

        let db = Arc::new(Database::memory().expect("create memory db"));
        let state = AppState::new(db.clone());
        db.save_provider("claude", &claude_provider("p1", "Claude One", "key-one"))
            .expect("save p1");
        db.save_provider("claude", &claude_provider("p2", "Claude Two", "key-two"))
            .expect("save p2");
        db.set_current_provider("claude", "p1")
            .expect("set db current provider");
        crate::settings::set_current_provider(&AppType::Claude, Some("p1"))
            .expect("set local current provider");

        let result = execute_switch(&state, AppType::Claude, "p2").expect("switch provider");

        assert!(result.warnings.is_empty());
        assert_eq!(
            db.get_current_provider("claude")
                .expect("get db current provider"),
            Some("p2".to_string())
        );
        assert_eq!(
            crate::settings::get_current_provider(&AppType::Claude),
            Some("p2".to_string())
        );
    }

    #[test]
    #[serial]
    fn execute_switch_maps_missing_provider_to_provider_not_found_error() {
        let _guard = settings_test_guard();
        let _home = TempHome::new();

        let db = Arc::new(Database::memory().expect("create memory db"));
        let state = AppState::new(db);

        let error = execute_switch(&state, AppType::Claude, "missing").unwrap_err();

        assert_eq!(error.code, "provider_not_found");
        assert_eq!(error.exit_code, 1);
        assert_eq!(error.message, "Provider not found: missing for claude");
    }

    #[test]
    #[serial]
    fn execute_update_key_updates_existing_provider_api_key() {
        let _guard = settings_test_guard();
        let _home = TempHome::new();

        let db = Arc::new(Database::memory().expect("create memory db"));
        let state = AppState::new(db.clone());
        db.save_provider("claude", &claude_provider("p1", "Claude One", "old-key"))
            .expect("save provider");

        execute_update_key(&state, AppType::Claude, "p1", "new-key").expect("update api key");

        let updated = db
            .get_provider_by_id("p1", "claude")
            .expect("read provider")
            .expect("provider exists");
        assert_eq!(provider_api_key(&updated), Some("new-key"));
    }

    #[test]
    #[serial]
    fn execute_update_key_rejects_empty_key() {
        let _guard = settings_test_guard();
        let _home = TempHome::new();

        let db = Arc::new(Database::memory().expect("create memory db"));
        let state = AppState::new(db);

        let error = execute_update_key(&state, AppType::Claude, "p1", "").unwrap_err();

        assert_eq!(error.code, "empty_api_key");
        assert_eq!(error.exit_code, 2);
    }

    #[test]
    #[serial]
    fn execute_update_key_rejects_blank_key() {
        let _guard = settings_test_guard();
        let _home = TempHome::new();

        let db = Arc::new(Database::memory().expect("create memory db"));
        let state = AppState::new(db.clone());
        db.save_provider("claude", &claude_provider("p1", "Claude One", "old-key"))
            .expect("save provider");

        let error = execute_update_key(&state, AppType::Claude, "p1", "   ")
            .expect_err("blank api key should be rejected");

        assert_eq!(error.code, "empty_api_key");
        assert_eq!(error.exit_code, 2);
    }

    #[test]
    #[serial]
    fn execute_update_key_rolls_back_provider_when_live_sync_fails() {
        let _guard = settings_test_guard();
        let _home = TempHome::new();

        let db = Arc::new(Database::memory().expect("create memory db"));
        let state = AppState::new(db.clone());
        let provider = Provider::with_id(
            "p1".to_string(),
            "Codex One".to_string(),
            serde_json::json!({
                "auth": {
                    "OPENAI_API_KEY": "old-key"
                },
                "config": ""
            }),
            None,
        );
        db.save_provider("codex", &provider).expect("save provider");
        db.set_current_provider("codex", "p1")
            .expect("set db current provider");
        crate::settings::set_current_provider(&AppType::Codex, Some("p1"))
            .expect("set local current provider");

        let error = execute_update_key(&state, AppType::Codex, "p1", "new-key")
            .expect_err("empty live codex config should reject bearer token write");

        assert_eq!(error.code, "update_key_failed");
        let updated = db
            .get_provider_by_id("p1", "codex")
            .expect("read provider")
            .expect("provider exists");
        assert_eq!(codex_auth_key(&updated), Some("old-key"));
    }

    #[test]
    #[serial]
    fn execute_update_key_rolls_back_live_config_when_mcp_sync_fails_after_live_write() {
        let _guard = settings_test_guard();
        let _home = TempHome::new();
        let codex_dir = crate::codex_config::get_codex_config_dir();
        std::fs::create_dir_all(&codex_dir).expect("create codex dir");

        let original_live_config = r#"model_provider = "custom"

[model_providers.custom]
name = "Old"
base_url = "https://old.example/v1"
experimental_bearer_token = "old-key"

[mcp_servers.bad]
type = "stdio"
command = "ok"
"#;
        crate::config::write_text_file(
            &crate::codex_config::get_codex_config_path(),
            original_live_config,
        )
        .expect("write initial live config");

        let db = Arc::new(Database::memory().expect("create memory db"));
        let state = AppState::new(db.clone());
        let provider = Provider::with_id(
            "p1".to_string(),
            "Codex One".to_string(),
            serde_json::json!({
                "auth": {
                    "OPENAI_API_KEY": "old-key"
                },
                "config": original_live_config
            }),
            None,
        );
        db.save_provider("codex", &provider).expect("save provider");
        db.set_current_provider("codex", "p1")
            .expect("set db current provider");
        crate::settings::set_current_provider(&AppType::Codex, Some("p1"))
            .expect("set local current provider");
        std::fs::create_dir_all(crate::opencode_config::get_opencode_dir())
            .expect("create opencode dir");
        db.save_mcp_server(&crate::app_config::McpServer {
            id: "bad".to_string(),
            name: "Bad".to_string(),
            server: serde_json::json!({
                "type": "unsupported"
            }),
            apps: crate::app_config::McpApps {
                opencode: true,
                ..Default::default()
            },
            description: None,
            homepage: None,
            docs: None,
            tags: Vec::new(),
        })
        .expect("save invalid mcp server");

        let error = execute_update_key(&state, AppType::Codex, "p1", "new-key")
            .expect_err("invalid MCP sync should fail after live write");

        assert_eq!(error.code, "update_key_failed");
        let updated = db
            .get_provider_by_id("p1", "codex")
            .expect("read provider")
            .expect("provider exists");
        assert_eq!(codex_auth_key(&updated), Some("old-key"));
        let live_config = std::fs::read_to_string(crate::codex_config::get_codex_config_path())
            .expect("read live config");
        assert!(live_config.contains("experimental_bearer_token = \"old-key\""));
        assert!(!live_config.contains("experimental_bearer_token = \"new-key\""));
    }

    #[test]
    #[serial]
    fn run_cli_command_outputs_text_list() {
        let _guard = settings_test_guard();
        crate::settings::reload_settings().expect("reload settings");
        let _settings_snapshot = CurrentProviderSettingsSnapshot::capture();
        crate::settings::set_current_provider(&AppType::Claude, None)
            .expect("clear local current provider");

        let db = Arc::new(Database::memory().expect("create memory db"));
        let state = AppState::new(db.clone());
        db.save_provider("claude", &provider("p1", "PackyCode"))
            .expect("save p1");
        db.save_provider("claude", &provider("p2", "OpenRouter"))
            .expect("save p2");
        db.set_current_provider("claude", "p2")
            .expect("set db current provider");

        let output = run_cli_command(
            &state,
            CliCommand::List {
                app: Some(AppType::Claude),
                json: false,
            },
        );

        assert_eq!(
            output,
            CliOutput {
                stdout: "claude\n  p1  PackyCode\n* p2  OpenRouter\n".to_string(),
                stderr: String::new(),
                exit_code: 0,
            }
        );
    }

    #[test]
    #[serial]
    fn run_cli_command_outputs_json_list() {
        let _guard = settings_test_guard();
        crate::settings::reload_settings().expect("reload settings");
        let _settings_snapshot = CurrentProviderSettingsSnapshot::capture();
        crate::settings::set_current_provider(&AppType::Claude, None)
            .expect("clear local current provider");

        let db = Arc::new(Database::memory().expect("create memory db"));
        let state = AppState::new(db.clone());
        db.save_provider("claude", &provider("p1", "PackyCode"))
            .expect("save p1");
        db.set_current_provider("claude", "p1")
            .expect("set db current provider");

        let output = run_cli_command(
            &state,
            CliCommand::List {
                app: Some(AppType::Claude),
                json: true,
            },
        );

        assert_eq!(
            output,
            CliOutput {
                stdout: r#"[{"app":"claude","id":"p1","name":"PackyCode","current":true}]"#
                    .to_string()
                    + "\n",
                stderr: String::new(),
                exit_code: 0,
            }
        );
    }

    #[test]
    #[serial]
    fn run_cli_command_outputs_text_switch_success() {
        let _guard = settings_test_guard();
        let _home = TempHome::new();

        let db = Arc::new(Database::memory().expect("create memory db"));
        let state = AppState::new(db.clone());
        db.save_provider("claude", &claude_provider("p1", "Claude One", "key-one"))
            .expect("save p1");
        db.save_provider("claude", &claude_provider("p2", "Claude Two", "key-two"))
            .expect("save p2");
        db.set_current_provider("claude", "p1")
            .expect("set db current provider");

        let output = run_cli_command(
            &state,
            CliCommand::Switch {
                app: AppType::Claude,
                provider_id: "p2".to_string(),
                json: false,
            },
        );

        assert_eq!(
            output,
            CliOutput {
                stdout: "Switched claude to p2\n".to_string(),
                stderr: String::new(),
                exit_code: 0,
            }
        );
    }

    #[test]
    #[serial]
    fn run_cli_command_outputs_json_switch_success() {
        let _guard = settings_test_guard();
        let _home = TempHome::new();

        let db = Arc::new(Database::memory().expect("create memory db"));
        let state = AppState::new(db.clone());
        db.save_provider("claude", &claude_provider("p1", "Claude One", "key-one"))
            .expect("save p1");
        db.save_provider("claude", &claude_provider("p2", "Claude Two", "key-two"))
            .expect("save p2");
        db.set_current_provider("claude", "p1")
            .expect("set db current provider");

        let output = run_cli_command(
            &state,
            CliCommand::Switch {
                app: AppType::Claude,
                provider_id: "p2".to_string(),
                json: true,
            },
        );
        let json: serde_json::Value = serde_json::from_str(output.stdout.trim_end())
            .expect("switch success output should be json");

        assert_eq!(output.stderr, "");
        assert_eq!(output.exit_code, 0);
        assert_eq!(json["ok"], true);
        assert_eq!(json["app"], "claude");
        assert_eq!(json["providerId"], "p2");
        assert!(json["warnings"].is_array());
    }

    #[test]
    #[serial]
    fn run_cli_command_outputs_text_update_key_success() {
        let _guard = settings_test_guard();
        let _home = TempHome::new();

        let db = Arc::new(Database::memory().expect("create memory db"));
        let state = AppState::new(db.clone());
        db.save_provider("claude", &claude_provider("p1", "Claude One", "old-key"))
            .expect("save provider");

        let output = run_cli_command(
            &state,
            CliCommand::UpdateKey {
                app: AppType::Claude,
                provider_id: "p1".to_string(),
                key: "new-key".to_string(),
                json: false,
            },
        );

        assert_eq!(
            output,
            CliOutput {
                stdout: "Updated API key for claude provider p1\n".to_string(),
                stderr: String::new(),
                exit_code: 0,
            }
        );
    }

    #[test]
    #[serial]
    fn run_cli_command_outputs_json_error_to_stderr() {
        let _guard = settings_test_guard();
        let _home = TempHome::new();

        let db = Arc::new(Database::memory().expect("create memory db"));
        let state = AppState::new(db);

        let output = run_cli_command(
            &state,
            CliCommand::Switch {
                app: AppType::Claude,
                provider_id: "missing".to_string(),
                json: true,
            },
        );

        assert_eq!(
            output,
            CliOutput {
                stdout: String::new(),
                stderr: r#"{"ok":false,"code":"provider_not_found","error":"Provider not found: missing for claude"}"#
                    .to_string()
                    + "\n",
                exit_code: 1,
            }
        );
    }

    #[test]
    fn run_cli_args_with_state_returns_none_for_gui_launch_without_subcommand() {
        let db = Arc::new(Database::memory().expect("create memory db"));
        let state = AppState::new(db);

        assert_eq!(
            run_cli_args_with_state(&state, vec!["cc-switch".to_string()]),
            None
        );
    }

    #[test]
    fn run_cli_args_with_state_outputs_help_without_running_gui() {
        let db = Arc::new(Database::memory().expect("create memory db"));
        let state = AppState::new(db);

        let output =
            run_cli_args_with_state(&state, vec!["cc-switch".to_string(), "--help".to_string()])
                .expect("help should be handled by cli");

        assert_eq!(output.stderr, "");
        assert_eq!(output.exit_code, 0);
        assert!(output
            .stdout
            .contains("Usage: cc-switch list [app] [--json]"));
        assert!(output
            .stdout
            .contains("Usage: cc-switch switch <app> <provider-id> [--json]"));
        assert!(output
            .stdout
            .contains("Usage: cc-switch update-key <app> <provider-id> <key> [--json]"));
    }

    #[test]
    #[serial]
    fn run_cli_args_with_state_executes_list_command() {
        let _guard = settings_test_guard();
        crate::settings::reload_settings().expect("reload settings");
        let _settings_snapshot = CurrentProviderSettingsSnapshot::capture();
        crate::settings::set_current_provider(&AppType::Claude, None)
            .expect("clear local current provider");

        let db = Arc::new(Database::memory().expect("create memory db"));
        let state = AppState::new(db.clone());
        db.save_provider("claude", &provider("p1", "PackyCode"))
            .expect("save p1");
        db.set_current_provider("claude", "p1")
            .expect("set db current provider");

        let output = run_cli_args_with_state(
            &state,
            vec![
                "cc-switch".to_string(),
                "list".to_string(),
                "claude".to_string(),
            ],
        )
        .expect("list should be handled by cli");

        assert_eq!(
            output,
            CliOutput {
                stdout: "claude\n* p1  PackyCode\n".to_string(),
                stderr: String::new(),
                exit_code: 0,
            }
        );
    }

    #[test]
    #[serial]
    fn run_cli_args_with_state_lists_switches_then_lists_changed_current_provider() {
        let _guard = settings_test_guard();
        let _home = TempHome::new();

        let db = Arc::new(Database::memory().expect("create memory db"));
        let state = AppState::new(db.clone());
        db.save_provider("claude", &claude_provider("p1", "Claude One", "key-one"))
            .expect("save p1");
        db.save_provider("claude", &claude_provider("p2", "Claude Two", "key-two"))
            .expect("save p2");
        db.set_current_provider("claude", "p1")
            .expect("set db current provider");
        crate::settings::set_current_provider(&AppType::Claude, Some("p1"))
            .expect("set local current provider");

        let first_list = run_cli_args_with_state(
            &state,
            vec![
                "cc-switch".to_string(),
                "list".to_string(),
                "claude".to_string(),
            ],
        )
        .expect("first list should be handled by cli");

        assert_eq!(first_list.stderr, "");
        assert_eq!(first_list.exit_code, 0);
        assert_eq!(
            first_list.stdout,
            "claude\n* p1  Claude One\n  p2  Claude Two\n"
        );

        let switch = run_cli_args_with_state(
            &state,
            vec![
                "cc-switch".to_string(),
                "switch".to_string(),
                "claude".to_string(),
                "p2".to_string(),
            ],
        )
        .expect("switch should be handled by cli");

        assert_eq!(
            switch,
            CliOutput {
                stdout: "Switched claude to p2\n".to_string(),
                stderr: String::new(),
                exit_code: 0,
            }
        );

        let second_list = run_cli_args_with_state(
            &state,
            vec![
                "cc-switch".to_string(),
                "list".to_string(),
                "claude".to_string(),
            ],
        )
        .expect("second list should be handled by cli");

        assert_eq!(second_list.stderr, "");
        assert_eq!(second_list.exit_code, 0);
        assert_eq!(
            second_list.stdout,
            "claude\n  p1  Claude One\n* p2  Claude Two\n"
        );
    }

    #[test]
    fn text_output_escapes_terminal_control_characters() {
        let records = vec![CliProviderRecord {
            app: "claude".to_string(),
            id: "bad\u{1b}[31m".to_string(),
            name: "Name\u{7}Hidden\nNext".to_string(),
            current: true,
        }];

        let output = format_list_text(&records);

        assert!(!output.contains('\u{1b}'));
        assert!(!output.contains('\u{7}'));
        assert!(!output.contains("\nNext"));
        assert_eq!(output, "claude\n* bad?[31m  Name?Hidden?Next\n");
    }

    #[test]
    fn text_error_output_escapes_terminal_control_characters() {
        let output = error_output(
            &CliError {
                code: "provider_not_found",
                message: "Provider not found: bad\u{1b}]52;c;secret\u{7}".to_string(),
                exit_code: 1,
                json: false,
            },
            false,
        );

        assert_eq!(output.stdout, "");
        assert_eq!(output.stderr, "Provider not found: bad?]52;c;secret?\n");
        assert_eq!(output.exit_code, 1);
    }

    #[test]
    fn json_error_output_preserves_escaped_content() {
        let output = error_output(
            &CliError {
                code: "provider_not_found",
                message: "Provider not found: bad\u{1b}".to_string(),
                exit_code: 1,
                json: true,
            },
            true,
        );

        assert!(output.stderr.contains("\\u001b"));
    }

    #[test]
    fn parse_returns_not_cli_for_gui_launch_without_subcommand() {
        assert_eq!(parse(&["cc-switch"]), CliParseOutcome::NotCli);
    }

    #[test]
    fn parse_returns_not_cli_for_deeplink_argument() {
        assert_eq!(
            parse(&["cc-switch", "ccswitch://provider/import?token=redacted"]),
            CliParseOutcome::NotCli
        );
    }

    #[test]
    fn parse_list_without_app() {
        assert_eq!(
            parse(&["cc-switch", "list"]),
            CliParseOutcome::Command(CliCommand::List {
                app: None,
                json: false,
            })
        );
    }

    #[test]
    fn parse_list_with_app_and_json_flag() {
        assert_eq!(
            parse(&["cc-switch", "list", "claude", "--json"]),
            CliParseOutcome::Command(CliCommand::List {
                app: Some(AppType::Claude),
                json: true,
            })
        );
    }

    #[test]
    fn parse_list_with_json_before_app() {
        assert_eq!(
            parse(&["cc-switch", "list", "--json", "codex"]),
            CliParseOutcome::Command(CliCommand::List {
                app: Some(AppType::Codex),
                json: true,
            })
        );
    }

    #[test]
    fn parse_switch_with_provider_id() {
        assert_eq!(
            parse(&["cc-switch", "switch", "gemini", "provider-1"]),
            CliParseOutcome::Command(CliCommand::Switch {
                app: AppType::Gemini,
                provider_id: "provider-1".to_string(),
                json: false,
            })
        );
    }

    #[test]
    fn parse_switch_with_json_flag() {
        assert_eq!(
            parse(&["cc-switch", "switch", "--json", "opencode", "provider-2"]),
            CliParseOutcome::Command(CliCommand::Switch {
                app: AppType::OpenCode,
                provider_id: "provider-2".to_string(),
                json: true,
            })
        );
    }

    #[test]
    fn parse_update_key_with_key() {
        assert_eq!(
            parse(&["cc-switch", "update-key", "claude", "provider-1", "sk-new"]),
            CliParseOutcome::Command(CliCommand::UpdateKey {
                app: AppType::Claude,
                provider_id: "provider-1".to_string(),
                key: "sk-new".to_string(),
                json: false,
            })
        );
    }

    #[test]
    fn parse_update_key_with_json_flag() {
        assert_eq!(
            parse(&[
                "cc-switch",
                "update-key",
                "--json",
                "gemini",
                "provider-1",
                "AIza-new",
            ]),
            CliParseOutcome::Command(CliCommand::UpdateKey {
                app: AppType::Gemini,
                provider_id: "provider-1".to_string(),
                key: "AIza-new".to_string(),
                json: true,
            })
        );
    }

    #[test]
    fn parse_update_key_missing_key_is_error() {
        assert_eq!(
            parse(&["cc-switch", "update-key", "claude", "provider-1"]),
            CliParseOutcome::Error(CliError {
                code: "usage",
                message: "Usage: cc-switch update-key <app> <provider-id> <key> [--json]"
                    .to_string(),
                exit_code: 2,
                json: false,
            })
        );
    }

    #[test]
    fn parse_unknown_cli_like_command_is_error() {
        assert_eq!(
            parse(&["cc-switch", "providers"]),
            CliParseOutcome::Error(CliError {
                code: "unknown_command",
                message: "Unknown command: providers".to_string(),
                exit_code: 2,
                json: false,
            })
        );
    }

    #[test]
    fn parse_switch_missing_provider_is_error() {
        assert_eq!(
            parse(&["cc-switch", "switch", "claude"]),
            CliParseOutcome::Error(CliError {
                code: "usage",
                message: "Usage: cc-switch switch <app> <provider-id> [--json]".to_string(),
                exit_code: 2,
                json: false,
            })
        );
    }

    #[test]
    fn parse_list_rejects_second_app_argument() {
        assert_eq!(
            parse(&["cc-switch", "list", "claude", "codex"]),
            CliParseOutcome::Error(CliError {
                code: "usage",
                message: "Usage: cc-switch list [app] [--json]".to_string(),
                exit_code: 2,
                json: false,
            })
        );
    }

    #[test]
    fn parse_error_carries_json_flag_when_json_requested() {
        let result = parse(&["cc-switch", "switch", "claude", "--json"]);
        match result {
            CliParseOutcome::Error(err) => {
                assert_eq!(err.code, "usage");
                assert!(err.json);
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_unknown_command_with_json_carries_json_flag() {
        let result = parse(&["cc-switch", "bogus", "--json"]);
        match result {
            CliParseOutcome::Error(err) => {
                assert_eq!(err.code, "unknown_command");
                assert!(err.json);
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_unrecognized_flag_shows_combined_usage() {
        let result = parse(&["cc-switch", "--foo"]);
        match result {
            CliParseOutcome::Error(err) => {
                assert_eq!(err.code, "usage");
                assert!(err.message.contains("list"));
                assert!(err.message.contains("switch"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn error_output_uses_json_when_error_has_json_flag() {
        let output = error_output(
            &CliError {
                code: "usage",
                message: "bad input".to_string(),
                exit_code: 2,
                json: true,
            },
            true,
        );

        let parsed: serde_json::Value =
            serde_json::from_str(output.stderr.trim()).expect("stderr should be valid json");
        assert_eq!(parsed["ok"], false);
        assert_eq!(parsed["code"], "usage");
    }

    #[test]
    fn cli_ipc_response_round_trips_output() {
        let response = CliIpcResponse {
            handled: true,
            stdout: "Switched claude to p2\n".to_string(),
            stderr: String::new(),
            exit_code: 0,
        };

        let encoded = serde_json::to_string(&response).expect("serialize response");
        let decoded: CliIpcResponse = serde_json::from_str(&encoded).expect("parse response");

        assert!(decoded.handled);
        assert_eq!(decoded.stdout, "Switched claude to p2\n");
        assert_eq!(decoded.stderr, "");
        assert_eq!(decoded.exit_code, 0);
    }

    #[test]
    fn cli_ipc_request_carries_raw_argv() {
        let request = CliIpcRequest {
            args: vec![
                "cc-switch".to_string(),
                "switch".to_string(),
                "claude".to_string(),
                "p2".to_string(),
            ],
        };

        let encoded = serde_json::to_string(&request).expect("serialize request");
        let decoded: CliIpcRequest = serde_json::from_str(&encoded).expect("parse request");

        assert_eq!(
            parse_cli_args(decoded.args),
            CliParseOutcome::Command(CliCommand::Switch {
                app: AppType::Claude,
                provider_id: "p2".to_string(),
                json: false,
            })
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[serial]
    fn run_if_cli_args_does_not_replay_command_after_gui_ipc_bad_response() {
        use std::io::{BufRead as _, Write as _};
        use std::os::unix::net::UnixListener;

        let _guard = settings_test_guard();
        let _home = TempHome::new();
        let runtime_dir = TempDir::new().expect("create runtime dir");
        let original_runtime_dir = env::var("XDG_RUNTIME_DIR").ok();
        env::set_var("XDG_RUNTIME_DIR", runtime_dir.path());

        let socket_path = cli_ipc_socket_path();
        std::fs::create_dir_all(socket_path.parent().expect("socket parent"))
            .expect("create socket parent");
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).expect("bind dummy ipc socket");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept cli connection");
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
            let mut line = String::new();
            reader.read_line(&mut line).expect("read cli request");
            stream
                .write_all(b"not-json\n")
                .expect("write malformed ipc response");
        });

        {
            let db = Database::init().expect("create persistent test db");
            db.save_provider("claude", &claude_provider("p1", "Claude One", "old-key"))
                .expect("save provider");
        }

        let exit_code = run_if_cli_args(vec![
            "cc-switch".to_string(),
            "update-key".to_string(),
            "claude".to_string(),
            "p1".to_string(),
            "new-key".to_string(),
        ])
        .expect("cli args should be handled");
        handle.join().expect("dummy ipc server should finish");

        let db = Database::init().expect("reopen persistent test db");
        let updated = db
            .get_provider_by_id("p1", "claude")
            .expect("read provider")
            .expect("provider exists");
        assert_eq!(exit_code, 1);
        assert_eq!(provider_api_key(&updated), Some("old-key"));

        match original_runtime_dir {
            Some(value) => env::set_var("XDG_RUNTIME_DIR", value),
            None => env::remove_var("XDG_RUNTIME_DIR"),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[serial]
    fn cli_ipc_socket_path_uses_xdg_runtime_dir() {
        let _guard = settings_test_guard();
        let dir = TempDir::new().expect("create runtime dir");
        let original_runtime_dir = env::var("XDG_RUNTIME_DIR").ok();
        env::set_var("XDG_RUNTIME_DIR", dir.path());

        let path = cli_ipc_socket_path();

        assert_eq!(path, dir.path().join(CLI_IPC_DIR).join(CLI_IPC_SOCKET_FILE));

        match original_runtime_dir {
            Some(value) => env::set_var("XDG_RUNTIME_DIR", value),
            None => env::remove_var("XDG_RUNTIME_DIR"),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[serial]
    fn cli_ipc_socket_path_falls_back_to_home_runtime_dir() {
        let _guard = settings_test_guard();
        let home = TempHome::new();
        let original_runtime_dir = env::var("XDG_RUNTIME_DIR").ok();
        env::remove_var("XDG_RUNTIME_DIR");

        let path = cli_ipc_socket_path();

        assert!(path.ends_with(format!("{CLI_IPC_DIR}/{CLI_IPC_SOCKET_FILE}")));
        assert!(path.starts_with(home._dir.path().join(".cc-switch").join("runtime")));

        match original_runtime_dir {
            Some(value) => env::set_var("XDG_RUNTIME_DIR", value),
            None => env::remove_var("XDG_RUNTIME_DIR"),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[serial]
    fn cli_ipc_socket_path_ignores_relative_xdg_runtime_dir() {
        let _guard = settings_test_guard();
        let home = TempHome::new();
        let original_runtime_dir = env::var("XDG_RUNTIME_DIR").ok();
        env::set_var("XDG_RUNTIME_DIR", "relative-runtime");

        let path = cli_ipc_socket_path();

        assert!(path.starts_with(home._dir.path().join(".cc-switch").join("runtime")));

        match original_runtime_dir {
            Some(value) => env::set_var("XDG_RUNTIME_DIR", value),
            None => env::remove_var("XDG_RUNTIME_DIR"),
        }
    }
}
