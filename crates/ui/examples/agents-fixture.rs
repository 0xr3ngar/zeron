//! Reproducible native screenshots of the Agents page with synthetic account data.
use gpui::{AppContext, AsyncApp, Bounds, WindowBounds, WindowOptions, px, size};
use std::{path::PathBuf, sync::Arc, time::Duration};
use zeron_proto::HarnessId;
use zeron_rpc::{RpcError, RpcReply, RpcService, methods};
use zeron_ui::*;
async fn pause(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}
fn capture(directory: &std::path::Path, name: &str) -> anyhow::Result<()> {
    let path = directory.join(format!("{name}.png"));
    #[cfg(target_os = "macos")]
    let status = {
        let app = objc2_app_kit::NSApplication::sharedApplication(
            objc2::MainThreadMarker::new().unwrap(),
        );
        let window = app
            .keyWindow()
            .or_else(|| app.mainWindow())
            .ok_or_else(|| anyhow::anyhow!("fixture window is not available"))?;
        std::process::Command::new("/usr/sbin/screencapture")
            .args(["-x", "-o", "-l", &window.windowNumber().to_string()])
            .arg(&path)
            .status()?
    };
    #[cfg(not(target_os = "macos"))]
    let status = {
        let capture_window = std::env::var("ZERON_BROWSER_CAPTURE_WINDOW").ok();
        let windows = std::process::Command::new("xdotool")
            .args([
                "search",
                "--onlyvisible",
                "--pid",
                &std::process::id().to_string(),
            ])
            .output()?;
        let id = capture_window
            .or_else(|| {
                String::from_utf8(windows.stdout)
                    .ok()?
                    .lines()
                    .next()
                    .map(str::to_owned)
            })
            .ok_or_else(|| anyhow::anyhow!("fixture window not visible"))?;
        std::process::Command::new("import")
            .args(["-window", &id])
            .arg(&path)
            .status()?
    };
    anyhow::ensure!(status.success(), "screenshot capture failed");
    Ok(())
}

const SECOND_DEVICE: &str = "22222222222222222222222222222222";
const SAMPLE_KIT: &str = "AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA";

