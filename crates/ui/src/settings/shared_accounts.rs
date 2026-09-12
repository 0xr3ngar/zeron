//! Settings → Agents (RFC 0001 §4): the per-profile vault's explicit
//! states and the four actions a person can take — set up a vault (and save
//! the recovery kit), approve this device from another one, approve other
//! devices by comparing their code, and remove a device (rotating keys).
//!
//! Everything here is a thin view over the engine's `Vault*` RPCs; no key
//! material ever reaches the UI except the recovery kit text, which is shown
//! once for the user to save and never persisted by the UI.

use gpui::{
    AnyElement, ClipboardItem, Context, Entity, IntoElement, Render, SharedString, Subscription,
    Task, Window, div, prelude::*, px,
};
use serde_json::Value;

use zeron_rpc::methods;

use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::popover::{self, Loadable};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

/// Which text prompt is open (the page has one input at a time).
enum Prompt {
    /// Recovery: the user types the kit text.
    Recover,
}

struct PromptDialog {
    kind: Prompt,
    input: Entity<ComposerInput>,
    _events: Subscription,
}

pub struct SharedAccountsPanel {
    state: Entity<AppState>,
    status: Loadable<Value>,
    pending: Vec<Value>,
    /// Shown once after setup: (kit text, recovery file JSON).
    kit: Option<(String, String)>,
    kit_copied: bool,
    prompt: Option<PromptDialog>,
    error: Option<String>,
    busy: bool,
    expanded: bool,
    load_task: Option<Task<()>>,
    action_task: Option<Task<()>>,
    copy_task: Option<Task<()>>,
    poll_task: Option<Task<()>>,
    _observe: Subscription,
}

/// Human copy for each vault phase (RFC §4.3 state table).
pub fn phase_copy(status: &Value) -> (&'static str, String) {
    let phase = status.get("phase").and_then(Value::as_str).unwrap_or("");
    let reason = status
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    match phase {
        "ready" => ("Vault ready", "Credentials are end-to-end encrypted for your approved devices.".into()),
        "notEnrolled" => {
            if status.get("remoteVault").and_then(Value::as_bool) == Some(true) {
                ("Approve this device", "This account already has an encrypted vault. Approve this device from another device, or use your recovery key.".into())
            } else {
                ("Not set up", "Set up end-to-end encryption to share agent accounts across your devices.".into())
            }
        }
        "pending" => ("Waiting for approval", "Open Settings → Agents on an approved device and compare the code below before approving.".into()),
        "locked" => ("Unlock this device", format!("Secure key storage is unavailable: {reason}")),
        "recoveryConfirmationRequired" => ("Save recovery kit", "Save the recovery key and file, then confirm. Encrypted writes remain paused until confirmation.".into()),
        "keyUpdateRequired" => ("Waiting for encryption keys", "A vault update or key delivery is pending. This page checks automatically.".into()),
        "verificationFailed" => ("Sync paused", format!("Data could not be verified: {reason}")),
        "revoked" => ("Removed", "This device was removed from the vault. Approve it again from another device to resume.".into()),
        "unavailable" => ("Not available", reason),
        _ => ("Unknown", String::new()),
    }
}

impl SharedAccountsPanel {
    #[cfg(feature = "browser-fixture")]
    pub(crate) fn fixture_action(&mut self, action: &str, cx: &mut Context<Self>) {
        match action {
            "manage" => self.expanded = true,
            "setup" => self.setup(cx),
            "confirm" => self.action(
                methods::VAULT_CONFIRM_RECOVERY,
                serde_json::json!({}),
                |page, _| page.kit = None,
                cx,
            ),
            "enroll" => self.request_enrollment(cx),
            "approve" => {
                let request = self.pending.first().expect("pending fixture enrollment");
                self.approve(
                    request["requestId"].as_str().unwrap().into(),
                    request["pairingCode"].as_str().unwrap().into(),
                    cx,
                );
            }
            "revoke" => self.revoke("22222222222222222222222222222222".into(), cx),
            "recover" => {
                self.open_recover(cx);
                self.prompt.as_ref().unwrap().input.update(cx, |input, cx| {
                    input.set_text(
                        "AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA",
                        cx,
                    )
                });
            }
            "submit-recovery" => self.submit_prompt(cx),
            _ => panic!("unknown security fixture action: {action}"),
        }
        cx.notify();
    }

