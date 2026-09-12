//! Unified agent settings: disclosure rows for installation, accounts and usage.

use chrono::{DateTime, Utc};
use gpui::{
    AnyElement, Context, Entity, Hsla, SharedString, Subscription, Task, Window, div, prelude::*,
    px,
};
use std::time::Duration;

use zeron_proto::{
    AgentAccount, AgentAccountsSnapshot, AgentLoginMode, AgentLoginPoll, AgentLoginStart,
    AgentLoginStatus, HarnessId,
};
use zeron_rpc::methods;

use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::popover::{self, Loadable};
use crate::state::AppState;
use crate::theme::Theme;

// ---------------------------------------------------------------------------
// Pure: usage meters + labels
// ---------------------------------------------------------------------------

pub const USAGE_WARN_FRACTION: f32 = 0.80;
pub const USAGE_CRITICAL_FRACTION: f32 = 0.95;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageLevel {
    /// < 80% — indigo.
    Normal,
    /// ≥ 80% — amber.
    Warn,
    /// ≥ 95% — red.
    Critical,
}

/// Threshold classification of a usage fraction. Pure.
pub fn usage_level(fraction: f32) -> UsageLevel {
    if fraction >= USAGE_CRITICAL_FRACTION {
        UsageLevel::Critical
    } else if fraction >= USAGE_WARN_FRACTION {
        UsageLevel::Warn
    } else {
        UsageLevel::Normal
    }
}

pub fn usage_color(level: UsageLevel, theme: &Theme) -> Hsla {
    match level {
        UsageLevel::Normal => theme.accent,
        UsageLevel::Warn => theme.warning,
        UsageLevel::Critical => theme.danger,
    }
}

/// Why a `ListAgentAccounts` load is happening. Pure input to
/// [`force_usage_for`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadTrigger {
    /// Page construction — the visit's first list.
    Mount,
    /// "Click to retry" after a failed load — still the visit's first
    /// successful list.
    Retry,
    /// The explicit Refresh button.
    Refresh,
    /// After a completed add-account login flow.
    PostLogin,
    /// After Switch/Forget succeeds.
    PostAction,
}

/// Whether a load should ask the engine to probe usage (`forceUsage`). The
/// engine only hits the provider when forced; non-forced lists serve the 60s
/// usage cache or nothing (engine/src/agent_accounts.rs module docs — the
/// design expects the UI to force "on page mount/refresh"). The visit's first
/// list (mount, or retry after a failure) must force, or every first open
/// renders "Usage unavailable" until a manual Refresh — the old app fetched
/// usage on every list. Post-Switch/Forget lists ride the still-warm cache.
pub fn force_usage_for(trigger: LoadTrigger) -> bool {
    match trigger {
        LoadTrigger::Mount | LoadTrigger::Retry | LoadTrigger::Refresh | LoadTrigger::PostLogin => {
            true
        }
        LoadTrigger::PostAction => false,
    }
}

/// Compact absolute reset moment (zeron settings.agents.tsx `formatReset`):
/// a local clock time ("3:45 PM") when it lands within ~22h, a short weekday
/// ("Mon") within a week, else month + day ("Sep 14") — a weekday is noise
/// when the window is a Codex free-tier MONTHLY reset weeks out. The caller
/// prefixes "resets ". Pure given `now`.
pub fn format_reset(resets_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Option<String> {
    use chrono::Local;
    let at = resets_at?;
    let local = at.with_timezone(&Local);
    Some(if at.signed_duration_since(now).num_hours() < 22 {
        format!("resets {}", local.format("%-I:%M %p"))
    } else if at.signed_duration_since(now).num_hours() < 24 * 7 {
        format!("resets {}", local.format("%a"))
    } else {
        format!("resets {}", local.format("%b %-d"))
    })
}

/// The provider cards, in display order: (harness, name, CLI command — named
/// in the empty-state copy, zeron settings.agents.tsx `PROVIDERS`).
pub const PROVIDERS: [(HarnessId, &str, &str); 8] = [
    (HarnessId::ClaudeCode, "Claude Code", "claude"),
    (HarnessId::Codex, "Codex", "codex"),
    (HarnessId::Cursor, "Cursor", "cursor-agent"),
    (HarnessId::Devin, "Devin", "devin"),
    (HarnessId::Grok, "Grok", "grok"),
    (HarnessId::Hermes, "Hermes", "hermes"),
    (HarnessId::Pi, "Pi", "pi"),
    (HarnessId::Opencode, "OpenCode", "opencode"),
];

/// Accounts of one provider, in the engine's order (slot creation). No
/// active-first re-sort: switching accounts must not move the switched-to
/// card — the Active badge already says which one is live, and a list that
/// reshuffles under the click reads as broken. Pure.
pub fn provider_accounts(
    snapshot: &AgentAccountsSnapshot,
    harness: HarnessId,
) -> Vec<&AgentAccount> {
    snapshot
        .accounts
        .iter()
        .filter(|a| a.harness == harness)
        .collect()
}

// ---------------------------------------------------------------------------
// Entity
// ---------------------------------------------------------------------------

enum LoginFlow {
    /// StartAgentLogin in flight.
    Starting { harness: HarnessId },
    /// Claude-style: open the URL, paste the code back.
    PasteCode {
        harness: HarnessId,
        start: AgentLoginStart,
        submitting: bool,
        error: Option<SharedString>,
    },
    /// Codex-style: open the URL, poll until the browser flow lands.
    Browser {
        harness: HarnessId,
        start: AgentLoginStart,
        message: Option<SharedString>,
        error: Option<SharedString>,
    },
}

impl LoginFlow {
    /// Dialog title (zeron: "Add Claude account" / "Add Codex account").
    fn title(&self) -> &'static str {
        let harness = match self {
            LoginFlow::Starting { harness }
            | LoginFlow::PasteCode { harness, .. }
            | LoginFlow::Browser { harness, .. } => *harness,
        };
        match harness {
            HarnessId::Codex => "Add Codex account",
            HarnessId::Cursor => "Connect Cursor",
            _ => "Add Claude account",
        }
    }
}

