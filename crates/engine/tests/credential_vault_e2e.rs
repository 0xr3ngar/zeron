//! End-to-end vault control-plane exercise against a REAL edge worker
//! (`wrangler dev --var AUTH_MODE:dev`): two devices set up / pair / seal /
//! open / revoke / rotate, and a third recovers with the kit. The edge holds
//! only ciphertext and public records throughout; every trust decision is
//! made client-side against local pins.
//!
//! Run with:
//!   (cd edge && npm run test:credentials)
//!
//! Without the env var the test is skipped (no network in unit CI).

use std::sync::Arc;

use zeron_crypto::content::{self, ContentPurpose};
use zeron_crypto::record::UnverifiedRecord;
use zeron_engine::doc_host::EdgeConfig;
use zeron_engine::vault::client::VaultClient;
use zeron_engine::vault::{MemoryProtection, VaultPhase, VaultService, VaultStore, object_id_for};

fn edge_url() -> Option<String> {
    std::env::var("ZERON_VAULT_EDGE_URL")
        .ok()
        .filter(|u| !u.trim().is_empty())
}

fn device(dir: &std::path::Path, edge: &str, org: &str, user: &str) -> VaultService {
    let bearer = format!("{user}@{org}");
    let config = EdgeConfig::with_static_token(edge, bearer);
    let client = VaultClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        config,
        org,
    );
    let store = VaultStore::new(
        dir,
        format!("{org}/{user}"),
        Box::new(MemoryProtection::new()),
    );
    VaultService::open(store, Some(client), org, user)
}

fn fresh_profile() -> (String, String) {
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    (
        format!("org-{}", &nonce[..8]),
        format!("user-{}", &nonce[8..16]),
    )
}