fn ready_status(two_devices: bool) -> serde_json::Value {
    let mut devices = vec![
        serde_json::json!({"deviceId":"11111111111111111111111111111111","status":"active","thisDevice":true}),
    ];
    if two_devices {
        devices.push(
            serde_json::json!({"deviceId":SECOND_DEVICE,"status":"active","thisDevice":false}),
        );
    }
    serde_json::json!({"phase":"ready","protection":"keychain","epoch":1,"devices":devices})
}
struct FixtureState {
    status: serde_json::Value,
    accounts: serde_json::Value,
    pending: Vec<serde_json::Value>,
    fail_next_key: bool,
    calls: Vec<String>,
    installing: bool,
    install_failed: bool,
}
impl FixtureState {
    fn new() -> Self {
        Self {
            status: ready_status(true),
            accounts: serde_json::json!({"accounts": [
                {"id":"native-claude","harness":"claude-code","email":"avery@example.com","planLabel":"Max","active":true,"authKind":"oauth","switchable":true,"usageWindows":[{"label":"Session","usedFraction":0.28,"resetsAt":"2026-09-13T02:00:00Z"},{"label":"Weekly","usedFraction":0.61,"resetsAt":"2026-09-16T00:00:00Z"}]},
                {"id":"shared:11111111-1111-4111-8111-111111111111","harness":"codex","displayName":"Personal OpenAI","active":true,"authKind":"api-key","switchable":true,"usageWindows":[]},
                {"id":"shared:22222222-2222-4222-8222-222222222222","harness":"cursor","email":"avery@example.com","active":true,"authKind":"api-key","switchable":true,"usageWindows":[]},
                {"id":"shared:33333333-3333-4333-8333-333333333333","harness":"hermes","displayName":"Research · Anthropic","active":true,"authKind":"api-key","switchable":true,"usageWindows":[]}
            ],"warnings":[]}),
            pending: vec![],
            fail_next_key: false,
            calls: vec![],
            installing: false,
            install_failed: false,
        }
    }
    fn handle(
        &mut self,
        method: &str,
        params: &serde_json::Value,
    ) -> Option<Result<RpcReply, RpcError>> {
        self.calls.push(method.into());
        let value = match method {
            methods::LIST_HARNESS_INSTALLATIONS => {
                let mut rows = zeron_engine::default_registry().descriptors();
                rows.retain(|r| r.id != HarnessId::Mock);
                let remote = params["targetDeviceId"] == "fixture-linux";
                let installations: Vec<_> = rows.iter_mut().map(|r| {
                    let installed = !remote || r.id == HarnessId::ClaudeCode;
                    r.installed = installed; r.enabled = Some(installed);
                    let version = match r.id { HarnessId::Codex=>"0.153.3", HarnessId::ClaudeCode=>"2.1.258", HarnessId::Grok=>"1.0.4", HarnessId::Opencode=>"1.18.21", _=>"1.0.0" };
                    serde_json::json!({"harness":r.id,"installed":installed,"version":if installed {Some(version)} else {None},"managed":!remote,"executable":format!("/home/avery/.zeron/harnesses/{}/{}",zeron_harness::installations::name(r.id),version),"recommendedVersion":version,"previousVersion":if installed {Some("0.152.0")} else {None},"canInstall":true,"installing":remote && r.id==HarnessId::Codex && self.installing,"error":if remote && r.id==HarnessId::Codex && self.install_failed {Some("Download interrupted. Your previous installation is still selected. Try again when this device is online.")} else {None}})
                }).collect();
                serde_json::json!({"harnesses":rows,"installations":installations})
            }
            methods::LIST_AGENT_ACCOUNTS => {
                let mut snapshot = self.accounts.clone();
                if self.status["phase"] != "ready" {
                    snapshot["accounts"]
                        .as_array_mut()
                        .unwrap()
                        .retain(|a| !a["id"].as_str().unwrap().starts_with("shared:"));
                }
                snapshot
            }
            methods::VAULT_REFRESH => self.status.clone(),
            methods::VAULT_PENDING_REQUESTS => serde_json::json!({"requests":self.pending}),
            methods::VAULT_SETUP => {
                self.status = serde_json::json!({"phase":"recoveryConfirmationRequired","protection":"keychain"});
                serde_json::json!({"kit":SAMPLE_KIT,"recoveryFile":{"fixture":true,"warning":"Synthetic screenshot data; not a usable recovery key"}})
            }
            methods::VAULT_CONFIRM_RECOVERY => {
                self.status = ready_status(false);
                serde_json::json!({"ok":true})
            }
            methods::VAULT_REQUEST_ENROLLMENT => {
                self.status = serde_json::json!({"phase":"pending","pairingCode":"4821-7396","protection":"keychain"});
                serde_json::json!({"requestId":"fixture-request","pairingCode":"4821-7396"})
            }
            methods::VAULT_APPROVE => {
                assert_eq!(params["code"], "4821-7396");
                self.status = ready_status(true);
                self.pending.clear();
                serde_json::json!({"ok":true})
            }
            methods::VAULT_REVOKE => {
                assert_eq!(params["deviceId"], SECOND_DEVICE);
                self.status["epoch"] = serde_json::json!(2);
                self.status["devices"][1]["status"] = serde_json::json!("revoked");
                serde_json::json!({"ok":true})
            }
            methods::VAULT_RECOVER => {
                assert_eq!(params["kit"], SAMPLE_KIT);
                self.status = ready_status(true);
                self.status["epoch"] = serde_json::json!(3);
                self.status["devices"][0]["thisDevice"] = serde_json::json!(false);
                self.status["devices"][1]["status"] = serde_json::json!("revoked");
                self.status["devices"].as_array_mut().unwrap().push(serde_json::json!({"deviceId":"33333333333333333333333333333333","status":"active","thisDevice":true}));
                serde_json::json!({"ok":true})
            }
            methods::ADD_SHARED_AGENT_KEY => {
                if self.fail_next_key {
                    self.fail_next_key = false;
                    return Some(Err(RpcError::Failed(
                        "Accounts changed on another device. Refresh and retry the change.".into(),
                    )));
                }
                assert_eq!(params["key"], "sk-fixture-not-a-real-key");
                let accounts = self.accounts["accounts"].as_array_mut().unwrap();
                for account in accounts
                    .iter_mut()
                    .filter(|a| a["harness"] == params["harness"])
                {
                    account["active"] = serde_json::json!(false);
                }
                accounts.push(serde_json::json!({"id":"shared:44444444-4444-4444-8444-444444444444","harness":params["harness"],"displayName":params["label"],"active":true,"authKind":"api-key","switchable":true,"usageWindows":[]}));
                serde_json::json!({"ok":true})
            }
            methods::ACTIVATE_AGENT_ACCOUNT => {
                for account in self.accounts["accounts"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .filter(|a| a["harness"] == params["harness"])
                {
                    account["active"] = serde_json::json!(account["id"] == params["accountId"]);
                }
                self.accounts.clone()
            }
            methods::FORGET_AGENT_ACCOUNT => {
                self.accounts["accounts"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|a| a["id"] != params["accountId"]);
                self.accounts.clone()
            }
            _ => return None,
        };
        Some(RpcReply::value(&value))
    }
}
struct FixtureRpc {
    inner: Arc<dyn RpcService>,
    state: Arc<std::sync::Mutex<FixtureState>>,
}
#[async_trait::async_trait]
impl RpcService for FixtureRpc {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if method == methods::LIST_HARNESSES {
            let mut rows = zeron_engine::default_registry().descriptors();
            rows.retain(|r| r.id != HarnessId::Mock);
            for row in &mut rows {
                row.installed = true;
                row.enabled = Some(true);
            }
            return RpcReply::value(&rows);
        }
        if let Some(reply) = self.state.lock().unwrap().handle(method, &params) {
            return reply;
        }
        self.inner.handle(method, params).await
    }
}