    #[cfg(feature = "browser-fixture")]
    pub(crate) fn fixture_idle(&self) -> bool {
        self.status.ready().is_some() && self.load_task.is_none() && !self.busy
    }

    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |_, _, cx| cx.notify());
        let mut page = Self {
            state,
            status: Loadable::Idle,
            pending: Vec::new(),
            kit: None,
            kit_copied: false,
            prompt: None,
            error: None,
            busy: false,
            expanded: false,
            load_task: None,
            action_task: None,
            copy_task: None,
            poll_task: None,
            _observe: observe,
        };
        page.load(cx);
        page.poll_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(2))
                    .await;
                if this
                    .update(cx, |page, cx| {
                        if !page.busy {
                            page.load(cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
        page
    }

    /// `VaultRefresh` (network reconcile + status) and, when this device is
    /// an approved member, the pending enrollment requests.
    fn load(&mut self, cx: &mut Context<Self>) {
        if self.load_task.is_some() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        if matches!(self.status, Loadable::Idle) {
            self.status = Loadable::Loading;
        }
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let status = engine
                .client()
                .call(methods::VAULT_REFRESH, serde_json::json!({}))
                .await;
            let ready = status
                .as_ref()
                .ok()
                .and_then(|s| s.get("phase").and_then(Value::as_str))
                == Some("ready");
            let pending = if ready {
                engine
                    .client()
                    .call(methods::VAULT_PENDING_REQUESTS, serde_json::json!({}))
                    .await
                    .ok()
                    .and_then(|v| v.get("requests").and_then(Value::as_array).cloned())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            this.update(cx, |page, cx| {
                page.load_task = None;
                page.status = match status {
                    Ok(value) => Loadable::Ready(value),
                    Err(err) => Loadable::Error(err.to_string()),
                };
                page.pending = pending;
                cx.notify();
            })
            .ok();
        }));
    }

    /// Run one vault action, then reload. `after` receives the reply.
    fn action(
        &mut self,
        method: &'static str,
        params: Value,
        after: impl FnOnce(&mut Self, Value) + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.error = None;
        self.busy = true;
        cx.notify();
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(method, params).await;
            this.update(cx, |page, cx| {
                page.busy = false;
                match result {
                    Ok(value) => after(page, value),
                    Err(err) => page.error = Some(err.to_string()),
                }
                page.load(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn setup(&mut self, cx: &mut Context<Self>) {
        self.action(
            methods::VAULT_SETUP,
            serde_json::json!({}),
            |page, value| {
                let kit = value
                    .get("kit")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let file = value
                    .get("recoveryFile")
                    .map(|f| serde_json::to_string_pretty(f).unwrap_or_default())
                    .unwrap_or_default();
                page.kit = Some((kit, file));
                page.kit_copied = false;
            },
            cx,
        );
    }

    fn request_enrollment(&mut self, cx: &mut Context<Self>) {
        self.action(
            methods::VAULT_REQUEST_ENROLLMENT,
            serde_json::json!({}),
            |_, _| {},
            cx,
        );
    }

    fn cancel_enrollment(&mut self, cx: &mut Context<Self>) {
        self.action(
            methods::VAULT_CANCEL_ENROLLMENT,
            serde_json::json!({}),
            |_, _| {},
            cx,
        );
    }

    fn approve(&mut self, request_id: String, code: String, cx: &mut Context<Self>) {
        self.action(
            methods::VAULT_APPROVE,
            serde_json::json!({ "requestId": request_id, "code": code }),
            |_, _| {},
            cx,
        );
    }

    fn reject(&mut self, request_id: String, cx: &mut Context<Self>) {
        self.action(
            methods::VAULT_REJECT,
            serde_json::json!({ "requestId": request_id }),
            |_, _| {},
            cx,
        );
    }

    fn revoke(&mut self, device_id: String, cx: &mut Context<Self>) {
        self.action(
            methods::VAULT_REVOKE,
            serde_json::json!({ "deviceId": device_id }),
            |_, _| {},
            cx,
        );
    }

    fn open_recover(&mut self, cx: &mut Context<Self>) {
        let input =
            cx.new(|cx| ComposerInput::new("Recovery key (XXXXX-XXXXX-…)", cx).with_secret());
        let events = cx.subscribe(&input, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.submit_prompt(cx);
            }
        });
        self.prompt = Some(PromptDialog {
            kind: Prompt::Recover,
            input,
            _events: events,
        });
        cx.notify();
    }

    fn submit_prompt(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.prompt.take() else {
            return;
        };
        let text = dialog.input.read(cx).text().trim().to_string();
        if text.is_empty() {
            cx.notify();
            return;
        }
        match dialog.kind {
            Prompt::Recover => self.action(
                methods::VAULT_RECOVER,
                serde_json::json!({ "kit": text }),
                |_, _| {},
                cx,
            ),
        }
    }

    fn copy_kit(&mut self, cx: &mut Context<Self>) {
        let Some((kit, file)) = self.kit.clone() else {
            return;
        };
        use sha2::Digest;
        let payload = format!("Zeron recovery key: {kit}\n\nRecovery file:\n{file}\n");
        let fingerprint = sha2::Sha256::digest(payload.as_bytes());
        cx.write_to_clipboard(ClipboardItem::new_string(payload));
        // Detached so leaving settings does not cancel cleanup. Do not erase
        // something else the user copied in the meantime, or retain the kit
        // plaintext just to compare it later.
        cx.spawn(async move |_, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(60))
                .await;
            let _ = cx.update(|cx| {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    let text = zeroize::Zeroizing::new(text);
                    if sha2::Sha256::digest(text.as_bytes()) == fingerprint {
                        cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
                    }
                }
            });
        })
        .detach();
        self.kit_copied = true;
        self.copy_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(1500))
                .await;
            this.update(cx, |page, cx| {
                page.kit_copied = false;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn render_prompt(
        &mut self,
        viewport: gpui::Size<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).clone();
        let dialog = self.prompt.as_ref()?;
        let input = dialog.input.clone();
        let (title, description, submit) = match &dialog.kind {
            Prompt::Recover => (
                "Use recovery key",
                "Enter the recovery key you saved when you set up encryption. This adds this device under a fresh key epoch; other devices catch up automatically.",
                "Recover",
            ),
        };
        let card = popover::dialog_card(&theme)
            .child(popover::dialog_title(&theme, title))
            .child(
                div()
                    .mt(px(8.0))
                    .text_size(crate::typography::ui_rems(12.5))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(description)),
            )
            .child(
                div()
                    .mt(px(12.0))
                    .child(popover::dialog_field(input.into_any_element())),
            )
            .child(
                div()
                    .mt(px(16.0))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        popover::btn_ghost(&theme, "Cancel", "vault-prompt-cancel")
                            .id("vault-prompt-cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.prompt = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        popover::btn_primary(&theme, submit)
                            .id("vault-prompt-submit")
                            .on_click(cx.listener(|this, _, _, cx| this.submit_prompt(cx))),
                    ),
            )
            .into_any_element();
        Some(popover::modal("vault-prompt-dialog", viewport, card))
    }

    fn action_button(
        &self,
        theme: &Theme,
        id: &'static str,
        label: &'static str,
        primary: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let busy = self.busy;
        let button = if primary {
            popover::btn_primary(theme, label)
        } else {
            popover::btn_ghost(theme, label, id)
        };
        button
            .id(id)
            .when(busy, |el| el.opacity(0.5))
            .when(!busy, |el| {
                el.cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
            })
            .into_any_element()
    }

    fn render_status(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Loadable::Ready(status) = &self.status else {
            return widgets::section_card(theme)
                .p(px(16.0))
                .child(popover::skeleton_rows(
                    "vault-skeleton",
                    theme,
                    2,
                    cx.entity_id(),
                    cx,
                ))
                .into_any_element();
        };
        let status = status.clone();
        let phase = status
            .get("phase")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let (title, copy) = phase_copy(&status);
        let epoch = status.get("epoch").and_then(Value::as_u64);
        let protection = status
            .get("protection")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let mut meta: Vec<AnyElement> = vec![
            div()
                .w_full()
                .min_w_0()
                .whitespace_normal()
                .child(SharedString::from(copy))
                .into_any_element(),
        ];
        if let Some(epoch) = epoch {
            meta.push(
                div()
                    .child(SharedString::from(format!(
                        "Key epoch {epoch} · keys protected by {}",
                        match protection.as_str() {
                            "keychain" => "the OS credential store",
                            "systemdCredential" => "a systemd credential (unattended)",
                            "keyFile" => "an operator key file (unattended)",
                            _ => "this process only",
                        }
                    )))
                    .into_any_element(),
            );
        }
        let mut actions: Vec<AnyElement> = Vec::new();
        match phase.as_str() {
            "recoveryConfirmationRequired" => {
                actions.push(self.action_button(
                    theme,
                    "vault-show-recovery",
                    "Show recovery kit",
                    true,
                    cx,
                    |this, cx| this.setup(cx),
                ));
            }
            "notEnrolled" => {
                if status.get("remoteVault").and_then(Value::as_bool) == Some(true) {
                    actions.push(self.action_button(
                        theme,
                        "vault-request",
                        "Approve from another device",
                        true,
                        cx,
                        |this, cx| this.request_enrollment(cx),
                    ));
                    actions.push(self.action_button(
                        theme,
                        "vault-recover",
                        "Use recovery key",
                        false,
                        cx,
                        |this, cx| this.open_recover(cx),
                    ));
                } else {
                    actions.push(self.action_button(
                        theme,
                        "vault-setup",
                        "Set up encryption",
                        true,
                        cx,
                        |this, cx| this.setup(cx),
                    ));
                }
            }
            "pending" => {
                actions.push(self.action_button(
                    theme,
                    "vault-cancel",
                    "Cancel request",
                    false,
                    cx,
                    |this, cx| this.cancel_enrollment(cx),
                ));
            }
            "revoked" => {
                actions.push(self.action_button(
                    theme,
                    "vault-request-again",
                    "Approve from another device",
                    true,
                    cx,
                    |this, cx| this.request_enrollment(cx),
                ));
            }
            _ => {}
        }
        if let Some(fingerprint) = status.get("genesisHash").and_then(Value::as_str) {
            let fingerprint = fingerprint.to_string();
            actions.push(self.action_button(
                theme,
                "vault-copy-fingerprint",
                "Copy vault fingerprint",
                false,
                cx,
                move |_, cx| cx.write_to_clipboard(ClipboardItem::new_string(fingerprint.clone())),
            ));
        }
        actions.push(self.action_button(
            theme,
            "vault-refresh",
            "Refresh",
            false,
            cx,
            |this, cx| this.load(cx),
        ));
        let badge = match phase.as_str() {
            "ready" => widgets::badge_active(theme, "On"),
            _ => widgets::badge(theme, title),
        };
        let mut card = widgets::section_card(theme).child(
            widgets::card_row(theme, true)
                .child(widgets::row_tile(theme, crate::icons::KEY_MINIMALISTIC))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(widgets::row_title(theme, "End-to-end encryption"))
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(4.0))
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(theme.text_muted)
                                .children(meta),
                        ),
                )
                .child(badge),
        );
        if phase == "pending"
            && let Some(code) = status.get("pairingCode").and_then(Value::as_str)
        {
            card = card.child(
                widgets::card_row(theme, false).child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .child(widgets::row_title(theme, "Comparison code"))
                        .child(
                            div()
                                .font_family("Geist Mono")
                                .text_size(crate::typography::ui_rems(22.0))
                                .text_color(theme.text)
                                .child(SharedString::from(code.to_string())),
                        )
                        .child(widgets::meta_line(
                            theme,
                            vec![div()
                                .w_full().min_w_0().whitespace_normal()
                                .child(SharedString::from(
                                    "Approve only if the approving device shows exactly this code.",
                                ))
                                .into_any_element()],
                        )),
                ),
            );
        }
        card = card.child(
            widgets::card_row(theme, false).child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(px(8.0))
                    .children(actions),
            ),
        );
        card.into_any_element()
    }

    fn render_kit(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (kit, _) = self.kit.clone()?;
        let copied = self.kit_copied;
        Some(
            widgets::section_card(theme)
                .child(
                    widgets::card_row(theme, true).child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_normal()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(widgets::row_title(theme, "Save your recovery key now"))
                            .child(
                                div()
                                    .font_family("Geist Mono")
                                    .text_size(crate::typography::ui_rems(13.0))
                                    .text_color(theme.text)
                                    .child(SharedString::from(kit)),
                            )
                            .child(widgets::warning_strip(
                                theme,
                                "If you lose every approved device and your recovery key, we \
                                 cannot recover your encrypted data. Resetting your account \
                                 password will not restore access.",
                            ))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap(px(8.0))
                                    .child(
                                        popover::btn_primary(
                                            theme,
                                            if copied {
                                                "Copied"
                                            } else {
                                                "Copy key and file"
                                            },
                                        )
                                        .id("vault-kit-copy")
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, _, cx| this.copy_kit(cx))),
                                    )
                                    .child(
                                        popover::btn_ghost(theme, "I saved it", "vault-kit-done")
                                            .id("vault-kit-done")
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.action(
                                                    methods::VAULT_CONFIRM_RECOVERY,
                                                    serde_json::json!({}),
                                                    |page, _| {
                                                        page.kit = None;
                                                    },
                                                    cx,
                                                );
                                            })),
                                    ),
                            ),
                    ),
                )
                .into_any_element(),
        )
    }

    fn render_pending(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.pending.is_empty() {
            return None;
        }
        let rows: Vec<AnyElement> = self
            .pending
            .iter()
            .enumerate()
            .map(|(ix, request)| {
                let request_id = request
                    .get("requestId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let code = request
                    .get("pairingCode")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let device = request
                    .get("deviceId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let approve_id = request_id.clone();
                let approve_code = code.clone();
                let reject_id = request_id.clone();
                widgets::card_row(theme, ix == 0)
                    .id(("vault-pending", ix))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(widgets::row_title(
                                theme,
                                format!("Device {}", crate::settings::devices::short_id(&device)),
                            ))
                            .child(
                                div()
                                    .font_family("Geist Mono")
                                    .text_size(crate::typography::ui_rems(18.0))
                                    .text_color(theme.text)
                                    .child(SharedString::from(code.clone())),
                            )
                            .child(widgets::meta_line(
                                theme,
                                vec![
                                    div()
                                        .w_full()
                                        .min_w_0()
                                        .whitespace_normal()
                                        .child(SharedString::from(
                                            "Compare with the code on the new device. An approved \
                                         device can read shared credentials and manage devices.",
                                        ))
                                        .into_any_element(),
                                ],
                            )),
                    )
                    .child(self.action_button(
                        theme,
                        "vault-reject",
                        "Reject",
                        false,
                        cx,
                        move |this, cx| this.reject(reject_id.clone(), cx),
                    ))
                    .child(self.action_button(
                        theme,
                        "vault-approve",
                        "Codes match — approve",
                        true,
                        cx,
                        move |this, cx| this.approve(approve_id.clone(), approve_code.clone(), cx),
                    ))
                    .into_any_element()
            })
            .collect();
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .mt(px(24.0))
                .child(widgets::field_label(theme, "Devices waiting for approval"))
                .child(widgets::section_card(theme).mt(px(0.0)).children(rows))
                .into_any_element(),
        )
    }

    fn render_devices(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Loadable::Ready(status) = &self.status else {
            return None;
        };
        let devices = status.get("devices")?.as_array()?.clone();
        if devices.is_empty() {
            return None;
        }
        let rows: Vec<AnyElement> = devices
            .iter()
            .enumerate()
            .map(|(ix, device)| {
                let id = device
                    .get("deviceId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let active = device.get("status").and_then(Value::as_str) == Some("active");
                let this_device = device.get("thisDevice").and_then(Value::as_bool) == Some(true);
                let state = self.state.read(cx);
                let named = state.devices.iter().find(|d| {
                    this_device && state.local_device_id.as_deref() == Some(d.id.as_str())
                });
                let label = status
                    .get("deviceNames")
                    .and_then(|names| names.get(&id))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| named.map(|d| d.name.clone()))
                    .unwrap_or_else(|| {
                        if this_device {
                            "This computer".into()
                        } else {
                            format!("Device {}", crate::settings::devices::short_id(&id))
                        }
                    });
                let icon = if named.is_some_and(|d| d.platform == "ios") {
                    crate::icons::SMARTPHONE
                } else {
                    crate::icons::MONITOR
                };
                let revoke_id = id.clone();
                let mut row = widgets::card_row(theme, ix == 0)
                    .id(("vault-device", ix))
                    .when(!active, |el| el.opacity(0.55))
                    .child(widgets::row_tile(theme, icon))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(widgets::row_title(theme, label))
                            .child(widgets::meta_line(
                                theme,
                                vec![
                                    div()
                                        .child(SharedString::from(if active {
                                            "Approved"
                                        } else {
                                            "Removed"
                                        }))
                                        .into_any_element(),
                                ],
                            )),
                    );
                if this_device {
                    row = row.child(widgets::badge_active(theme, "This device"));
                } else if active {
                    row = row.child(self.action_button(
                        theme,
                        "vault-revoke",
                        "Remove",
                        false,
                        cx,
                        move |this, cx| this.revoke(revoke_id.clone(), cx),
                    ));
                }
                row.into_any_element()
            })
            .collect();
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .mt(px(24.0))
                .child(widgets::field_label(theme, "Approved devices"))
                .child(widgets::section_card(theme).mt(px(0.0)).children(rows))
                .child(widgets::meta_line(
                    theme,
                    vec![
                        div()
                            .w_full()
                            .min_w_0()
                            .whitespace_normal()
                            .child(SharedString::from(
                                "Removing a device blocks future vault updates. Rotate API keys \
                             with your provider to invalidate copies it already received.",
                            ))
                            .into_any_element(),
                    ],
                ))
                .into_any_element(),
        )
    }
}