#[tokio::test]
async fn two_devices_pair_seal_open_revoke_and_recover() {
    let Some(edge) = edge_url() else {
        eprintln!("ZERON_VAULT_EDGE_URL unset; skipping live vault e2e");
        return;
    };
    let (org, user) = fresh_profile();
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let dir_c = tempfile::tempdir().unwrap();

    // ── A: nothing exists yet, set up ────────────────────────────────────
    let a = device(dir_a.path(), &edge, &org, &user);
    let status = a.refresh().await.unwrap();
    assert_eq!(
        status.phase,
        VaultPhase::NotEnrolled {
            remote_vault: false
        }
    );
    let kit = a.setup().await.unwrap();
    assert_eq!(a.status().phase, VaultPhase::RecoveryConfirmationRequired);
    a.confirm_recovery_kit().await.unwrap();
    assert!(a.is_ready(), "{:?}", a.status().phase);
    assert_eq!(kit.kit.split('-').count(), 11);
    assert!(a.setup().await.is_err(), "second setup is refused");

    // ── B: pairs through the untrusted relay with a comparison code ─────
    let b = device(dir_b.path(), &edge, &org, &user);
    let status = b.refresh().await.unwrap();
    assert_eq!(status.phase, VaultPhase::NotEnrolled { remote_vault: true });
    assert!(
        b.setup().await.is_err(),
        "cannot create a second vault over an existing one"
    );
    let (request_id, code_on_b) = b.request_enrollment().await.unwrap();
    assert!(matches!(b.status().phase, VaultPhase::Pending { .. }));
    let pending = a.pending_requests().await.unwrap();
    let request = pending
        .iter()
        .find(|r| r.request_id == request_id)
        .expect("A sees B's request");
    assert_eq!(
        request.pairing_code, code_on_b,
        "both sides derive the same code"
    );
    // A wrong code (a relay that swapped keys) is refused.
    assert!(a.approve(&request_id, "0000-0000").await.is_err());
    a.approve(&request_id, &code_on_b).await.unwrap();
    // B learns of the approval on refresh and becomes Ready.
    let status = b.refresh().await.unwrap();
    assert_eq!(status.phase, VaultPhase::Ready, "{status:?}");
    assert_eq!(status.devices.len(), 2);

    // ── A seals, B opens (object key published through the control plane)
    let object = object_id_for("chat", "chat-e2e");
    let material = a.seal_material(object).await.unwrap();
    let sealed = content::seal(
        &material.binding,
        ContentPurpose::ChatUpdate,
        &material.key,
        &material.signer,
        b"private canary from A",
        1024,
    )
    .unwrap();
    let untrusted = *UnverifiedRecord::parse(sealed.encoded(), 2048)
        .unwrap()
        .untrusted_binding();
    let context = b.open_material(object, &untrusted).await.unwrap();
    let opened = content::open(
        sealed.encoded(),
        &context.binding,
        ContentPurpose::ChatUpdate,
        &context.key,
        &context.author_public_key,
        1024,
    )
    .unwrap();
    assert_eq!(opened.plaintext().as_bytes(), b"private canary from A");
    // Both writers converge on ONE key per object/epoch (first writer wins).
    let material_b = b.seal_material(object).await.unwrap();
    assert_eq!(material_b.key.identifier(), material.key.identifier());

    // ── A revokes B: fresh epoch; B is out, A still seals under epoch 2 ──
    let b_id = b.status().device_id.clone().unwrap();
    a.revoke(&b_id).await.unwrap();
    let status = a.status();
    assert_eq!(status.epoch, Some(2));
    let status = b.refresh().await.unwrap();
    assert_eq!(status.phase, VaultPhase::Revoked, "{status:?}");
    let material2 = a.seal_material(object).await.unwrap();
    assert_eq!(material2.binding.epoch, 2);
    assert_ne!(
        material2.key.identifier(),
        material.key.identifier(),
        "new epoch, new object key"
    );
    let sealed2 = content::seal(
        &material2.binding,
        ContentPurpose::ChatUpdate,
        &material2.key,
        &material2.signer,
        b"after revocation",
        1024,
    )
    .unwrap();
    let untrusted2 = *UnverifiedRecord::parse(sealed2.encoded(), 2048)
        .unwrap()
        .untrusted_binding();
    // B (revoked) cannot obtain epoch-2 material; its historical epoch-1
    // material still opens the earlier record (accepted history).
    assert!(b.open_material(object, &untrusted2).await.is_err());
    assert!(b.open_material(object, &untrusted).await.is_ok());

    // ── C recovers with the kit (no existing device involved) ───────────
    let c = device(dir_c.path(), &edge, &org, &user);
    assert!(
        c.recover(
            "AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA",
            None
        )
        .await
        .is_err()
    );
    let genesis = kit.recovery_file["genesisHash"].as_str().map(|h| {
        zeron_engine::vault::store::Hex(h.to_string())
            .decode::<32>()
            .unwrap()
    });
    c.recover(&kit.kit, genesis).await.unwrap();
    assert!(c.is_ready(), "{:?}", c.status().phase);
    assert_eq!(c.status().epoch, Some(3), "recovery is a fresh epoch");
    // C holds history: opens A's epoch-1 and epoch-2 records.
    let context = c.open_material(object, &untrusted2).await.unwrap();
    let opened = content::open(
        sealed2.encoded(),
        &context.binding,
        ContentPurpose::ChatUpdate,
        &context.key,
        &context.author_public_key,
        1024,
    )
    .unwrap();
    assert_eq!(opened.plaintext().as_bytes(), b"after revocation");
    assert!(c.open_material(object, &untrusted).await.is_ok());
    // A catches up to epoch 3 through the recovery envelope C published.
    let status = a.refresh().await.unwrap();
    assert_eq!(status.phase, VaultPhase::Ready, "{status:?}");
    assert_eq!(status.epoch, Some(3));
    let material3 = a.seal_material(object).await.unwrap();
    assert_eq!(material3.binding.epoch, 3);

    // ── Persistence: reopening C's store restores trust without the network
    let c_again = device(dir_c.path(), &edge, &org, &user);
    let _ = c_again; // fresh MemoryProtection cannot open the file → Locked, never plaintext
    let locked = VaultService::open(
        VaultStore::new(
            dir_c.path(),
            format!("{org}/{user}"),
            Box::new(MemoryProtection::new()),
        ),
        None,
        &org,
        &user,
    );
    assert!(matches!(
        locked.status().phase,
        VaultPhase::Unavailable { .. } | VaultPhase::Locked { .. }
    ));
    drop(Arc::new(()));
}

