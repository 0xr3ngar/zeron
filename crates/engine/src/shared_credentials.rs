//! Account-scoped credential documents, sealed using the common device vault.
//! Only explicit enrollment imports secrets. Native CLI homes are never replicated.
use crate::{
    EngineError,
    vault::{
        VaultService,
        client::{VaultClient, decode_base64, encode_base64},
        object_id_for,
    },
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use zeroize::Zeroizing;
use zeron_crypto::{
    content::{self, ContentPurpose},
    record::UnverifiedRecord,
};
use zeron_proto::{AgentAccount, AgentAuthKind, HarnessId};

const MAX_BYTES: usize = 900_000;
fn error(message: &str) -> EngineError {
    EngineError::Other(message.into())
}

#[derive(Clone, Serialize)]
#[serde(transparent)]
pub struct Secret(String);
impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Secret::new(value).map_err(|_| serde::de::Error::custom("invalid credential"))
    }
}
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}
impl Drop for Secret {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.0.zeroize();
    }
}
impl Secret {
    pub fn new(value: String) -> Result<Self, EngineError> {
        if !value.is_ascii()
            || value.is_empty()
            || value.len() > 16_384
            || value.chars().any(char::is_control)
        {
            return Err(error("Enter a valid API key."));
        }
        Ok(Self(value))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    Anthropic,
    Openai,
    Cursor,
    Devin,
    Xai,
    Openrouter,
}
impl Provider {
    pub fn environment(self) -> &'static str {
        match self {
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::Openai => "OPENAI_API_KEY",
            Self::Cursor => "CURSOR_API_KEY",
            Self::Devin => "WINDSURF_API_KEY",
            Self::Xai => "XAI_API_KEY",
            Self::Openrouter => "OPENROUTER_API_KEY",
        }
    }
    pub fn supports(self, harness: HarnessId) -> bool {
        match harness {
            HarnessId::ClaudeCode => self == Self::Anthropic,
            HarnessId::Codex => self == Self::Openai,
            HarnessId::Cursor => self == Self::Cursor,
            HarnessId::Devin => self == Self::Devin,
            HarnessId::Grok => self == Self::Xai,
            HarnessId::Hermes | HarnessId::Pi | HarnessId::Opencode => matches!(
                self,
                Self::Anthropic | Self::Openai | Self::Xai | Self::Openrouter
            ),
            HarnessId::Mock => false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedAccount {
    pub id: String,
    pub harness: HarnessId,
    pub provider: Provider,
    pub label: String,
    pub secret: Secret,
    pub saved_at: i64,
    #[serde(default)]
    pub expires_at: Option<i64>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Document {
    version: u32,
    revision: u64,
    accounts: Vec<SharedAccount>,
    active: std::collections::HashMap<HarnessId, String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CredentialEnvelope {
    pub revision: u64,
    pub record: String,
}

struct Inner {
    vault: VaultService,
    client: Option<VaultClient>,
    ops: tokio::sync::Mutex<()>,
}
#[derive(Clone)]
pub struct SharedCredentials {
    inner: Arc<Inner>,
}
impl SharedCredentials {
    pub fn new(vault: VaultService, client: Option<VaultClient>) -> Self {
        Self {
            inner: Arc::new(Inner {
                vault,
                client,
                ops: tokio::sync::Mutex::new(()),
            }),
        }
    }
    pub fn vault(&self) -> &VaultService {
        &self.inner.vault
    }
    fn client(&self) -> Result<&VaultClient, EngineError> {
        self.inner
            .client
            .as_ref()
            .ok_or_else(|| error("Sign into Zeron to share accounts."))
    }
    async fn open(&self, envelope: &CredentialEnvelope) -> Result<Document, EngineError> {
        if envelope.revision == 0 {
            if !envelope.record.is_empty() {
                return Err(error("Invalid credential revision."));
            }
            return Ok(Document {
                version: 1,
                ..Default::default()
            });
        }
        let bytes =
            decode_base64(&envelope.record).ok_or_else(|| error("Invalid credential encoding."))?;
        let record = UnverifiedRecord::parse(&bytes, MAX_BYTES + 512)
            .map_err(|_| error("Invalid credential record."))?;
        if record.untrusted_revision_id()[..8] != envelope.revision.to_be_bytes() {
            return Err(error("Credential sequence is not authenticated."));
        }
        let material = self
            .vault()
            .open_material(
                object_id_for("credentials", "accounts"),
                record.untrusted_binding(),
            )
            .await
            .map_err(|_| error("Credential keys are unavailable or the author is not trusted."))?;
        let opened = content::open(
            &bytes,
            &material.binding,
            ContentPurpose::Credentials,
            &material.key,
            &material.author_public_key,
            MAX_BYTES,
        )
        .map_err(|_| error("Credentials failed authentication."))?;
        let doc: Document = serde_json::from_slice(opened.plaintext().as_bytes())
            .map_err(|_| error("Unsupported credential data."))?;
        if doc.version != 1 || doc.revision != envelope.revision || doc.accounts.len() > 128 {
            return Err(error("Unsupported credential version or revision."));
        }
        let mut ids = std::collections::HashSet::new();
        for account in &doc.accounts {
            if !account.provider.supports(account.harness)
                || account
                    .id
                    .strip_prefix("shared:")
                    .and_then(|id| uuid::Uuid::parse_str(id).ok())
                    .is_none()
                || !ids.insert(&account.id)
                || account.label.is_empty()
                || account.label.len() > 160
                || account.label.chars().any(char::is_control)
            {
                return Err(error("Invalid shared account."));
            }
        }
        if doc
            .active
            .iter()
            .any(|(h, id)| !doc.accounts.iter().any(|a| a.harness == *h && &a.id == id))
        {
            return Err(error("Invalid active account binding."));
        }
        Ok(doc)
    }
    async fn fetch(&self) -> Result<Document, EngineError> {
        self.vault().refresh().await?;
        if !self.vault().is_ready() {
            return Err(error(
                "Approve this device in Shared accounts before using shared credentials.",
            ));
        }
        let (pin, pending) = self.vault().credential_state();
        if let Some((revision, record)) = pending {
            let queued = CredentialEnvelope { revision, record };
            // A timeout preserves the journal. A competing write is explicit;
            // never silently overwrite an account selection made elsewhere.
            if self.client()?.put_credentials(&queued).await? {
                self.open(&queued).await?;
                self.vault()
                    .save_credential_state(Some((queued.revision, queued.record)), None)?;
            } else {
                self.vault().save_credential_state(pin.clone(), None)?;
                return Err(error(
                    "Accounts changed on another device. Refresh and retry the change.",
                ));
            }
        }
        let envelope = self.client()?.credentials().await?;
        let (pin, _) = self.vault().credential_state();
        if let Some((revision, record)) = &pin {
            if envelope.revision < *revision
                || (envelope.revision == *revision && envelope.record != *record)
            {
                return Err(error(
                    "Credential history changed unexpectedly. Access is paused.",
                ));
            }
        }
        let doc = self.open(&envelope).await?;
        if pin.as_ref() != Some(&(envelope.revision, envelope.record.clone())) {
            self.vault()
                .save_credential_state(Some((envelope.revision, envelope.record)), None)?;
        }
        Ok(doc)
    }
    async fn save(&self, mut doc: Document) -> Result<(), EngineError> {
        doc.revision = doc
            .revision
            .checked_add(1)
            .filter(|v| *v <= 9_007_199_254_740_991)
            .ok_or_else(|| error("Credential revision exhausted."))?;
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&doc).map_err(|_| error("Cannot encode credentials."))?,
        );
        let material = self
            .vault()
            .seal_material(object_id_for("credentials", "accounts"))
            .await?;
        let sealed = content::seal_credentials(
            &material.binding,
            doc.revision,
            &material.key,
            &material.signer,
            &plaintext,
            MAX_BYTES,
        )
        .map_err(|_| error("Cannot encrypt credentials."))?;
        let envelope = CredentialEnvelope {
            revision: doc.revision,
            record: encode_base64(sealed.encoded()),
        };
        let (pin, _) = self.vault().credential_state();
        self.vault().save_credential_state(
            pin.clone(),
            Some((envelope.revision, envelope.record.clone())),
        )?;
        if !self.client()?.put_credentials(&envelope).await? {
            self.vault().save_credential_state(pin, None)?;
            return Err(error(
                "Accounts changed on another device. Refresh and retry the change.",
            ));
        }
        self.vault()
            .save_credential_state(Some((envelope.revision, envelope.record)), None)
    }
    pub async fn list(&self) -> Result<Vec<AgentAccount>, EngineError> {
        let _lock = self.inner.ops.lock().await;
        let doc = self.fetch().await?;
        Ok(doc
            .accounts
            .iter()
            .map(|a| AgentAccount {
                id: a.id.clone(),
                harness: a.harness,
                email: None,
                plan_label: None,
                active: doc.active.get(&a.harness) == Some(&a.id),
                usage_windows: Vec::new(),
                display_name: Some(a.label.clone()),
                organization: None,
                auth_kind: Some(AgentAuthKind::ApiKey),
                switchable: true,
                saved_at: Some(a.saved_at),
            })
            .collect())
    }
    pub async fn add(
        &self,
        harness: HarnessId,
        provider: Provider,
        label: String,
        secret: Secret,
    ) -> Result<(), EngineError> {
        self.add_with_expiry(harness, provider, label, secret, None)
            .await
    }
    pub async fn add_with_expiry(
        &self,
        harness: HarnessId,
        provider: Provider,
        label: String,
        secret: Secret,
        expires_at: Option<i64>,
    ) -> Result<(), EngineError> {
        if !provider.supports(harness) {
            return Err(error("This provider is not supported by that agent."));
        }
        let label = label.trim().to_string();
        if label.is_empty() || label.len() > 160 || label.chars().any(char::is_control) {
            return Err(error("Enter an account name (up to 160 characters)."));
        }
        let _lock = self.inner.ops.lock().await;
        let mut doc = self.fetch().await?;
        if let Some(existing) = doc.accounts.iter().find(|a| {
            a.harness == harness && a.provider == provider && a.secret.expose() == secret.expose()
        }) {
            doc.active.insert(harness, existing.id.clone());
            return self.save(doc).await;
        }
        if doc.accounts.len() >= 128 {
            return Err(error("The shared account limit is 128."));
        }
        let id = format!("shared:{}", uuid::Uuid::new_v4());
        doc.active.insert(harness, id.clone());
        doc.accounts.push(SharedAccount {
            id,
            harness,
            provider,
            label,
            secret,
            expires_at,
            saved_at: crate::now_ms(),
        });
        self.save(doc).await
    }
    pub async fn select(&self, harness: HarnessId, id: Option<&str>) -> Result<(), EngineError> {
        let _lock = self.inner.ops.lock().await;
        let mut doc = self.fetch().await?;
        if let Some(id) = id {
            if !doc
                .accounts
                .iter()
                .any(|a| a.harness == harness && a.id == id)
            {
                return Err(error("Shared account no longer exists."));
            }
            doc.active.insert(harness, id.into());
        } else {
            doc.active.remove(&harness);
        }
        self.save(doc).await
    }
    pub async fn forget(&self, harness: HarnessId, id: &str) -> Result<(), EngineError> {
        let _lock = self.inner.ops.lock().await;
        let mut doc = self.fetch().await?;
        doc.accounts.retain(|a| a.harness != harness || a.id != id);
        if doc.active.get(&harness).is_some_and(|v| v == id) {
            doc.active.remove(&harness);
        }
        self.save(doc).await
    }
    pub async fn active(&self, harness: HarnessId) -> Result<Option<SharedAccount>, EngineError> {
        if !self.vault().is_enrolled() {
            return Ok(None);
        }
        let _lock = self.inner.ops.lock().await;
        let doc = self.fetch().await?;
        let account = doc
            .accounts
            .into_iter()
            .find(|a| a.harness == harness && doc.active.get(&harness) == Some(&a.id));
        if account
            .as_ref()
            .and_then(|a| a.expires_at)
            .is_some_and(|expiry| expiry <= crate::now_ms())
        {
            return Err(error(
                "This shared account has expired. Connect it again in Settings → Agents.",
            ));
        }
        Ok(account)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credential_import_rejects_controls_and_provider_confusion() {
        for value in ["", "key\nvalue", "key\0value", "🔑"] {
            assert!(Secret::new(value.into()).is_err());
            assert!(serde_json::from_value::<Secret>(serde_json::json!(value)).is_err());
        }
        assert!(!Provider::Openai.supports(HarnessId::ClaudeCode));
        assert!(!Provider::Cursor.supports(HarnessId::Opencode));
        for harness in [HarnessId::Hermes, HarnessId::Pi, HarnessId::Opencode] {
            assert!(Provider::Anthropic.supports(harness));
            assert!(Provider::Openrouter.supports(harness));
        }
    }
}
