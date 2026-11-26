//! Interactive provider setup and environment-driven route initialization.

use super::{
    Failure, HostSecret, InitArgs, InitProvider, open_local_engine, resolve_user_data_dir,
};
use coda::credential_input::{CredentialInputError, CredentialInputMode, read_credential};
use coda::domain::Route;
use coda::engine::Engine;
use serde_json::json;
use std::collections::HashMap;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_MODEL: &str = "gpt-4.1";

fn prompt_line_with_default(label: &str, default_val: &str) -> Result<String, Failure> {
    eprint!("\x1b[1m{label}\x1b[0m [{default_val}]: ");
    let _ = io::stderr().flush();
    let mut buf = String::new();
    io::stdin()
        .read_line(&mut buf)
        .map_err(|e| Failure::custom("input_error", format!("读取输入失败: {e}")))?;
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        Ok(default_val.to_owned())
    } else {
        Ok(trimmed.to_owned())
    }
}

fn read_secret_from_stdin_line() -> Result<String, Failure> {
    let mut line = String::new();
    let mut reader = io::BufReader::new(io::stdin());
    reader.read_line(&mut line).map_err(|e| {
        Failure::custom(
            "credential_invalid",
            format!("读取 stdin API Key 失败: {e}"),
        )
    })?;
    let key = line.trim_end_matches(['\r', '\n']).trim().to_owned();
    if key.is_empty() || key.len() >= 4096 || key.chars().any(char::is_control) {
        return Err(Failure::fixed(
            "credential_invalid",
            "API Key 为空或包含无效控制字符",
            false,
        ));
    }
    Ok(key)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProviderPreset {
    provider: InitProvider,
    display_name: &'static str,
    cli_flag: &'static str,
    key_env: &'static str,
    base_env: &'static str,
    default_base_url: &'static str,
    default_model: &'static str,
}

impl InitProvider {
    fn preset(self) -> ProviderPreset {
        match self {
            Self::Openai => ProviderPreset {
                provider: Self::Openai,
                display_name: "OpenAI",
                cli_flag: "openai",
                key_env: "OPENAI_API_KEY",
                base_env: "OPENAI_BASE_URL",
                default_base_url: DEFAULT_BASE_URL,
                default_model: DEFAULT_MODEL,
            },
            Self::Anthropic => ProviderPreset {
                provider: Self::Anthropic,
                display_name: "Anthropic",
                cli_flag: "anthropic",
                key_env: "ANTHROPIC_API_KEY",
                base_env: "ANTHROPIC_BASE_URL",
                default_base_url: "https://api.anthropic.com/v1",
                default_model: "claude-sonnet-4-20250514",
            },
            Self::Gemini => ProviderPreset {
                provider: Self::Gemini,
                display_name: "Gemini",
                cli_flag: "gemini",
                key_env: "GEMINI_API_KEY",
                base_env: "GEMINI_BASE_URL",
                default_base_url: "https://generativelanguage.googleapis.com/v1beta",
                default_model: "gemini-2.5-pro",
            },
        }
    }

    fn from_protocol(protocol: coda::provider::ProviderProtocol) -> Self {
        match protocol {
            coda::provider::ProviderProtocol::OpenAi => Self::Openai,
            coda::provider::ProviderProtocol::Anthropic => Self::Anthropic,
            coda::provider::ProviderProtocol::Gemini => Self::Gemini,
        }
    }
}

fn non_empty_env_with<F>(env_fn: &F, name: &str) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    env_fn(name)
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn resolve_init_provider<F>(
    explicit: Option<InitProvider>,
    base_url: Option<&str>,
    env_fn: F,
) -> InitProvider
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(provider) = explicit {
        return provider;
    }
    if let Some(url) = base_url.map(str::trim).filter(|s| !s.is_empty()) {
        return InitProvider::from_protocol(coda::provider::ProviderProtocol::detect(url));
    }
    [
        InitProvider::Openai,
        InitProvider::Anthropic,
        InitProvider::Gemini,
    ]
    .into_iter()
    .find(|&candidate| non_empty_env_with(&env_fn, candidate.preset().key_env).is_some())
    .unwrap_or(InitProvider::Openai)
}