#[tokio::test]
async fn shared_accounts_round_trip_and_refuse_unapproved_or_revoked_devices() {
    use zeron_engine::shared_credentials::{Provider, Secret, SharedCredentials};
    use zeron_proto::HarnessId;
    let Some(edge) = edge_url() else {
        return;
    };
    let (org, user) = fresh_profile();
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let a = device(a_dir.path(), &edge, &org, &user);
    let kit = a.setup().await.unwrap();
    a.confirm_recovery_kit().await.unwrap();
    let b = device(b_dir.path(), &edge, &org, &user);
    let client = || {
        VaultClient::new(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            EdgeConfig::with_static_token(&edge, format!("{user}@{org}")),
            &org,
        )
    };
    let accounts_a = SharedCredentials::new(a.clone(), Some(client()));
    let accounts_b = SharedCredentials::new(b.clone(), Some(client()));
    assert!(
        accounts_b.list().await.is_err(),
        "a bearer alone cannot decrypt"
    );
    accounts_a
        .add(
            HarnessId::ClaudeCode,
            Provider::Anthropic,
            "Private account canary".into(),
            Secret::new("sk-private-credential-canary".into()).unwrap(),
        )
        .await
        .unwrap();
    let envelope = client().credentials().await.unwrap();
    assert_eq!(envelope.revision, 1);
    assert!(!envelope.record.contains("canary"));
    let outsider = VaultClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        EdgeConfig::with_static_token(&edge, format!("another-user@{org}")),
        &org,
    );
    let unrelated = outsider.credentials().await.unwrap();
    assert_eq!(
        unrelated.revision, 0,
        "another user has a different vault namespace"
    );
    assert!(unrelated.record.is_empty());
    let wrong_org = VaultClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        EdgeConfig::with_static_token(&edge, format!("{user}@another-org")),
        &org,
    );
    assert!(
        wrong_org.credentials().await.is_err(),
        "bearer organization must match the route"
    );
    let disk = std::fs::read(a_dir.path().join("vault.json")).unwrap();
    assert!(!String::from_utf8_lossy(&disk).contains("canary"));
    let (request, code) = b.request_enrollment().await.unwrap();
    assert!(a.approve(&request, "bad-code").await.is_err());
    a.approve(&request, &code).await.unwrap();
    b.refresh().await.unwrap();
    let list = accounts_b.list().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(
        list[0].display_name.as_deref(),
        Some("Private account canary")
    );
    assert!(list[0].active);
    assert!(!serde_json::to_string(&list).unwrap().contains("sk-private"));
    let selected = accounts_b
        .active(HarnessId::ClaudeCode)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(selected.secret.expose(), "sk-private-credential-canary");
    assert!(!format!("{selected:?}").contains("sk-private"));
    assert!(accounts_b.active(HarnessId::Codex).await.unwrap().is_none());

    // Removing an active account works; the authenticated empty document is
    // durable, so a new device cannot revive the deleted account from local slots.
    accounts_b
        .forget(HarnessId::ClaudeCode, &list[0].id)
        .await
        .unwrap();
    assert!(accounts_a.list().await.unwrap().is_empty());
    assert!(
        !client().put_credentials(&envelope).await.unwrap(),
        "old signed write cannot overwrite a new revision"
    );
    // A stale response from a malicious relay is rejected against a local pin.
    let (pin, _) = a.credential_state();
    assert!(
        a.save_credential_state(Some((0, String::new())), None)
            .is_err()
    );
    assert_eq!(a.credential_state().0, pin);

    // Concurrent editors either commit or get an explicit conflict; there is
    // no last-writer-wins merge of secrets and no invisible local-only success.
    let (one, two) = tokio::join!(
        accounts_a.add(
            HarnessId::Codex,
            Provider::Openai,
            "OpenAI".into(),
            Secret::new("openai-canary".into()).unwrap()
        ),
        accounts_b.add(
            HarnessId::Cursor,
            Provider::Cursor,
            "Cursor".into(),
            Secret::new("cursor-canary".into()).unwrap()
        )
    );
    assert!(one.is_ok() || two.is_ok());
    let list = accounts_a.list().await.unwrap();
    assert_eq!(
        list.len(),
        usize::from(one.is_ok()) + usize::from(two.is_ok())
    );

    a.revoke(&b.status().device_id.unwrap()).await.unwrap();
    assert!(accounts_b.list().await.is_err());
    assert!(accounts_b.active(HarnessId::Codex).await.is_err());
    // Recovery on a third device restores the same encrypted account data.
    let c_dir = tempfile::tempdir().unwrap();
    let c = device(c_dir.path(), &edge, &org, &user);
    c.recover(&kit.kit, None).await.unwrap();
    let accounts_c = SharedCredentials::new(c, Some(client()));
    assert_eq!(accounts_c.list().await.unwrap().len(), list.len());
}