async fn capture_step(
    window: gpui::WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
    output: &std::path::Path,
    action: Option<&str>,
    name: &str,
) -> anyhow::Result<()> {
    for _ in 0..400 {
        pause(cx, 25).await;
        if window.read_with(cx, |shell, cx| shell.fixture_agents_idle(cx))? {
            break;
        }
    }
    anyhow::ensure!(
        window.read_with(cx, |shell, cx| shell.fixture_agents_idle(cx))?,
        "Agents fixture did not load: {name}"
    );
    if let Some(action) = action {
        window.update(cx, |shell, _, cx| shell.fixture_agents_action(action, cx))?;
    }
    // Wait for the real page's RPC actions and loads, then allow native layout/paint.
    for _ in 0..400 {
        pause(cx, 25).await;
        if window.read_with(cx, |shell, cx| shell.fixture_agents_idle(cx))? {
            break;
        }
    }
    anyhow::ensure!(
        window.read_with(cx, |shell, cx| shell.fixture_agents_idle(cx))?,
        "Agents fixture did not settle: {name}"
    );
    pause(cx, 200).await;
    capture(output, name)?;
    println!("Captured {name}");
    Ok(())
}
fn main() -> anyhow::Result<()> {
    let output = PathBuf::from(std::env::args().nth(1).expect("capture directory"));
    std::fs::create_dir_all(&output)?;
    let temp = tempfile::tempdir()?;
    let runtime = tokio::runtime::Runtime::new()?;
    let core = runtime.block_on(async {
        zeron_engine::EngineCore::assemble(
            &temp.path().join("engine"),
            Arc::new(zeron_engine::default_registry()),
            HarnessId::Codex,
            None,
        )
    })?;
    let ipc_port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let fixture = Arc::new(std::sync::Mutex::new(FixtureState::new()));
    let _ipc = runtime.block_on(zeron_engine::serve_ipc(
        ipc_port,
        Arc::new(FixtureRpc {
            inner: core.rpc_service(),
            state: fixture.clone(),
        }),
    ))?;
    let data = temp.path().join("ui");
    std::fs::create_dir(&data)?;
    let boot = EngineBootConfig {
        data_dir: data.clone(),
        ipc_port,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Codex,
    };
    let device = core.device_id.clone();
    let failure = Arc::new(std::sync::Mutex::new(None));
    let result = failure.clone();
    gpui_platform::application()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_tokio::init(cx);
            gpui_base::init(cx);
            let settings = settings::UiSettings::default();
            settings::init(settings.clone(), data.clone(), cx);
            let fonts = typography::register_fonts(cx);
            typography::init(
                settings.ui_font_family.clone(),
                settings.ui_font_size,
                fonts,
                cx,
            );
            theme_library::init(data.clone(), cx);
            appearance::init(
                appearance::AppearanceMode::Dark,
                settings.theme_selection,
                settings.accent,
                settings.surface,
                cx,
            );
            history::init(
                settings.git_history_columns,
                settings.git_history_column_widths,
                settings.git_history_column_order,
                settings.git_history_author_display,
                cx,
            );
            composer::init(cx, settings.composer_send_behavior);
            terminal::panel::init(cx);
            app_menus::init(cx);
            let state = cx.new(|_| {
                let mut s = state::AppState::new();
                s.connection = zeron_proto::view::ConnectionStatus::Ready;
                s.workspace_scope = Some(zeron_proto::WorkspaceScope::Local);
                s.local_device_id = Some(device);
                s.auto_selected = true;
                s.chats_synced = true;
                s.spaces_synced = true;
                s
            });
            state::AppState::bootstrap(state.clone(), boot.clone(), cx);
            let window = cx
                .open_window(
                    WindowOptions {
                        window_background: theme::Theme::of(cx).window_background_appearance(),
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            gpui::point(px(12.), px(30.)),
                            size(px(1180.), px(980.)),
                        ))),
                        ..Default::default()
                    },
                    |_, cx| cx.new(|cx| shell::Shell::new(state.clone(), boot, cx)),
                )
                .unwrap();
            cx.activate(true);
            cx.spawn(async move |cx| {
                let run: anyhow::Result<()> = async {
                    for _ in 0..200 {
                        if state.read_with(cx, |s, _| s.engine().is_some()) {
                            break;
                        }
                        pause(cx, 50).await;
                    }
                    anyhow::ensure!(
                        state.read_with(cx, |s, _| s.engine().is_some()),
                        "fixture engine did not attach"
                    );
                    pause(cx, 500).await;
                    state.update(cx, |s,cx| {
                        let local = s.local_device_id.clone().unwrap();
                        s.devices = vec![
                            serde_json::from_value(serde_json::json!({"id":local,"name":"Avery’s MacBook Pro","platform":"darwin","lastSeenAt":chrono::Utc::now()})).unwrap(),
                            serde_json::from_value(serde_json::json!({"id":"fixture-linux","name":"Linux workstation","platform":"linux","lastSeenAt":chrono::Utc::now()})).unwrap(),
                            serde_json::from_value(serde_json::json!({"id":"fixture-offline","name":"Home desktop","platform":"linux","lastSeenAt":null})).unwrap(),
                        ]; cx.notify();
                    });
                    window.update(cx, |shell,_,cx| shell.fixture_open_harnesses(cx))?;
                    pause(cx, 1200).await;
                    capture(&output, "harnesses-devices-dark")?;
                    window.update(cx, |shell,_,cx| shell.fixture_harness(HarnessId::Codex, state.read(cx).local_device_id.clone(), cx))?;
                    pause(cx, 600).await;
                    capture(&output, "harnesses-manage-dark")?;
                    cx.update(|cx| appearance::set_mode(appearance::AppearanceMode::Light,cx));
                    pause(cx, 400).await;
                    capture(&output, "harnesses-manage-light")?;
                    cx.update(|cx| appearance::set_mode(appearance::AppearanceMode::Dark,cx));
                    fixture.lock().unwrap().installing = true;
                    window.update(cx, |shell,_,cx| shell.fixture_open_harnesses(cx))?;
                    pause(cx, 600).await;
                    capture(&output, "harnesses-installing")?;
                    fixture.lock().unwrap().installing = false;
                    fixture.lock().unwrap().install_failed = true;
                    window.update(cx, |shell,_,cx| shell.fixture_harness(HarnessId::Codex,None,cx))?;
                    pause(cx, 600).await;
                    capture(&output, "harnesses-install-error")?;
                    window.update(cx, |shell, _, cx| shell.fixture_open_agents(cx))?;
                    pause(cx, 1600).await;
                    capture(&output, "agents-overview-dark")?;
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_expand_agent(HarnessId::ClaudeCode, cx);
                        shell.fixture_expand_agent(HarnessId::Codex, cx);
                    })?;
                    pause(cx, 500).await;
                    capture(&output, "agents-accounts-dark")?;
                    cx.update(|cx| appearance::set_mode(appearance::AppearanceMode::Light, cx));
                    pause(cx, 500).await;
                    capture(&output, "agents-accounts-light")?;
                    cx.update(|cx| appearance::set_mode(appearance::AppearanceMode::Dark, cx));
                    fixture.lock().unwrap().accounts["accounts"].as_array_mut().unwrap().retain(|a| !a["id"].as_str().unwrap().starts_with("shared:"));
                    fixture.lock().unwrap().status = serde_json::json!({"phase":"notEnrolled","remoteVault":false,"protection":"keychain"});
                    window.update(cx, |shell, _, cx| shell.fixture_open_agents(cx))?;
                    capture_step(window, cx, &output, None, "flow-01-set-up-encryption").await?;
                    capture_step(window, cx, &output, None, "flow-02-save-recovery-kit").await?;
                    capture_step(window, cx, &output, Some("security-confirm"), "flow-03-encryption-ready").await?;

                    fixture.lock().unwrap().accounts = FixtureState::new().accounts;
                    fixture.lock().unwrap().status = serde_json::json!({"phase":"notEnrolled","remoteVault":true,"protection":"keychain"});
                    window.update(cx, |shell, _, cx| shell.fixture_open_agents(cx))?;
                    capture_step(window, cx, &output, None, "flow-04-new-device").await?;
                    capture_step(window, cx, &output, Some("security-enroll"), "flow-05-comparison-code").await?;
                    {
                        let mut f = fixture.lock().unwrap(); f.status = ready_status(false);
                        f.pending = vec![serde_json::json!({"requestId":"fixture-request","deviceId":SECOND_DEVICE,"pairingCode":"4821-7396"})];
                    }
                    window.update(cx, |shell, _, cx| shell.fixture_open_agents(cx))?;
                    capture_step(window, cx, &output, None, "flow-06-approve-matching-device").await?;
                    capture_step(window, cx, &output, Some("security-approve"), "flow-07-device-approved").await?;
                    capture_step(window, cx, &output, Some("security-manage"), "flow-08-manage-approved-devices").await?;

                    window.update(cx, |shell, _, cx| shell.fixture_open_agents(cx))?;
                    capture_step(window, cx, &output, None, "flow-09-ready-to-connect").await?;
                    capture_step(window, cx, &output, Some("connect-codex"), "flow-10-connect-shared-account").await?;
                    capture_step(window, cx, &output, Some("submit-key"), "flow-11-shared-account-connected").await?;
                    capture_step(window, cx, &output, Some("switch"), "flow-12-account-switched").await?;
                    capture_step(window, cx, &output, Some("forget"), "flow-13-account-removed").await?;
                    capture_step(window, cx, &output, Some("connect-hermes"), "flow-14-hermes-provider-options").await?;
                    fixture.lock().unwrap().fail_next_key = true;
                    capture_step(window, cx, &output, Some("submit-key"), "flow-15-concurrent-change-error").await?;
                    capture_step(window, cx, &output, Some("cancel-key"), "flow-16-key-dialog-dismissed").await?;

                    window.update(cx, |shell, _, cx| shell.fixture_open_agents(cx))?;
                    capture_step(window, cx, &output, Some("security-manage"), "flow-17-before-device-removal").await?;
                    capture_step(window, cx, &output, Some("security-revoke"), "flow-18-device-access-removed").await?;
                    fixture.lock().unwrap().status = serde_json::json!({"phase":"revoked","protection":"keychain"});
                    window.update(cx, |shell, _, cx| shell.fixture_open_agents(cx))?;
                    capture_step(window, cx, &output, None, "flow-19-removed-device").await?;
                    fixture.lock().unwrap().status = serde_json::json!({"phase":"notEnrolled","remoteVault":true,"protection":"keychain"});
                    window.update(cx, |shell, _, cx| shell.fixture_open_agents(cx))?;
                    capture_step(window, cx, &output, Some("security-recover"), "flow-20-recover-with-key").await?;
                    capture_step(window, cx, &output, Some("security-submit-recovery"), "flow-21-recovered-device").await?;
                    window.update(cx, |shell, _, cx| shell.fixture_agents_action("refresh", cx))?;
                    capture_step(window, cx, &output, Some("security-manage"), "flow-22-recovered-new-epoch").await?;
                    fixture.lock().unwrap().status = serde_json::json!({"phase":"locked","protection":"keychain","reason":"Unlock your OS credential store and try again."});
                    window.update(cx, |shell, _, cx| shell.fixture_open_agents(cx))?;
                    capture_step(window, cx, &output, None, "flow-23-locked-device").await?;
                    for expected in [methods::VAULT_SETUP, methods::VAULT_CONFIRM_RECOVERY, methods::VAULT_REQUEST_ENROLLMENT, methods::VAULT_APPROVE, methods::VAULT_REVOKE, methods::VAULT_RECOVER, methods::ADD_SHARED_AGENT_KEY, methods::ACTIVATE_AGENT_ACCOUNT, methods::FORGET_AGENT_ACCOUNT] {
                        anyhow::ensure!(fixture.lock().unwrap().calls.iter().any(|call| call == expected), "flow never exercised {expected}");
                    }
                    Ok(())
                }
                .await;
                if let Err(e) = run {
                    *result.lock().unwrap() = Some(format!("{e:#}"));
                }
                let _ = window.update(cx, |_, w, _| w.remove_window());
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    runtime.block_on(core.shutdown());
    if let Some(error) = failure.lock().unwrap().take() {
        anyhow::bail!(error);
    }
    Ok(())
}
