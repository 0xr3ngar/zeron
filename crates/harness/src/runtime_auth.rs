//! Credentials exist only in the launch context, never in serialized RunRequest.
use std::{future::Future, sync::Arc};
use tokio::process::Command;

pub struct RuntimeAuth {
    environment: Vec<(String, String)>,
    arguments: Vec<String>,
}
impl std::fmt::Debug for RuntimeAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RuntimeAuth([REDACTED])")
    }
}
impl Drop for RuntimeAuth {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        for (_, value) in &mut self.environment {
            value.zeroize();
        }
    }
}
impl RuntimeAuth {
    pub fn new(environment: Vec<(String, String)>, arguments: Vec<String>) -> Self {
        Self {
            environment,
            arguments,
        }
    }
    pub fn apply(&self, command: &mut Command) {
        // A selected shared account must not inherit an unrelated provider key.
        for key in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "OPENAI_API_KEY",
            "CURSOR_API_KEY",
            "WINDSURF_API_KEY",
            "XAI_API_KEY",
            "OPENROUTER_API_KEY",
            "GROK_AUTH_PROVIDER_ACCESS_TOKEN",
            "GROK_AUTH_PROVIDER_REFRESH_TOKEN",
            "GROK_AUTH_PROVIDER_COMMAND",
            "OPENAI_BASE_URL",
            "OPENAI_API_BASE",
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_API_URL",
            "OPENROUTER_BASE_URL",
            "XAI_BASE_URL",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
        ] {
            command.env_remove(key);
        }
        command.envs(self.environment.iter().map(|(k, v)| (k, v)));
        command.args(&self.arguments);
    }
}
tokio::task_local! { static AUTH: Option<Arc<RuntimeAuth>>; }
pub async fn scope<T>(auth: Option<Arc<RuntimeAuth>>, future: impl Future<Output = T>) -> T {
    AUTH.scope(auth, future).await
}
pub fn is_active() -> bool {
    AUTH.try_with(|auth| auth.is_some()).unwrap_or(false)
}
pub fn apply(command: &mut Command) {
    let _ = AUTH.try_with(|auth| {
        if let Some(auth) = auth {
            auth.apply(command);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn simultaneous_launch_contexts_are_isolated_and_do_not_modify_the_process() {
        let original = std::env::var("OPENAI_API_KEY").ok();
        let launch = |key: &'static str| async move {
            let auth = Arc::new(RuntimeAuth::new(
                vec![("OPENAI_API_KEY".into(), key.into())],
                vec![],
            ));
            assert!(!format!("{auth:?}").contains(key));
            scope(Some(auth), async {
                tokio::task::yield_now().await;
                let mut command = Command::new("unused-test-program");
                apply(&mut command);
                assert_eq!(
                    command
                        .as_std()
                        .get_envs()
                        .find(|(k, _)| *k == "OPENAI_API_KEY")
                        .unwrap()
                        .1
                        .unwrap(),
                    key
                );
                assert_eq!(
                    command
                        .as_std()
                        .get_envs()
                        .find(|(k, _)| *k == "CLAUDE_CODE_OAUTH_TOKEN")
                        .unwrap()
                        .1,
                    None
                );
            })
            .await;
        };
        tokio::join!(
            launch("first-secret-canary"),
            launch("second-secret-canary")
        );
        assert_eq!(std::env::var("OPENAI_API_KEY").ok(), original);
        let mut command = Command::new("unused-test-program");
        apply(&mut command);
        assert_eq!(command.as_std().get_envs().count(), 0);
    }
}
