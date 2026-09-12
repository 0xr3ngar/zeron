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

struct FixtureRpc(Arc<dyn RpcService>);
#[async_trait::async_trait]
impl RpcService for FixtureRpc {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        match method {
            methods::LIST_AGENT_ACCOUNTS => RpcReply::value(&serde_json::json!({"accounts": [
                {"id":"native-claude","harness":"claude-code","email":"avery@example.com","planLabel":"Max","active":true,"authKind":"oauth","switchable":true,"usageWindows":[{"label":"Session","usedFraction":0.28,"resetsAt":"2026-09-13T02:00:00Z"},{"label":"Weekly","usedFraction":0.61,"resetsAt":"2026-09-16T00:00:00Z"}]},
                {"id":"shared:11111111-1111-4111-8111-111111111111","harness":"codex","displayName":"Personal OpenAI","active":true,"authKind":"api-key","switchable":true,"usageWindows":[]},
                {"id":"shared:22222222-2222-4222-8222-222222222222","harness":"cursor","email":"avery@example.com","active":true,"authKind":"api-key","switchable":true,"usageWindows":[]},
                {"id":"shared:33333333-3333-4333-8333-333333333333","harness":"hermes","displayName":"Research · Anthropic","active":true,"authKind":"api-key","switchable":true,"usageWindows":[]}
            ],"warnings":[]})),
            methods::LIST_HARNESSES => {
                let mut rows = zeron_engine::default_registry().descriptors();
                rows.retain(|r| r.id != HarnessId::Mock);
                for row in &mut rows {
                    row.installed = true;
                    row.enabled = Some(true);
                }
                RpcReply::value(&rows)
            }
            methods::VAULT_REFRESH => RpcReply::value(
                &serde_json::json!({"phase":"ready","protection":"keychain","epoch":1,"devices":[{"deviceId":"11111111111111111111111111111111","status":"active","thisDevice":true},{"deviceId":"22222222222222222222222222222222","status":"active","thisDevice":false}]}),
            ),
            methods::VAULT_PENDING_REQUESTS => RpcReply::value(&serde_json::json!({"requests":[]})),
            _ => self.0.handle(method, params).await,
        }
    }
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
    let _ipc = runtime.block_on(zeron_engine::serve_ipc(
        ipc_port,
        Arc::new(FixtureRpc(core.rpc_service())),
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