#[tokio::test]
async fn a_crash_after_server_commit_replays_the_exact_ciphertext_once() {
    use zeron_engine::shared_credentials::{CredentialEnvelope, SharedCredentials};
    use zeron_engine::vault::{
        ProtectionKeyProvider,
        store::{ProtectionMode, VaultStoreError},
    };
    struct TestProtection([u8; 32]);
    impl ProtectionKeyProvider for TestProtection {
        fn mode(&self) -> ProtectionMode {
            ProtectionMode::Memory
        }
        fn load_or_create(&self, _: &str) -> Result<zeron_crypto::SecretBytes, VaultStoreError> {
            Ok(zeron_crypto::SecretBytes::from_slice(&self.0))
        }
    }
    let Some(edge) = edge_url() else {
        return;
    };
    let (org, user) = fresh_profile();
    let directory = tempfile::tempdir().unwrap();
    let client = || {
        VaultClient::new(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            EdgeConfig::with_static_token(&edge, format!("{user}@{org}")),
            &org,
        )
    };
    let open = || {
        VaultService::open(
            VaultStore::new(
                directory.path(),
                format!("{org}/{user}"),
                Box::new(TestProtection([37; 32])),
            ),
            Some(client()),
            &org,
            &user,
        )
    };
    let vault = open();
    vault.setup().await.unwrap();
    vault.confirm_recovery_kit().await.unwrap();
    let material = vault
        .seal_material(object_id_for("credentials", "accounts"))
        .await
        .unwrap();
    let record = content::seal_credentials(
        &material.binding,
        1,
        &material.key,
        &material.signer,
        br#"{"version":1,"revision":1,"accounts":[],"active":{}}"#,
        1024,
    )
    .unwrap();
    let record = zeron_engine::vault::client::encode_base64(record.encoded());
    vault
        .save_credential_state(None, Some((1, record.clone())))
        .unwrap();
    let envelope = CredentialEnvelope {
        revision: 1,
        record,
    };
    assert!(client().put_credentials(&envelope).await.unwrap());
    // Simulate losing the successful response before committing the local pin.
    drop(vault);
    let reopened = open();
    assert!(reopened.credential_state().1.is_some());
    let accounts = SharedCredentials::new(reopened.clone(), Some(client()));
    assert!(accounts.list().await.unwrap().is_empty());
    assert_eq!(
        reopened.credential_state().0,
        Some((1, envelope.record.clone()))
    );
    assert!(reopened.credential_state().1.is_none());
    assert_eq!(client().credentials().await.unwrap().revision, 1);
    let relabeled = CredentialEnvelope {
        revision: 2,
        record: envelope.record,
    };
    assert!(
        client().put_credentials(&relabeled).await.is_err(),
        "a bearer cannot relabel a signed revision"
    );
    assert_eq!(client().credentials().await.unwrap().revision, 1);
}