pub struct AccountsPage {
    state: Entity<AppState>,
    expanded: std::collections::HashSet<HarnessId>,
    harnesses: Vec<zeron_engine::registry::HarnessDescriptor>,
    titles: Entity<super::harnesses::TitleSettingsSection>,
    security: Entity<super::shared_accounts::SharedAccountsPanel>,
    key_harness: Option<HarnessId>,
    key_provider: zeron_engine::shared_credentials::Provider,
    key_input: Entity<ComposerInput>,
    label_input: Entity<ComposerInput>,
    key_busy: bool,
    _key_events: Subscription,
    snapshot: Loadable<AgentAccountsSnapshot>,
    /// Account id with an in-flight Switch/Forget.
    busy_account: Option<String>,
    login: Option<LoginFlow>,
    error: Option<SharedString>,
    code_input: Entity<ComposerInput>,
    load_task: Option<Task<()>>,
    action_task: Option<Task<()>>,
    poll_task: Option<Task<()>>,
    _observe: Subscription,
    _code_events: Subscription,
}

impl AccountsPage {
    #[cfg(feature = "browser-fixture")]
    pub(crate) fn fixture_expand(&mut self, harness: HarnessId, cx: &mut Context<Self>) {
        self.expanded.insert(harness);
        cx.notify();
    }

    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |_, _, cx| cx.notify());
        let code_input = cx.new(|cx| ComposerInput::new("Paste the authorization code", cx));
        let code_events = cx.subscribe(&code_input, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.submit_code(cx);
            }
        });
        let titles = cx.new(|cx| super::harnesses::TitleSettingsSection::new(state.clone(), cx));
        let security =
            cx.new(|cx| super::shared_accounts::SharedAccountsPanel::new(state.clone(), cx));
        let key_input = cx.new(|cx| ComposerInput::new("API key", cx).with_secret());
        let label_input = cx.new(|cx| ComposerInput::new("Account name", cx).with_single_line());
        let key_events = cx.subscribe(&key_input, |page: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                page.submit_key(cx);
            }
        });
        let mut page = Self {
            state,
            titles,
            security,
            key_harness: None,
            key_provider: zeron_engine::shared_credentials::Provider::Anthropic,
            key_input,
            label_input,
            key_busy: false,
            _key_events: key_events,
            harnesses: Vec::new(),
            expanded: std::collections::HashSet::new(),
            snapshot: Loadable::Idle,
            busy_account: None,
            login: None,
            error: None,
            code_input,
            load_task: None,
            action_task: None,
            poll_task: None,
            _observe: observe,
            _code_events: code_events,
        };
        // Force the usage probe on the visit's first list — a plain list
        // returns no usage windows on a cold engine cache, which rendered
        // every account as "Usage unavailable" until a manual Refresh. The
        // Loading skeleton (meter ghosts) covers the probe latency, so
        // "Usage unavailable" is reserved for a probe that genuinely failed.
        page.load(force_usage_for(LoadTrigger::Mount), cx);
        page
    }

    fn open_key(&mut self, harness: HarnessId, cx: &mut Context<Self>) {
        use zeron_engine::shared_credentials::Provider;
        self.key_provider = match harness {
            HarnessId::Codex => Provider::Openai,
            HarnessId::Cursor => Provider::Cursor,
            HarnessId::Devin => Provider::Devin,
            HarnessId::Grok => Provider::Xai,
            _ => Provider::Anthropic,
        };
        self.key_harness = Some(harness);
        self.key_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.label_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.error = None;
        cx.notify();
    }
    fn close_key(&mut self, cx: &mut Context<Self>) {
        if self.key_busy {
            return;
        }
        self.key_harness = None;
        self.key_input
            .update(cx, |input, cx| input.set_text("", cx));
        cx.notify();
    }
    fn submit_key(&mut self, cx: &mut Context<Self>) {
        if self.key_busy {
            return;
        }
        let Some(harness) = self.key_harness else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let key = self.key_input.read(cx).text().trim().to_string();
        let label = self.label_input.read(cx).text().trim().to_string();
        if key.is_empty() || label.is_empty() {
            self.error = Some("Enter an account name and API key.".into());
            cx.notify();
            return;
        }
        let provider = self.key_provider;
        self.key_busy = true;
        self.key_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::ADD_SHARED_AGENT_KEY, serde_json::json!({"harness": harness, "provider": provider, "label": label, "key": key})).await;
            this.update(cx, |page, cx| {
                page.key_busy = false;
                match result { Ok(_) => { page.close_key(cx); page.load(false, cx); }, Err(e) => page.error = Some(e.to_string().into()) }
                cx.notify();
            }).ok();
        }));
        cx.notify();
    }
    fn render_key_dialog(
        &mut self,
        viewport: gpui::Size<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        use zeron_engine::shared_credentials::Provider;
        let harness = self.key_harness?;
        let theme = Theme::of(cx).clone();
        let choices = [
            (Provider::Anthropic, "Anthropic"),
            (Provider::Openai, "OpenAI"),
            (Provider::Cursor, "Cursor"),
            (Provider::Devin, "Devin"),
            (Provider::Xai, "xAI"),
            (Provider::Openrouter, "OpenRouter"),
        ];
        let mut card = popover::dialog_card(&theme)
            .child(popover::dialog_title(&theme, "Connect a shared account"))
            .child(div().mt(px(8.0)).child(popover::dialog_body(&theme, "Your API key is end-to-end encrypted. Only approved devices can use it. Set up Shared accounts first.")));
        card = card
            .child(
                div().mt(px(16.0)).flex().gap(px(6.0)).children(
                    choices
                        .into_iter()
                        .enumerate()
                        .filter(|(_, (p, _))| p.supports(harness))
                        .map(|(ix, (provider, label))| {
                            crate::settings::widgets::ghost_action(&theme)
                                .id(("key-provider", ix))
                                .when(self.key_provider == provider, |el| {
                                    el.bg(crate::theme::ink(0.07))
                                })
                                .on_click(cx.listener(move |page, _, _, cx| {
                                    if !page.key_busy {
                                        page.key_provider = provider;
                                        cx.notify();
                                    }
                                }))
                                .child(label)
                        }),
                ),
            )
            .child(
                div().mt(px(14.0)).child(
                    div()
                        .w_full()
                        .px(px(12.0))
                        .py(px(10.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(theme.border)
                        .child(self.label_input.clone()),
                ),
            )
            .child(
                div().mt(px(12.0)).child(
                    div()
                        .w_full()
                        .px(px(12.0))
                        .py(px(10.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(theme.border)
                        .child(self.key_input.clone()),
                ),
            )
            .when_some(self.error.clone(), |el, e| {
                el.child(crate::settings::widgets::error_strip(&theme, e))
            })
            .child(
                div()
                    .mt(px(20.0))
                    .flex()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        popover::btn_ghost(&theme, "Cancel", "key-cancel")
                            .id("key-cancel")
                            .on_click(cx.listener(|page, _, _, cx| page.close_key(cx))),
                    )
                    .child(
                        popover::btn_primary(
                            &theme,
                            if self.key_busy {
                                "Connecting…"
                            } else {
                                "Connect"
                            },
                        )
                        .id("key-submit")
                        .on_click(cx.listener(|page, _, _, cx| page.submit_key(cx))),
                    ),
            );
        Some(popover::modal(
            "shared-key-dialog",
            viewport,
            card.into_any_element(),
        ))
    }

    fn toggle_harness(&mut self, harness: HarnessId, enabled: bool, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::SET_HARNESS_ENABLED,
                    serde_json::json!({"harness": harness, "enabled": enabled}),
                )
                .await;
            this.update(cx, |page, cx| {
                match result {
                    Ok(value) => {
                        if let Ok(list) = serde_json::from_value(value) {
                            page.harnesses = list;
                        }
                        crate::pickers::bump_harness_catalog(cx);
                    }
                    Err(error) => page.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn load(&mut self, force_usage: bool, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.snapshot = Loadable::Error("Engine not connected".into());
            return;
        };
        self.snapshot = Loadable::Loading;
        let params = serde_json::json!({ "forceUsage": force_usage });
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let (result, harnesses) = futures::join!(
                engine.client().call(methods::LIST_AGENT_ACCOUNTS, params),
                engine
                    .client()
                    .call(methods::LIST_HARNESSES, serde_json::json!({}))
            );
            this.update(cx, |page, cx| {
                if let Ok(value) = harnesses {
                    if let Ok(list) = serde_json::from_value(value) {
                        page.harnesses = list;
                    }
                }
                page.snapshot = match result {
                    Ok(value) => match serde_json::from_value::<AgentAccountsSnapshot>(value) {
                        Ok(snapshot) => Loadable::Ready(snapshot),
                        Err(err) => Loadable::Error(err.to_string()),
                    },
                    Err(err) => Loadable::Error(err.to_string()),
                };
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Switch / Forget an account.
    fn account_action(
        &mut self,
        method: &'static str,
        account: &AgentAccount,
        cx: &mut Context<Self>,
    ) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.busy_account = Some(account.id.clone());
        self.error = None;
        // Tolerant param shape: both `id` and `accountId` plus the harness.
        let params = serde_json::json!({
            "id": account.id,
            "accountId": account.id,
            "harness": account.harness,
        });
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(method, params).await;
            this.update(cx, |page, cx| {
                page.busy_account = None;
                match result {
                    Ok(_) => page.load(force_usage_for(LoadTrigger::PostAction), cx),
                    Err(err) => page.error = Some(format!("{err}").into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    // ---- add-account flows ----

    fn start_login(&mut self, harness: HarnessId, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.login = Some(LoginFlow::Starting { harness });
        self.error = None;
        let params = serde_json::json!({ "harness": harness });
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::START_AGENT_LOGIN, params)
                .await;
            this.update(cx, |page, cx| {
                match result.and_then(|value| {
                    serde_json::from_value::<AgentLoginStart>(value)
                        .map_err(|e| zeron_rpc::RpcError::Failed(e.to_string()))
                }) {
                    Ok(start) => {
                        cx.open_url(&start.url);
                        match start.mode {
                            AgentLoginMode::PasteCode => {
                                page.code_input
                                    .update(cx, |input, cx| input.set_text("", cx));
                                page.login = Some(LoginFlow::PasteCode {
                                    harness,
                                    start,
                                    submitting: false,
                                    error: None,
                                });
                            }
                            AgentLoginMode::Browser => {
                                page.login = Some(LoginFlow::Browser {
                                    harness,
                                    start,
                                    message: None,
                                    error: None,
                                });
                                page.spawn_poll(cx);
                            }
                        }
                    }
                    Err(err) => {
                        page.login = None;
                        page.error = Some(format!("Login failed to start: {err}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn submit_code(&mut self, cx: &mut Context<Self>) {
        let Some(LoginFlow::PasteCode {
            start, submitting, ..
        }) = &mut self.login
        else {
            return;
        };
        if *submitting {
            return;
        }
        let code = self.code_input.read(cx).text().trim().to_string();
        if code.is_empty() {
            return;
        }
        let login_id = start.login_id.clone();
        *submitting = true;
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = serde_json::json!({ "loginId": login_id, "code": code });
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::COMPLETE_AGENT_LOGIN, params)
                .await;
            this.update(cx, |page, cx| {
                match result {
                    Ok(_) => {
                        page.login = None;
                        page.load(force_usage_for(LoadTrigger::PostLogin), cx);
                    }
                    Err(err) => {
                        if let Some(LoginFlow::PasteCode {
                            submitting, error, ..
                        }) = &mut page.login
                        {
                            *submitting = false;
                            *error = Some(format!("{err}").into());
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// The browser-wait poll loop: PollAgentLogin every 1.5s until Done/Error.
    fn spawn_poll(&mut self, cx: &mut Context<Self>) {
        let Some(LoginFlow::Browser { start, .. }) = &self.login else {
            return;
        };
        let login_id = start.login_id.clone();
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = serde_json::json!({ "loginId": login_id });
        self.poll_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(1500))
                    .await;
                let result = engine
                    .client()
                    .call(methods::POLL_AGENT_LOGIN, params.clone())
                    .await;
                let outcome = this.update(cx, |page, cx| {
                    let Some(LoginFlow::Browser { message, error, .. }) = &mut page.login else {
                        return true; // dialog dismissed — stop polling
                    };
                    match result.as_ref().ok().and_then(|value| {
                        serde_json::from_value::<AgentLoginPoll>(value.clone()).ok()
                    }) {
                        Some(poll) => match poll.status {
                            AgentLoginStatus::Done => {
                                page.login = None;
                                page.load(force_usage_for(LoadTrigger::PostLogin), cx);
                                cx.notify();
                                true
                            }
                            AgentLoginStatus::Error => {
                                *error = Some(
                                    poll.message
                                        .unwrap_or_else(|| "Login failed".to_string())
                                        .into(),
                                );
                                cx.notify();
                                true
                            }
                            AgentLoginStatus::Pending => {
                                if let Some(text) = poll.message {
                                    *message = Some(text.into());
                                }
                                cx.notify();
                                false
                            }
                        },
                        None => {
                            let text = match &result {
                                Err(err) => format!("Poll failed: {err}"),
                                Ok(_) => "Poll failed: malformed reply".to_string(),
                            };
                            *error = Some(text.into());
                            cx.notify();
                            true
                        }
                    }
                });
                match outcome {
                    Ok(true) | Err(_) => break,
                    Ok(false) => {}
                }
            }
        }));
    }

    fn cancel_login(&mut self, cx: &mut Context<Self>) {
        let login_id = match &self.login {
            Some(LoginFlow::PasteCode { start, .. }) | Some(LoginFlow::Browser { start, .. }) => {
                Some(start.login_id.clone())
            }
            _ => None,
        };
        self.login = None;
        self.poll_task = None;
        if let (Some(login_id), Some(engine)) = (login_id, self.state.read(cx).engine().cloned()) {
            let params = serde_json::json!({ "loginId": login_id });
            self.action_task = Some(cx.spawn(async move |_, _| {
                if let Err(err) = engine
                    .client()
                    .call(methods::CANCEL_AGENT_LOGIN, params)
                    .await
                {
                    tracing::debug!(error = %err, "CancelAgentLogin failed (best-effort)");
                }
            }));
        }
        cx.notify();
    }

    // ---- render pieces ----

    /// One usage window (zeron settings.agents.tsx `UsageMeter`): label ·
    /// 5px rounded-full bar (indigo → amber ≥80% → red ≥95%) · "NN% used" ·
    /// quiet reset time.
    fn render_usage_meter(
        &self,
        window: &zeron_proto::AgentUsageWindow,
        theme: &Theme,
        now: DateTime<Utc>,
    ) -> AnyElement {
        let fraction = window.used_fraction.clamp(0.0, 1.0);
        let level = usage_level(fraction);
        let fill = usage_color(level, theme).opacity(match level {
            UsageLevel::Normal => 0.8,
            _ => 0.85,
        });
        let reset = format_reset(window.resets_at, now);
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .text_size(crate::typography::ui_rems(11.5))
            .text_color(theme.text_muted.opacity(0.7))
            .child(
                div()
                    .w(px(48.0))
                    .flex_none()
                    .truncate()
                    .child(SharedString::from(window.label.clone())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(56.0))
                    .max_w(px(230.0))
                    .h(px(5.0))
                    .rounded_full()
                    .overflow_hidden()
                    .bg(crate::theme::ink(0.07))
                    .when(fraction > 0.0, |el| {
                        el.child(
                            div()
                                .h_full()
                                // A 1.5% floor keeps tiny non-zero usage
                                // visible (zeron `max(used, 1.5)%`).
                                .w(gpui::relative(fraction.max(0.015)))
                                .rounded_full()
                                .bg(fill),
                        )
                    }),
            )
            .child(
                div()
                    .w(px(64.0))
                    .flex_none()
                    .text_right()
                    .child(SharedString::from(format!(
                        "{}% used",
                        (fraction * 100.0).round() as u32
                    ))),
            )
            .when_some(reset, |el, reset| {
                el.child(
                    div()
                        .flex_none()
                        .truncate()
                        .text_color(theme.text_muted.opacity(0.45))
                        .child(SharedString::from(reset)),
                )
            })
            .into_any_element()
    }

    /// One account row (zeron settings.agents.tsx `AccountRow`): initial
    /// avatar, email + usage meters left; badges over the Switch/Forget
    /// actions right-anchored.
    fn render_account_row(
        &self,
        account: &AgentAccount,
        ix: usize,
        first: bool,
        theme: &Theme,
        now: DateTime<Utc>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::settings::widgets;
        let is_busy = self.busy_account.as_deref() == Some(account.id.as_str());
        let email: SharedString = account
            .email
            .clone()
            .or_else(|| account.display_name.clone())
            .unwrap_or_else(|| "Unknown account".into())
            .into();
        let initial: SharedString = email
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".into())
            .into();
        let switch_account = account.clone();
        let forget_account = account.clone();
        let share_account = account.clone();

        let badges = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .child(widgets::badge(
                theme,
                if account.id.starts_with("shared:") {
                    "Shared"
                } else {
                    "This device"
                },
            ))
            .when(account.active, |el| {
                el.child(widgets::badge_active(theme, "Active"))
            })
            .when(
                !account.id.starts_with("shared:")
                    && (account.harness == HarnessId::Cursor
                        || account.auth_kind == Some(zeron_proto::AgentAuthKind::ApiKey)),
                |el| {
                    el.child(
                        widgets::ghost_action(theme)
                            .id(("share-account", ix))
                            .child("Share")
                            .on_click(cx.listener(move |page, _, _, cx| {
                                page.account_action(
                                    methods::SHARE_AGENT_ACCOUNT,
                                    &share_account,
                                    cx,
                                )
                            })),
                    )
                },
            )
            .when_some(account.plan_label.clone(), |el, plan| {
                el.child(widgets::badge(theme, plan))
            });

        // Actions only on INACTIVE accounts (zeron `{!account.active && …}`):
        // an icon-only Forget (trash, hover → foreground) then Switch, which
        // reads "Switching…" while the activate round-trips.
        let actions: Option<gpui::Div> = (!account.active || account.id.starts_with("shared:"))
            .then(|| {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.0))
                    .child(
                        div()
                            .id(("account-forget", ix))
                            .rounded(px(6.0))
                            .px(px(6.0))
                            .py(px(4.0))
                            .text_color(theme.text_muted)
                            .cursor_pointer()
                            .when(is_busy, |el| el.opacity(0.5))
                            .hover(|s| s.bg(crate::theme::ink(0.06)).text_color(theme.text))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.account_action(
                                    methods::FORGET_AGENT_ACCOUNT,
                                    &forget_account,
                                    cx,
                                );
                            }))
                            .child(
                                crate::icons::icon(crate::icons::TRASH_BIN_MINIMALISTIC)
                                    .size(px(14.0))
                                    .text_color(theme.text_muted),
                            ),
                    )
                    .when(account.switchable && !account.active, |el| {
                        el.child(
                            crate::popover::btn_primary(
                                theme,
                                if is_busy { "Switching…" } else { "Switch" },
                            )
                            .id(("account-switch", ix))
                            .px(px(8.0))
                            .py(px(4.0))
                            .rounded(px(6.0))
                            .text_size(crate::typography::ui_rems(11.5))
                            .when(is_busy, |el| el.opacity(0.5))
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.account_action(
                                        methods::ACTIVATE_AGENT_ACCOUNT,
                                        &switch_account,
                                        cx,
                                    );
                                },
                            )),
                        )
                    })
            });

        div()
            .px(px(20.0))
            .py(px(14.0))
            .when(!first, |el| el.border_t_1().border_color(theme.border))
            .flex()
            .flex_row()
            .items_stretch()
            .gap(px(12.0))
            .child(
                // Initial avatar: size-8 rounded-full border bg-white/[0.03].
                div()
                    .flex_none()
                    .self_center()
                    .size(px(32.0))
                    .rounded_full()
                    .border_1()
                    .border_color(theme.border)
                    .bg(crate::theme::ink(0.03))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(crate::typography::ui_rems(12.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text_muted)
                    .child(initial),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(widgets::row_title(theme, email))
                    .map(|el| {
                        // Meters XOR the quiet fallback line — never both
                        // (zeron: `usage ? meters : "Usage unavailable"…`).
                        if account.usage_windows.is_empty() {
                            el.child(
                                div()
                                    .mt(px(6.0))
                                    .truncate()
                                    .text_size(crate::typography::ui_rems(11.5))
                                    .text_color(theme.text_muted.opacity(0.6))
                                    .child(SharedString::from(if account.switchable {
                                        if account.id.starts_with("shared:") {
                                            "Usage is billed by your provider"
                                        } else {
                                            "Usage unavailable"
                                        }
                                    } else {
                                        "Credentials unavailable"
                                    })),
                            )
                        } else {
                            el.child(
                                div().mt(px(6.0)).flex().flex_col().gap(px(4.0)).children(
                                    account
                                        .usage_windows
                                        .iter()
                                        .map(|w| self.render_usage_meter(w, theme, now)),
                                ),
                            )
                        }
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .items_end()
                    .justify_between()
                    .gap(px(8.0))
                    .child(badges)
                    .children(actions),
            )
            .into_any_element()
    }

    fn render_login_dialog(
        &mut self,
        viewport: gpui::Size<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).clone();
        let red_text = theme.danger_muted.opacity(0.9); // red-300
        let login = self.login.as_ref()?;
        let title = login.title();
        let url_link =
            |id: &'static str, label: &'static str, url: &str, cx: &mut Context<Self>| {
                let open_url = url.to_string();
                // "Reopen the …" text link (zeron: `text-[12px]
                // text-muted-foreground/60 hover:underline`).
                div()
                    .id(id)
                    .mt(px(6.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted.opacity(0.6))
                    .truncate()
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme.text))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.open_url(&open_url);
                    }))
                    .child(SharedString::from(label))
            };
        let body: AnyElement = match login {
            LoginFlow::Starting { .. } => div()
                .mt(px(8.0))
                .child(popover::skeleton_rows(
                    "login-starting",
                    &theme,
                    2,
                    cx.entity_id(),
                    cx,
                ))
                .into_any_element(),
            LoginFlow::PasteCode {
                start,
                submitting,
                error,
                ..
            } => {
                let submitting = *submitting;
                div()
                    .flex()
                    .flex_col()
                    .child(div().mt(px(8.0)).child(popover::dialog_body(
                        &theme,
                        "A browser window opened. Sign in to the account you want to add, \
                         approve access, then paste the code Anthropic shows you below. Your \
                         current login is untouched until you switch.",
                    )))
                    .child(url_link(
                        "login-open-url",
                        "Reopen the authorization page",
                        &start.url,
                        cx,
                    ))
                    .child(
                        div().mt(px(12.0)).child(
                            popover::dialog_field(self.code_input.clone().into_any_element())
                                .font_family(theme.font_mono.clone())
                                .text_size(crate::typography::ui_rems(13.0)),
                        ),
                    )
                    .when_some(error.clone(), |el, message| {
                        el.child(
                            div()
                                .mt(px(8.0))
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(red_text)
                                .child(message),
                        )
                    })
                    .child(
                        div()
                            .mt(px(16.0))
                            .flex()
                            .flex_row()
                            .justify_end()
                            .gap(px(8.0))
                            .child(
                                popover::btn_ghost(&theme, "Cancel", "login-cancel")
                                    .id("login-cancel")
                                    .on_click(cx.listener(|this, _, _, cx| this.cancel_login(cx))),
                            )
                            .child(
                                popover::btn_primary(
                                    &theme,
                                    if submitting {
                                        "Verifying…"
                                    } else {
                                        "Add account"
                                    },
                                )
                                .id("login-submit-code")
                                .when(submitting, |el| el.opacity(0.5))
                                .on_click(cx.listener(|this, _, _, cx| this.submit_code(cx))),
                            ),
                    )
                    .into_any_element()
            }
            LoginFlow::Browser {
                harness,
                start,
                message,
                error,
            } => {
                let has_error = error.is_some();
                let body = match harness {
                    HarnessId::Cursor => {
                        "Finish signing in to Cursor in your browser. This mints a \
                         zeron-named API key you can revoke any time from Cursor's \
                         dashboard — it is separate from `cursor-agent login`."
                    }
                    _ => {
                        "Finish signing in to OpenAI in your browser. The new login is \
                         captured in an isolated profile — your current session is untouched \
                         until you switch."
                    }
                };
                div()
                    .flex()
                    .flex_col()
                    .child(div().mt(px(8.0)).child(popover::dialog_body(&theme, body)))
                    .child(url_link(
                        "login-open-url-browser",
                        "Reopen the sign-in page",
                        &start.url,
                        cx,
                    ))
                    .when(!has_error, |el| {
                        el.child(
                            div()
                                .mt(px(16.0))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .child(crate::loaders::gradient_spinner(
                                    "login-poll",
                                    &theme,
                                    3.0,
                                    cx.entity_id(),
                                    cx,
                                ))
                                .child(
                                    div()
                                        .text_size(crate::typography::ui_rems(12.5))
                                        .text_color(theme.text_muted.opacity(0.7))
                                        .child(message.clone().unwrap_or_else(|| {
                                            SharedString::from("Waiting for the browser…")
                                        })),
                                ),
                        )
                    })
                    .when_some(error.clone(), |el, message| {
                        el.child(
                            div()
                                .mt(px(12.0))
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(red_text)
                                .child(message),
                        )
                    })
                    .child(
                        div().mt(px(16.0)).flex().flex_row().justify_end().child(
                            popover::btn_ghost(
                                &theme,
                                if has_error { "Close" } else { "Cancel" },
                                "login-cancel",
                            )
                            .id("login-cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_login(cx))),
                        ),
                    )
                    .into_any_element()
            }
        };
        let card = popover::dialog_card(&theme)
            .child(popover::dialog_title(&theme, title))
            .child(body)
            .into_any_element();
        Some(popover::modal("add-account-dialog", viewport, card))
    }
}

impl Render for AccountsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::settings::widgets;
        use zeron_engine::registry::descriptor_enabled;
        let theme = Theme::of(cx).clone();
        let now = Utc::now();
        let dialog = self
            .render_key_dialog(window.viewport_size(), cx)
            .or_else(|| self.render_login_dialog(window.viewport_size(), cx));
        let snapshot = self.snapshot.ready().cloned().unwrap_or_default();
        let enabled_count = self
            .harnesses
            .iter()
            .filter(|d| descriptor_enabled(d))
            .count();
        let rows: Vec<AnyElement> = PROVIDERS
            .into_iter()
            .enumerate()
            .map(|(index, (harness, name, cli))| {
                let open = self.expanded.contains(&harness);
                let accounts = provider_accounts(&snapshot, harness);
                let descriptor = self.harnesses.iter().find(|d| d.id == harness);
                let installed = descriptor.is_some_and(|d| d.installed);
                let enabled = descriptor.is_some_and(descriptor_enabled);
                let interactive =
                    (installed || enabled) && !(installed && enabled && enabled_count == 1);
                let (mark, tint) = crate::pickers::harness_brand_icon(harness);
                let summary = if accounts.is_empty() {
                    if installed {
                        "No accounts connected".to_string()
                    } else {
                        "Not installed".to_string()
                    }
                } else {
                    format!(
                        "{} account{}",
                        accounts.len(),
                        if accounts.len() == 1 { "" } else { "s" }
                    )
                };
                let header = div()
                    .id(("agent-disclosure", index))
                    .px(px(18.0))
                    .py(px(15.0))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .cursor_pointer()
                    .hover(|style| style.bg(crate::theme::ink(0.025)))
                    .on_click(cx.listener(move |page, _, _, cx| {
                        if !page.expanded.remove(&harness) {
                            page.expanded.insert(harness);
                        }
                        cx.notify();
                    }))
                    .child(
                        div()
                            .size(px(34.0))
                            .rounded(px(9.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(crate::theme::ink(0.035))
                            .child(
                                crate::icons::icon(mark)
                                    .size(px(18.0))
                                    .text_color(tint.unwrap_or(theme.text)),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(widgets::row_title(&theme, name))
                            .child(
                                div()
                                    .mt(px(3.0))
                                    .text_size(crate::typography::ui_rems(11.5))
                                    .text_color(theme.text_muted)
                                    .child(summary),
                            ),
                    )
                    .child(
                        widgets::toggle_switch(&theme, enabled)
                            .id(("agent-toggle", index))
                            .when(interactive, |el| {
                                el.cursor_pointer()
                                    .on_click(cx.listener(move |page, _, _, cx| {
                                        cx.stop_propagation();
                                        page.toggle_harness(harness, !enabled, cx);
                                    }))
                            }),
                    )
                    .child(
                        crate::icons::icon(if open {
                            crate::icons::ALT_ARROW_DOWN
                        } else {
                            crate::icons::ALT_ARROW_RIGHT
                        })
                        .size(px(14.0))
                        .text_color(theme.text_muted),
                    );
                let mut card = widgets::section_card(&theme)
                    .mt(px(0.0))
                    .mb(px(8.0))
                    .child(header);
                if open {
                    let mut body = div().border_t_1().border_color(theme.border).pb(px(12.0));
                    for (ix, account) in accounts.iter().enumerate() {
                        body = body.child(self.render_account_row(
                            account,
                            index * 1000 + ix,
                            ix == 0,
                            &theme,
                            now,
                            cx,
                        ));
                    }
                    if accounts.is_empty() {
                        body = body.child(
                            div()
                                .px(px(20.0))
                                .py(px(16.0))
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(theme.text_muted)
                                .child(if installed {
                                    format!("Connect an account to get started with {name}.")
                                } else {
                                    format!("Install {cli} to run {name} on this device.")
                                }),
                        );
                    }
                    for warning in snapshot.warnings.iter().filter(|w| w.harness == harness) {
                        body = body.child(
                            div()
                                .px(px(18.0))
                                .child(widgets::warning_strip(&theme, warning.message.clone())),
                        );
                    }
                    body = body.child(
                        div()
                            .px(px(18.0))
                            .pt(px(6.0))
                            .flex()
                            .items_center()
                            .child(
                                widgets::ghost_action(&theme)
                                    .id(("agent-connect", index))
                                    .hover(|style| widgets::ghost_hover(&theme, style))
                                    .on_click(
                                        cx.listener(move |page, _, _, cx| {
                                            page.open_key(harness, cx)
                                        }),
                                    )
                                    .child(
                                        crate::icons::icon(crate::icons::ADD_CIRCLE).size(px(14.0)),
                                    )
                                    .child("Connect account"),
                            )
                            .when(
                                matches!(
                                    harness,
                                    HarnessId::ClaudeCode | HarnessId::Codex | HarnessId::Cursor
                                ),
                                |el| {
                                    el.child(
                                        widgets::ghost_action(&theme)
                                            .id(("agent-native-login", index))
                                            .on_click(cx.listener(move |page, _, _, cx| {
                                                page.start_login(harness, cx)
                                            }))
                                            .child("Sign in on this device"),
                                    )
                                },
                            ),
                    );
                    card = card.child(body);
                }
                card.into_any_element()
            })
            .collect();
        div()
            .id("agents-page")
            .size_full()
            .overflow_y_scroll()
            .child(
                widgets::page_column()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(widgets::page_header(&theme, "Agents", None))
                            .child(div().flex_1())
                            .child(
                                widgets::ghost_action(&theme)
                                    .id("agents-refresh")
                                    .on_click(cx.listener(|page, _, _, cx| page.load(true, cx)))
                                    .child(crate::icons::icon(crate::icons::REFRESH).size(px(15.0)))
                                    .child("Refresh"),
                            ),
                    )
                    .child(widgets::page_subtitle(
                        &theme,
                        "Your agents, connected accounts, and usage.",
                    ))
                    .when_some(self.error.clone(), |el, message| {
                        el.child(widgets::error_strip(&theme, message))
                    })
                    .when_some(
                        match &self.snapshot {
                            Loadable::Error(e) => Some(e.clone()),
                            _ => None,
                        },
                        |el, e| el.child(widgets::error_strip(&theme, e)),
                    )
                    .child(div().mt(px(24.0)).child(self.security.clone()))
                    .child(div().mt(px(16.0)).children(rows))
                    .child(self.titles.clone()),
            )
            .when_some(dialog, |el, dialog| el.child(dialog))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    #[test]
    fn first_load_of_a_visit_forces_the_usage_probe() {
        // The engine only probes usage when forced (M5c); without forcing on
        // mount, the first Accounts open always rendered "Usage unavailable".
        assert!(force_usage_for(LoadTrigger::Mount));
        // A retry after a failed load is still the visit's first successful
        // list — same requirement.
        assert!(force_usage_for(LoadTrigger::Retry));
        // Explicit refresh and a just-completed login always re-probe.
        assert!(force_usage_for(LoadTrigger::Refresh));
        assert!(force_usage_for(LoadTrigger::PostLogin));
        // Switch/Forget re-lists ride the still-warm 60s cache.
        assert!(!force_usage_for(LoadTrigger::PostAction));
    }

    #[test]
    fn usage_thresholds_match_zeron() {
        assert_eq!(usage_level(0.0), UsageLevel::Normal);
        assert_eq!(usage_level(0.79), UsageLevel::Normal);
        assert_eq!(usage_level(0.80), UsageLevel::Warn);
        assert_eq!(usage_level(0.94), UsageLevel::Warn);
        assert_eq!(usage_level(0.95), UsageLevel::Critical);
        assert_eq!(usage_level(1.0), UsageLevel::Critical);
    }

    #[test]
    fn usage_colors_map_to_theme_accents() {
        let theme = Theme::dark();
        assert_eq!(usage_color(UsageLevel::Normal, &theme), theme.accent);
        assert_eq!(usage_color(UsageLevel::Warn, &theme), theme.warning);
        assert_eq!(usage_color(UsageLevel::Critical, &theme), theme.danger);
    }

    #[test]
    fn reset_formatting_is_absolute() {
        use chrono::Local;
        let now = Utc::now();
        assert_eq!(format_reset(None, now), None);
        // Within ~22h: a local clock time ("resets 3:45 PM").
        let soon = now + TimeDelta::minutes(125);
        assert_eq!(
            format_reset(Some(soon), now),
            Some(format!(
                "resets {}",
                soon.with_timezone(&Local).format("%-I:%M %p")
            ))
        );
        // Within a week: a short weekday ("resets Mon").
        let later = now + TimeDelta::days(3);
        assert_eq!(
            format_reset(Some(later), now),
            Some(format!(
                "resets {}",
                later.with_timezone(&Local).format("%a")
            ))
        );
        // Beyond a week (Codex free tier resets ~monthly): month + day
        // ("resets Sep 14") — a weekday 4 weeks out carries no information.
        let monthly = now + TimeDelta::days(26);
        assert_eq!(
            format_reset(Some(monthly), now),
            Some(format!(
                "resets {}",
                monthly.with_timezone(&Local).format("%b %-d")
            ))
        );
    }

    #[test]
    fn provider_grouping_keeps_engine_order_even_when_active_is_later() {
        let account = |id: &str, harness: HarnessId, active: bool| AgentAccount {
            id: id.into(),
            harness,
            email: None,
            plan_label: None,
            active,
            usage_windows: vec![],
            display_name: None,
            organization: None,
            auth_kind: None,
            switchable: true,
            saved_at: None,
        };
        let snapshot = AgentAccountsSnapshot {
            accounts: vec![
                account("c1", HarnessId::ClaudeCode, false),
                account("x1", HarnessId::Codex, false),
                account("c2", HarnessId::ClaudeCode, true),
            ],
            warnings: vec![],
        };
        let claude = provider_accounts(&snapshot, HarnessId::ClaudeCode);
        let ids: Vec<&str> = claude.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            ["c1", "c2"],
            "engine (creation) order holds — switching must not move a card"
        );
        assert_eq!(provider_accounts(&snapshot, HarnessId::Codex).len(), 1);
        assert!(provider_accounts(&snapshot, HarnessId::Cursor).is_empty());
    }
}
