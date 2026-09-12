//! Runtime credential selection is scoped to one profile and one launch.
use crate::shared_credentials::{Provider, SharedCredentials};
use async_trait::async_trait;
use futures::stream::BoxStream;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use zeron_harness::{
    Harness, HarnessError, RunControls,
    runtime_auth::{self, RuntimeAuth},
};
use zeron_proto::{
    AgentEvent, HarnessId, Model, ReasoningLevel, RunRequest, SlashCommand, SteeringMode,
};

pub struct AuthenticatedHarness {
    pub inner: Arc<dyn Harness>,
    pub credentials: SharedCredentials,
    pub root: PathBuf,
}
impl AuthenticatedHarness {
    async fn context(&self) -> Result<Option<Arc<RuntimeAuth>>, HarnessError> {
        let Some(account) = self
            .credentials
            .active(self.id())
            .await
            .map_err(|e| HarnessError::Protocol(e.to_string()))?
        else {
            return Ok(None);
        };
        let id = account
            .id
            .strip_prefix("shared:")
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .ok_or_else(|| HarnessError::Protocol("Invalid shared account identity".into()))?;
        let root = self.root.join(id.to_string());
        std::fs::create_dir_all(&root)?;
        #[cfg(unix)]
        std::fs::set_permissions(&root, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
        let mut env = vec![(
            account.provider.environment().into(),
            account.secret.expose().into(),
        )];
        let mut arguments = Vec::new();
        let path = |p: &Path| p.to_string_lossy().into_owned();
        let provider = match account.provider {
            Provider::Anthropic => "anthropic",
            Provider::Openai => "openai",
            Provider::Cursor => "cursor",
            Provider::Devin => "devin",
            Provider::Xai => "xai",
            Provider::Openrouter => "openrouter",
        };
        match self.id() {
            HarnessId::ClaudeCode => {
                env.push(("CLAUDE_CONFIG_DIR".into(), path(&root)));
                env.push((
                    "ANTHROPIC_BASE_URL".into(),
                    "https://api.anthropic.com".into(),
                ));
            }
            HarnessId::Codex => {
                env.push(("CODEX_HOME".into(), path(&root)));
                // CLI overrides take precedence over trusted project settings.
                for setting in [
                    "model_provider=\"zeron-shared\"",
                    "model_providers.zeron-shared.base_url=\"https://api.openai.com/v1\"",
                    "model_providers.zeron-shared.env_key=\"OPENAI_API_KEY\"",
                    "model_providers.zeron-shared.wire_api=\"responses\"",
                    "model_providers.zeron-shared.requires_openai_auth=false",
                ] {
                    arguments.extend(["-c".into(), setting.into()]);
                }
                write_config(&root.join("config.toml"), b"model_provider = \"zeron-shared\"\n[model_providers.zeron-shared]\nname = \"OpenAI\"\nbase_url = \"https://api.openai.com/v1\"\nenv_key = \"OPENAI_API_KEY\"\nwire_api = \"responses\"\nrequires_openai_auth = false\n")?;
            }
            HarnessId::Grok => {
                env.push(("GROK_HOME".into(), path(&root)));
                // Keep device policy in force in the managed profile. These
                // links contain no newly copied secrets and are never synced.
                let native = std::env::var_os("GROK_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| crate::repos::home_dir().join(".grok"));
                for name in ["requirements.toml", "managed_config.toml"] {
                    policy_link(&native.join(name), &root.join(name))?;
                }
                write_config(&root.join("config.toml"), b"[model.grok-build]\nenv_key = \"XAI_API_KEY\"\n[models]\ndefault = \"grok-build\"\n")?;
            }
            HarnessId::Hermes => {
                env.push(("HERMES_HOME".into(), path(&root)));
                let provider = if account.provider == Provider::Openai {
                    "openai-api"
                } else {
                    provider
                };
                write_config(
                    &root.join("config.yaml"),
                    format!("model:\n  provider: {provider}\n").as_bytes(),
                )?;
            }
            HarnessId::Pi => {
                env.push(("PI_CODING_AGENT_DIR".into(), path(&root)));
                write_config(
                    &root.join("settings.json"),
                    serde_json::json!({"defaultProvider": provider})
                        .to_string()
                        .as_bytes(),
                )?;
            }
            HarnessId::Opencode => {
                env.push(("XDG_DATA_HOME".into(), path(&root)));
                let base_url = match account.provider {
                    Provider::Anthropic => "https://api.anthropic.com/v1",
                    Provider::Openai => "https://api.openai.com/v1",
                    Provider::Xai => "https://api.x.ai/v1",
                    _ => "https://openrouter.ai/api/v1",
                };
                env.push(("OPENCODE_CONFIG_CONTENT".into(), serde_json::json!({"provider": {provider: {"options": {"apiKey": format!("{{env:{}}}", account.provider.environment()), "baseURL": base_url}}}}).to_string()));
            }
            HarnessId::Cursor | HarnessId::Devin => {}
            HarnessId::Mock => {
                return Err(HarnessError::Protocol("Mock has no shared accounts".into()));
            }
        }
        Ok(Some(Arc::new(RuntimeAuth::new(env, arguments))))
    }
}

fn write_config(path: &Path, contents: &[u8]) -> Result<(), HarnessError> {
    // Config contains provider names and env references only, never credentials.
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    file.write_all(contents)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| HarnessError::Io(e.error))?;
    Ok(())
}
fn policy_link(source: &Path, target: &Path) -> Result<(), HarnessError> {
    if !source.exists() || target.symlink_metadata().is_ok() {
        return Ok(());
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(source, target)?;
    #[cfg(not(unix))]
    {
        std::fs::copy(source, target)?;
    }
    Ok(())
}

#[async_trait]
impl Harness for AuthenticatedHarness {
    fn id(&self) -> HarnessId {
        self.inner.id()
    }
    fn display_name(&self) -> &str {
        self.inner.display_name()
    }
    fn supports_steering(&self) -> bool {
        self.inner.supports_steering()
    }
    fn steering_mode(&self) -> SteeringMode {
        self.inner.steering_mode()
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        self.inner.reasoning_levels()
    }
    fn installed(&self) -> bool {
        self.inner.installed()
    }
    fn deterministic_turn_end(&self) -> bool {
        self.inner.deterministic_turn_end()
    }
    fn authoritative_prompt_end(&self) -> bool {
        self.inner.authoritative_prompt_end()
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        runtime_auth::scope(self.context().await?, self.inner.models()).await
    }
    async fn commands(&self) -> Result<Vec<SlashCommand>, HarnessError> {
        runtime_auth::scope(self.context().await?, self.inner.commands()).await
    }
    async fn run_title(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        runtime_auth::scope(
            self.context().await?,
            self.inner.run_title(request, controls),
        )
        .await
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        runtime_auth::scope(self.context().await?, self.inner.run(request, controls)).await
    }
}