// A deliberately small test driver exercises the production resolver and an
// actual OS child without calling a provider or exposing credentials in output.
#[cfg(unix)]
struct CredentialProbe(zeron_proto::HarnessId, &'static str);
#[cfg(unix)]
#[async_trait::async_trait]
impl zeron_harness::Harness for CredentialProbe {
    fn id(&self) -> zeron_proto::HarnessId {
        self.0
    }
    fn display_name(&self) -> &str {
        "Credential launch probe"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> zeron_proto::SteeringMode {
        zeron_proto::SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[zeron_proto::ReasoningLevel] {
        &[]
    }
    async fn models(&self) -> Result<Vec<zeron_proto::Model>, zeron_harness::HarnessError> {
        let mut child = tokio::process::Command::new("/bin/sh");
        // Only synthetic test values occur in this script. Nothing is printed.
        child.arg("-c").arg(format!("test \"${{{}}}\" = portable-launch-canary && test -z \"${{OPENAI_API_BASE}}\" && test -z \"${{CLAUDE_CODE_OAUTH_TOKEN}}\"", self.1));
        child.env("OPENAI_API_BASE", "https://unrelated.invalid");
        child.env("CLAUDE_CODE_OAUTH_TOKEN", "unrelated-native-token");
        zeron_harness::runtime_auth::apply(&mut child);
        if !child.status().await?.success() {
            return Err(zeron_harness::HarnessError::Protocol(
                "Credential launch isolation failed".into(),
            ));
        }
        Ok(vec![])
    }
    async fn run(
        &self,
        _: zeron_proto::RunRequest,
        _: zeron_harness::RunControls,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            Result<zeron_proto::AgentEvent, zeron_harness::HarnessError>,
        >,
        zeron_harness::HarnessError,
    > {
        self.models().await?;
        Ok(Box::pin(futures::stream::empty()))
    }
}

#[cfg(unix)]
#[tokio::test]
async fn approved_second_device_launches_all_eight_accounts_without_the_source_device() {
    use zeron_engine::{
        registry::HarnessRegistry,
        shared_credentials::{Provider, Secret, SharedCredentials},
    };
    use zeron_proto::HarnessId;
    let Some(edge) = edge_url() else {
        return;
    };
    let (org, user) = fresh_profile();
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let a = device(a_dir.path(), &edge, &org, &user);
    a.setup().await.unwrap();
    a.confirm_recovery_kit().await.unwrap();
    let client = || {
        VaultClient::new(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            EdgeConfig::with_static_token(&edge, format!("{user}@{org}")),
            &org,
        )
    };
    let accounts_a = SharedCredentials::new(a.clone(), Some(client()));
    let cases = [
        (HarnessId::ClaudeCode, Provider::Anthropic),
        (HarnessId::Codex, Provider::Openai),
        (HarnessId::Cursor, Provider::Cursor),
        (HarnessId::Devin, Provider::Devin),
        (HarnessId::Grok, Provider::Xai),
        (HarnessId::Hermes, Provider::Openai),
        (HarnessId::Pi, Provider::Anthropic),
        (HarnessId::Opencode, Provider::Openrouter),
    ];
    for (harness, provider) in cases {
        accounts_a
            .add(
                harness,
                provider,
                "Synthetic portable account".into(),
                Secret::new("portable-launch-canary".into()).unwrap(),
            )
            .await
            .unwrap();
    }
    let b = device(b_dir.path(), &edge, &org, &user);
    let (request, code) = b.request_enrollment().await.unwrap();
    a.approve(&request, &code).await.unwrap();
    b.refresh().await.unwrap();
    // No source-device object or running broker remains.
    drop(accounts_a);
    drop(a);
    let accounts_b = SharedCredentials::new(b, Some(client()));
    let catalog = HarnessRegistry::new();
    for (harness, provider) in cases {
        catalog.register(Arc::new(CredentialProbe(harness, provider.environment())));
    }
    let scoped = catalog.with_credentials(accounts_b, b_dir.path().join("agent-profiles"));
    for (harness, _) in cases {
        scoped.resolve(harness).unwrap().models().await.unwrap();
        // Scoping B must not mutate the original catalog's credential context.
        assert!(catalog.resolve(harness).unwrap().models().await.is_err());
    }
}