struct ZeroInitPlan {
    preset: ProviderPreset,
    base_url: String,
    model: String,
    api_key: HostSecret,
}

fn resolve_zero_init_plan<F>(
    target_id: &str,
    requested_route: Option<&str>,
    model_override: Option<&str>,
    env_fn: F,
) -> Result<ZeroInitPlan, Failure>
where
    F: Fn(&str) -> Option<String>,
{
    let explicit_provider = match requested_route {
        Some(r) if r.eq_ignore_ascii_case("anthropic") => Some(InitProvider::Anthropic),
        Some(r) if r.eq_ignore_ascii_case("gemini") => Some(InitProvider::Gemini),
        Some(r) if r.eq_ignore_ascii_case("openai") => Some(InitProvider::Openai),
        _ => None,
    };

    let model_hint = model_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| non_empty_env_with(&env_fn, "CODA_MODEL"));

    let hint_provider = model_hint.as_deref().and_then(|hint| {
        let lower = hint.to_ascii_lowercase();
        if lower.starts_with("claude") && non_empty_env_with(&env_fn, "ANTHROPIC_API_KEY").is_some()
        {
            Some(InitProvider::Anthropic)
        } else if (lower.starts_with("gemini") || lower.starts_with("models/gemini"))
            && non_empty_env_with(&env_fn, "GEMINI_API_KEY").is_some()
        {
            Some(InitProvider::Gemini)
        } else if non_empty_env_with(&env_fn, "OPENAI_BASE_URL").is_some()
            && non_empty_env_with(&env_fn, "OPENAI_API_KEY").is_some()
        {
            Some(InitProvider::Openai)
        } else {
            None
        }
    });

    let fallback_by_env = || {
        [
            InitProvider::Openai,
            InitProvider::Anthropic,
            InitProvider::Gemini,
        ]
        .into_iter()
        .find(|&candidate| non_empty_env_with(&env_fn, candidate.preset().key_env).is_some())
    };

    let Some(chosen_provider) = explicit_provider.or(hint_provider).or_else(fallback_by_env) else {
        return Err(Failure::custom(
            "route_not_configured",
            format!(
                "未找到模型路由 `{target_id}`；请先运行 `coda init` 或设置环境变量 `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` / `GEMINI_API_KEY`"
            ),
        ));
    };

    let preset = chosen_provider.preset();
    let Some(api_key) = non_empty_env_with(&env_fn, preset.key_env) else {
        return Err(Failure::custom(
            "route_not_configured",
            format!(
                "未找到模型路由 `{target_id}` 的有效凭据（`{}` 未设置或为空）；请运行 `coda init --provider {}` 或设置环境变量 `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` / `GEMINI_API_KEY`",
                preset.key_env, preset.cli_flag
            ),
        ));
    };

    let base_url = non_empty_env_with(&env_fn, preset.base_env)
        .unwrap_or_else(|| preset.default_base_url.to_owned());
    let model = model_hint.unwrap_or_else(|| preset.default_model.to_owned());

    Ok(ZeroInitPlan {
        preset,
        base_url,
        model,
        api_key: HostSecret(api_key),
    })
}