impl Render for SharedAccountsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let ready = self.status.ready().is_some_and(|s| s["phase"] == "ready");
        if ready
            && !self.expanded
            && self.pending.is_empty()
            && self.kit.is_none()
            && self.error.is_none()
        {
            let count = self
                .status
                .ready()
                .and_then(|s| s["devices"].as_array())
                .map(|d| d.iter().filter(|d| d["status"] == "active").count())
                .unwrap_or(0);
            return div()
                .id("account-sync-summary")
                .w_full()
                .px(px(16.0))
                .py(px(12.0))
                .rounded(px(10.0))
                .border_1()
                .border_color(theme.border)
                .flex()
                .items_center()
                .gap(px(12.0))
                .cursor_pointer()
                .hover(|s| s.bg(crate::theme::ink(0.025)))
                .on_click(cx.listener(|panel, _, _, cx| {
                    panel.expanded = true;
                    cx.notify();
                }))
                .child(
                    crate::icons::icon(crate::icons::KEY_MINIMALISTIC)
                        .size(px(16.0))
                        .text_color(theme.text_muted),
                )
                .child(
                    div()
                        .flex_1()
                        .child(widgets::row_title(&theme, "Account sync"))
                        .child(
                            div()
                                .mt(px(3.0))
                                .text_size(crate::typography::ui_rems(11.5))
                                .text_color(theme.text_muted)
                                .child(format!(
                                    "End-to-end encrypted · {count} approved {}",
                                    if count == 1 { "device" } else { "devices" }
                                )),
                        ),
                )
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(11.5))
                        .text_color(theme.text_muted)
                        .child("Manage"),
                )
                .child(
                    crate::icons::icon(crate::icons::ALT_ARROW_RIGHT)
                        .size(px(14.0))
                        .text_color(theme.text_muted),
                )
                .into_any_element();
        }
        let error = self
            .error
            .clone()
            .map(|message| widgets::error_strip(&theme, message).into_any_element());
        let load_error = match &self.status {
            Loadable::Error(message) => Some(message.clone()),
            _ => None,
        };
        let status = self.render_status(&theme, cx);
        let kit = self.render_kit(&theme, cx);
        let pending = self.render_pending(&theme, cx);
        let devices = self.render_devices(&theme, cx);
        let dialog = self.render_prompt(window.viewport_size(), cx);

        div()
            .id("shared-account-security")
            .w_full()
            .relative()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(widgets::field_label(&theme, "Shared accounts"))
                    .when(ready, |el| {
                        el.child(
                            widgets::ghost_action(&theme)
                                .id("close-account-sync")
                                .child("Done")
                                .on_click(cx.listener(|panel, _, _, cx| {
                                    panel.expanded = false;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .child(widgets::page_subtitle(
                &theme,
                "Connect once. Use your accounts on approved devices.",
            ))
            .children(error)
            .when_some(load_error, |el, message| {
                el.child(widgets::error_strip(&theme, message))
            })
            .children(kit)
            .child(status)
            .children(pending)
            .children(devices)
            .when_some(dialog, |el, dialog| el.child(dialog))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_copy_never_promises_encryption_before_ready() {
        let not_enrolled = serde_json::json!({ "phase": "notEnrolled", "remoteVault": false });
        assert_eq!(phase_copy(&not_enrolled).0, "Not set up");
        let existing = serde_json::json!({ "phase": "notEnrolled", "remoteVault": true });
        assert_eq!(phase_copy(&existing).0, "Approve this device");
        let locked = serde_json::json!({ "phase": "locked", "reason": "no keychain" });
        assert!(phase_copy(&locked).1.contains("no keychain"));
        assert_eq!(
            phase_copy(&serde_json::json!({ "phase": "ready" })).0,
            "Vault ready"
        );
        assert_eq!(
            phase_copy(&serde_json::json!({ "phase": "keyUpdateRequired" })).0,
            "Waiting for encryption keys"
        );
    }
}