pub(super) async fn run_init(
    instance_lock: &mut Option<std::fs::File>,
    top_data_dir: Option<PathBuf>,
    json_output: bool,
    args: InitArgs,
) -> Result<(), Failure> {
    let data_dir = resolve_user_data_dir(args.data_dir.or(top_data_dir));
    let interactive = !args.api_key_stdin && io::stdin().is_terminal() && !json_output;

    let provider = resolve_init_provider(args.provider, args.base_url.as_deref(), |k| {
        std::env::var(k).ok()
    });
    let preset = provider.preset();

    let env_base = std::env::var(preset.base_env)
        .ok()
        .filter(|s| !s.trim().is_empty());
    let env_model = std::env::var("CODA_MODEL")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let env_key = std::env::var(preset.key_env)
        .ok()
        .filter(|s| !s.trim().is_empty());

    let raw_base_url = match args.base_url {
        Some(url) => url,
        None if interactive => {
            let def = env_base.as_deref().unwrap_or(preset.default_base_url);
            prompt_line_with_default(&format!("{} API Base URL", preset.display_name), def)?
        }
        None => env_base.unwrap_or_else(|| preset.default_base_url.to_owned()),
    };

    let raw_model = match args.model {
        Some(m) => m,
        None if interactive => {
            let def = env_model.as_deref().unwrap_or(preset.default_model);
            prompt_line_with_default("Default Model", def)?
        }
        None => env_model.unwrap_or_else(|| preset.default_model.to_owned()),
    };

    let normalized = coda::host_configuration::RouteInput {
        name: args.name.clone(),
        base_url: raw_base_url,
        model: raw_model,
        max_tokens: args.max_tokens,
        parallel_limit: args.parallel_limit,
    }
    .normalized()
    .map_err(|_| {
        Failure::fixed(
            "invalid_config",
            "路由配置无效（请检查 HTTPS/loopback Base URL 与模型名称）",
            false,
        )
    })?;

    let api_key = HostSecret(if args.api_key_stdin {
        read_secret_from_stdin_line()?
    } else if let Some(key) = env_key.clone().filter(|_| !interactive) {
        key
    } else if interactive {
        if env_key.is_some() {
            eprintln!(
                "提示: 检测到环境变量 {}，请在隐藏提示中输入要持久保存的 API Key:",
                preset.key_env
            );
        }
        read_credential(CredentialInputMode::Prompt)
            .map_err(|err| match err {
                CredentialInputError::Cancelled => {
                    Failure::fixed("credential_cancelled", "API Key 输入已取消", false)
                }
                CredentialInputError::Unavailable => Failure::custom(
                    "credential_unavailable",
                    format!(
                        "无法打开安全终端输入；可使用 --api-key-stdin 或设置 {}",
                        preset.key_env
                    ),
                ),
                CredentialInputError::Invalid => {
                    Failure::fixed("credential_invalid", "API Key 输入无效", false)
                }
            })?
            .into_string()
    } else if let Some(key) = env_key {
        key
    } else {
        return Err(Failure::custom(
            "credential_required",
            format!(
                "需要提供 API Key：请在终端交互运行 coda init、传入 --api-key-stdin 或设置 {}（支持 OPENAI_API_KEY / ANTHROPIC_API_KEY / GEMINI_API_KEY）",
                preset.key_env
            ),
        ));
    });

    let engine = open_local_engine(instance_lock, &data_dir)?;
    let result = async {
        let mut settings = engine
            .store
            .settings()
            .map_err(|e| Failure::custom("storage_error", e.to_string()))?
            .ok_or_else(|| Failure::fixed("storage_error", "缺少初始设置", false))?;

        let route = Route {
            id: args.route.clone(),
            name: normalized.name,
            base_url: normalized.base_url,
            model: normalized.model,
            max_tokens: normalized.max_tokens,
            parallel_limit: normalized.parallel_limit,
            key_env: None,
        };

        if let Some(existing) = settings.routes.iter_mut().find(|r| r.id == route.id) {
            *existing = route.clone();
        } else {
            settings.routes.push(route.clone());
        }

        engine
            .configure_with_keys(
                settings,
                HashMap::from([(route.id.clone(), api_key.as_str().to_owned())]),
            )
            .await
            .map_err(|e| Failure::custom("config_error", format!("保存路由配置失败: {e}")))?;

        if json_output {
            println!(
                "{}",
                json!({
                    "ok": true,
                    "data_dir": data_dir.to_string_lossy(),
                    "route_id": route.id,
                    "base_url": route.base_url,
                    "model": route.model,
                    "max_tokens": route.max_tokens,
                })
            );
        } else {
            println!(
                "\x1b[32m✓\x1b[0m Coda CLI 初始化完成 ({})!\n  数据目录: {}\n  默认路由: {} ({})\n  模型名称: {}\n\n现在可以在任意代码目录直接运行 `\x1b[1mcoda chat\x1b[0m` 或 `\x1b[1mcoda ask \"...\"\x1b[0m`!",
                preset.display_name,
                data_dir.display(),
                route.id,
                route.base_url,
                route.model
            );
        }
        Ok(())
    }
    .await;

    let _ = engine.shutdown(Duration::from_secs(5)).await;
    result
}

pub(super) async fn ensure_route_ready(
    engine: &Arc<Engine>,
    requested_route: Option<&str>,
    model_override: Option<&str>,
) -> Result<Route, Failure> {
    let mut settings = engine
        .store
        .settings()
        .map_err(|e| Failure::custom("storage_error", e.to_string()))?
        .ok_or_else(|| Failure::fixed("storage_error", "缺少系统配置", false))?;

    let target_id = requested_route
        .map(str::to_owned)
        .or_else(|| settings.routes.first().map(|r| r.id.clone()))
        .unwrap_or_else(|| "default".to_owned());

    if !settings.routes.iter().any(|r| r.id == target_id) {
        let plan = resolve_zero_init_plan(&target_id, requested_route, model_override, |k| {
            std::env::var(k).ok()
        })?;

        let normalized = coda::host_configuration::RouteInput {
            name: plan.preset.display_name.into(),
            base_url: plan.base_url,
            model: plan.model,
            max_tokens: 16384,
            parallel_limit: 4,
        }
        .normalized()
        .map_err(|_| {
            Failure::custom(
                "invalid_config",
                format!("环境变量 {} 或 CODA_MODEL 格式无效", plan.preset.base_env),
            )
        })?;

        let route = Route {
            id: target_id.clone(),
            name: normalized.name,
            base_url: normalized.base_url,
            model: normalized.model,
            max_tokens: normalized.max_tokens,
            parallel_limit: normalized.parallel_limit,
            key_env: Some(plan.preset.key_env.into()),
        };
        settings.routes.push(route.clone());
        engine
            .configure_with_keys(
                settings.clone(),
                HashMap::from([(target_id.clone(), plan.api_key.as_str().to_owned())]),
            )
            .await
            .map_err(|e| Failure::custom("config_error", format!("自动配置路由失败: {e}")))?;
        return Ok(route);
    }

    if let Some(new_model) = model_override {
        let current_model = settings
            .routes
            .iter()
            .find(|r| r.id == target_id)
            .map(|r| r.model.as_str())
            .unwrap_or_default();
        if current_model != new_model {
            if let Some(route) = settings.routes.iter_mut().find(|r| r.id == target_id) {
                route.model = new_model.trim().to_owned();
            }
            engine
                .configure(settings.clone())
                .await
                .map_err(|e| Failure::custom("config_error", format!("切换模型失败: {e}")))?;
        }
    }

    settings
        .routes
        .into_iter()
        .find(|r| r.id == target_id)
        .ok_or_else(|| Failure::fixed("route_not_found", "未找到指定模型路由", false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_init_provider_handles_explicit_base_url_and_env_fallbacks() {
        let empty_env = |_: &str| -> Option<String> { None };
        assert_eq!(
            resolve_init_provider(Some(InitProvider::Anthropic), None, empty_env),
            InitProvider::Anthropic
        );
        assert_eq!(
            resolve_init_provider(None, Some("https://api.anthropic.com/v1"), |_| Some(
                "sk-openai".into()
            )),
            InitProvider::Anthropic
        );
        assert_eq!(
            resolve_init_provider(None, Some("http://127.0.0.1:9000/v1beta"), |_| Some(
                "sk-openai".into()
            )),
            InitProvider::Gemini
        );
        assert_eq!(
            resolve_init_provider(None, None, |k| match k {
                "ANTHROPIC_API_KEY" => Some("sk-ant-123".into()),
                "GEMINI_API_KEY" => Some("AIza-123".into()),
                _ => None,
            }),
            InitProvider::Anthropic
        );
        assert_eq!(
            resolve_init_provider(None, None, |k| match k {
                "GEMINI_API_KEY" => Some("AIza-123".into()),
                _ => None,
            }),
            InitProvider::Gemini
        );
        assert_eq!(
            resolve_init_provider(None, None, empty_env),
            InitProvider::Openai
        );
    }

    #[test]
    fn resolve_zero_init_plan_supports_all_three_providers_and_model_hints() {
        // 1. Explicit route selects provider preset
        let ant = resolve_zero_init_plan("anthropic", Some("anthropic"), None, |k| match k {
            "OPENAI_API_KEY" => Some("sk-openai".into()),
            "ANTHROPIC_API_KEY" => Some("sk-ant-key".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(ant.preset.provider, InitProvider::Anthropic);
        assert_eq!(ant.preset.key_env, "ANTHROPIC_API_KEY");
        assert_eq!(ant.base_url, "https://api.anthropic.com/v1");
        assert_eq!(ant.model, "claude-sonnet-4-20250514");
        assert_eq!(ant.api_key.as_str(), "sk-ant-key");

        // 2. Model hint `claude-*` or `models/gemini-*` overrides OPENAI_API_KEY fallback
        let claude_hint = resolve_zero_init_plan(
            "default",
            None,
            Some("Claude-Opus-4-20250514"),
            |k| match k {
                "OPENAI_API_KEY" => Some("sk-openai".into()),
                "ANTHROPIC_API_KEY" => Some("sk-ant-hint".into()),
                "ANTHROPIC_BASE_URL" => Some("http://127.0.0.1:4100/anthropic/v1".into()),
                _ => None,
            },
        )
        .unwrap();
        assert_eq!(claude_hint.preset.provider, InitProvider::Anthropic);
        assert_eq!(claude_hint.base_url, "http://127.0.0.1:4100/anthropic/v1");
        assert_eq!(claude_hint.model, "Claude-Opus-4-20250514");

        let gemini_env_hint = resolve_zero_init_plan("default", None, None, |k| match k {
            "OPENAI_API_KEY" => Some("sk-openai".into()),
            "GEMINI_API_KEY" => Some("AIza-gem".into()),
            "CODA_MODEL" => Some("models/gemini-2.5-flash".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(gemini_env_hint.preset.provider, InitProvider::Gemini);
        assert_eq!(gemini_env_hint.preset.key_env, "GEMINI_API_KEY");
        assert_eq!(
            gemini_env_hint.base_url,
            "https://generativelanguage.googleapis.com/v1beta"
        );
        assert_eq!(gemini_env_hint.model, "models/gemini-2.5-flash");

        // 3. Fallback priority: OPENAI_API_KEY -> ANTHROPIC_API_KEY -> GEMINI_API_KEY
        let fallback_gem = resolve_zero_init_plan("default", None, None, |k| match k {
            "GEMINI_API_KEY" => Some("AIza-only".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(fallback_gem.preset.provider, InitProvider::Gemini);
        assert_eq!(fallback_gem.model, "gemini-2.5-pro");

        // 4. Missing keys produce actionable error mentioning coda init and all three env vars
        let err = resolve_zero_init_plan("default", None, None, |_| None)
            .err()
            .unwrap();
        assert_eq!(err.code, "route_not_configured");
        assert!(err.message.contains("coda init"));
        assert!(err.message.contains("OPENAI_API_KEY"));
        assert!(err.message.contains("ANTHROPIC_API_KEY"));
        assert!(err.message.contains("GEMINI_API_KEY"));
    }
}
