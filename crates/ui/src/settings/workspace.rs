use chrono::{DateTime, Utc};
use gpui::{
    AnyElement, ClipboardItem, Context, Entity, EventEmitter, SharedString, Subscription, Task,
    Window, div, prelude::*, px,
};
use serde::Deserialize;
use serde_json::json;
use zeron_proto::WorkspaceScope;
use zeron_rpc::methods;

use crate::composer::ComposerInput;
use crate::popover::{self, Loadable};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum NodeRole {
    Client,
    Server,
}

impl NodeRole {
    fn wire(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Server => "server",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Client => "Client",
            Self::Server => "Agent server",
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrivateNode {
    device_id: String,
    name: String,
    role: NodeRole,
    #[serde(with = "chrono::serde::ts_milliseconds")]
    paired_at: DateTime<Utc>,
}

#[derive(Clone, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum PrivateStatus {
    Unconfigured,
    Configured {
        workspace_id: String,
        hub_url: String,
        name: String,
        role: NodeRole,
        host_hub: bool,
        enabled: bool,
        nodes: Vec<PrivateNode>,
    },
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Invitation {
    code: String,
    #[serde(with = "chrono::serde::ts_milliseconds")]
    expires_at: DateTime<Utc>,
    hub_url: String,
}

fn invitation_link(invitation: &Invitation) -> String {
    let mut url = url::Url::parse("zeron://private/join").expect("fixed private pairing URL");
    url.query_pairs_mut()
        .append_pair("hub", &invitation.hub_url)
        .append_pair("code", &invitation.code);
    url.into()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Setup {
    Choose,
    Create,
    Join,
}

pub enum WorkspaceEvent {
    StartCloud,
    LeaveCloud,
    RunInBackground,
    Restart {
        target: WorkspaceScope,
        import: bool,
    },
}

impl EventEmitter<WorkspaceEvent> for WorkspacePage {}

pub struct WorkspacePage {
    state: Entity<AppState>,
    scroll: widgets::PageScroll,
    status: Loadable<PrivateStatus>,
    setup: Setup,
    role: NodeRole,
    invitation_role: NodeRole,
    bring_work: bool,
    name: Entity<ComposerInput>,
    hub: Entity<ComposerInput>,
    code: Entity<ComposerInput>,
    invitation: Option<Invitation>,
    confirm_leave: bool,
    revoke: Option<String>,
    error: Option<String>,
    notice: Option<String>,
    busy: bool,
    unsaved_files: bool,
    task: Option<Task<()>>,
    expiry_task: Option<Task<()>>,
    _observe: Subscription,
}

impl WorkspacePage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |_, _, cx| cx.notify());
        let mut page = Self {
            state,
            scroll: widgets::PageScroll::default(),
            status: Loadable::Idle,
            setup: Setup::Choose,
            role: NodeRole::Server,
            invitation_role: NodeRole::Client,
            bring_work: false,
            name: cx.new(|cx| ComposerInput::new("Name", cx).with_single_line()),
            hub: cx.new(|cx| {
                ComposerInput::new("https://your-hub.your-tailnet.ts.net:8443", cx)
                    .with_single_line()
            }),
            code: cx.new(|cx| ComposerInput::new("Six-digit pairing code", cx).with_single_line()),
            invitation: None,
            confirm_leave: false,
            revoke: None,
            error: None,
            notice: None,
            busy: false,
            unsaved_files: false,
            task: None,
            expiry_task: None,
            _observe: observe,
        };
        page.load(cx);
        page
    }

    pub fn prefill_invitation(
        &mut self,
        invitation: crate::links::PrivateInvitationLink,
        cx: &mut Context<Self>,
    ) {
        if self.state.read(cx).workspace_scope != Some(WorkspaceScope::Local) {
            self.error = Some("Switch to Local before joining another workspace.".into());
        } else {
            self.hub
                .update(cx, |input, cx| input.set_text(invitation.hub_url, cx));
            self.code
                .update(cx, |input, cx| input.set_text(invitation.code, cx));
            self.setup = Setup::Join;
        }
        cx.notify();
    }

    fn switch_blocked(&self, cx: &Context<Self>) -> bool {
        let state = self.state.read(cx);
        self.unsaved_files
            || state.sessions.iter().any(|session| {
                Some(session.device_id.as_str()) == state.local_device_id.as_deref()
                    && matches!(
                        state.indicator_for(&session.chat_id, Utc::now()),
                        crate::state::Indicator::Working | crate::state::Indicator::AwaitingInput
                    )
            })
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.status =
                Loadable::Error("The engine is not connected. Retry when it is ready.".into());
            return;
        };
        self.status = Loadable::Loading;
        self.busy = true;
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::PRIVATE_STATUS, json!({}))
                .await;
            this.update(cx, |page, cx| {
                page.busy = false;
                page.status = match result {
                    Ok(value) => match serde_json::from_value(value) {
                        Ok(status) => Loadable::Ready(status),
                        Err(error) => Loadable::Error(format!("Invalid private status: {error}")),
                    },
                    Err(error) => Loadable::Error(error.to_string()),
                };
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn configure(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.switch_blocked(cx) {
            return;
        }
        let name = self.name.read(cx).text().trim().to_owned();
        if name.is_empty() {
            self.error = Some("Enter a name.".into());
            cx.notify();
            return;
        }
        let (method, params) = match self.setup {
            Setup::Create => (
                methods::CREATE_PRIVATE_WORKSPACE,
                json!({"name": name, "role": self.role.wire()}),
            ),
            Setup::Join => {
                let hub = self.hub.read(cx).text().trim().to_owned();
                let code = self.code.read(cx).text().trim().to_owned();
                if hub.is_empty()
                    || code.len() != 6
                    || !code.bytes().all(|byte| byte.is_ascii_digit())
                {
                    self.error =
                        Some("Enter the hub HTTPS address and its six-digit pairing code.".into());
                    cx.notify();
                    return;
                }
                (
                    methods::JOIN_PRIVATE_WORKSPACE,
                    json!({"hubUrl": hub, "code": code, "name": name, "role": self.role.wire()}),
                )
            }
            Setup::Choose => return,
        };
        self.call(
            method,
            params,
            Some((WorkspaceScope::Private, self.bring_work)),
            cx,
        );
    }

    fn call(
        &mut self,
        method: &'static str,
        params: serde_json::Value,
        restart: Option<(WorkspaceScope, bool)>,
        cx: &mut Context<Self>,
    ) {
        if self.busy || (restart.is_some() && self.switch_blocked(cx)) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.error = Some("The engine is not connected.".into());
            cx.notify();
            return;
        };
        self.busy = true;
        self.error = None;
        self.notice = None;
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(method, params).await;
            this.update(cx, |page, cx| {
                page.busy = false;
                match result {
                    Err(error) => page.error = Some(error.to_string()),
                    Ok(value) => {
                        if let Some((target, import)) = restart {
                            page.code.update(cx, |input, cx| input.set_text("", cx));
                            page.busy = true;
                            page.notice = Some("Switching workspace…".into());
                            cx.emit(WorkspaceEvent::Restart { target, import });
                        } else if method == methods::CREATE_PRIVATE_INVITATION {
                            match serde_json::from_value(value) {
                                Ok(invitation) => {
                                    page.invitation = Some(invitation);
                                    let delay = page
                                        .invitation
                                        .as_ref()
                                        .map(|invite| {
                                            (invite.expires_at - Utc::now())
                                                .to_std()
                                                .unwrap_or_default()
                                        })
                                        .unwrap_or_default();
                                    page.expiry_task = Some(cx.spawn(async move |this, cx| {
                                        cx.background_executor().timer(delay).await;
                                        this.update(cx, |_, cx| cx.notify()).ok();
                                    }));
                                }
                                Err(error) => {
                                    page.error = Some(format!("Invalid invitation: {error}"))
                                }
                            }
                        } else {
                            page.invitation = None;
                            page.revoke = None;
                            page.load(cx);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn transition_failed(&mut self, error: String, cx: &mut Context<Self>) {
        self.busy = false;
        self.notice = None;
        self.error = Some(error);
        self.load(cx);
    }

    pub fn set_unsaved_files(&mut self, unsaved: bool, cx: &mut Context<Self>) {
        if self.unsaved_files != unsaved {
            self.unsaved_files = unsaved;
            cx.notify();
        }
    }

    fn button(
        &self,
        theme: &Theme,
        id: &'static str,
        title: &'static str,
        enabled: bool,
        action: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        widgets::ghost_action(theme)
            .id(id)
            .opacity(if enabled { 1.0 } else { 0.45 })
            .child(title)
            .on_click(cx.listener(move |this, _, _, cx| {
                if enabled && !this.busy {
                    action(this, cx);
                }
            }))
            .into_any_element()
    }

    fn role_picker(&self, theme: &Theme, invitation: bool, cx: &mut Context<Self>) -> AnyElement {
        let role = if invitation {
            self.invitation_role
        } else {
            self.role
        };
        div()
            .flex()
            .gap(px(8.0))
            .children(
                [NodeRole::Client, NodeRole::Server]
                    .into_iter()
                    .map(|option| {
                        widgets::ghost_action(theme)
                            .id(SharedString::from(format!(
                                "private-role-{invitation}-{}",
                                option.wire()
                            )))
                            .border_1()
                            .border_color(if option == role {
                                theme.accent_strong
                            } else {
                                theme.border
                            })
                            .child(option.label())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.busy {
                                    if invitation {
                                        this.invitation_role = option;
                                    } else {
                                        this.role = option;
                                    }
                                    cx.notify();
                                }
                            }))
                    }),
            )
            .into_any_element()
    }

    fn render_setup(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let creating = self.setup == Setup::Create;
        let ready = !self.busy && !self.switch_blocked(cx);
        let mut form = div()
            .mt(px(20.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(widgets::row_title(
                theme,
                if creating {
                    "Create private workspace"
                } else {
                    "Join private workspace"
                },
            ))
            .child(widgets::field_label(
                theme,
                if creating {
                    "Workspace name"
                } else {
                    "This device's name"
                },
            ))
            .child(popover::dialog_field(self.name.clone().into_any_element()));
        if !creating {
            form = form
                .child(widgets::field_label(theme, "Hub HTTPS address"))
                .child(popover::dialog_field(self.hub.clone().into_any_element()))
                .child(widgets::field_label(theme, "Pairing code"))
                .child(popover::dialog_field(self.code.clone().into_any_element()));
        }
        form = form.child(widgets::field_label(theme, "This device's role"))
            .child(self.role_picker(theme, false, cx))
            .child(widgets::page_subtitle(theme, "Clients control sessions. Agent servers also run agents against their own repositories."));
        if self.state.read(cx).workspace_scope == Some(WorkspaceScope::Local) {
            form = form.child(div().id("private-bring-work").flex().items_center().gap(px(10.0)).cursor_pointer()
                .child(widgets::toggle_switch(theme, self.bring_work))
                .child(widgets::row_title(theme, "Bring my local work"))
                .on_click(cx.listener(|this, _, _, cx| {
                    if !this.busy { this.bring_work = !this.bring_work; cx.notify(); }
                })))
                .child(widgets::page_subtitle(theme, "Copies local projects and conversations. The original local workspace stays on this device."));
        }
        if creating {
            form = form.child(widgets::page_subtitle(theme, "Tailscale must be connected with HTTPS enabled. Setup configures a private Tailscale Serve listener on port 8443. Existing routes are preserved."));
        } else {
            form = form.child(widgets::page_subtitle(theme, "Connect this device to the same tailnet. The hub administrator chooses the invitation's role."));
        }
        form.child(
            div()
                .flex()
                .gap(px(8.0))
                .child(self.button(
                    theme,
                    "private-submit",
                    if creating {
                        "Create workspace"
                    } else {
                        "Join workspace"
                    },
                    ready,
                    |this, cx| this.configure(cx),
                    cx,
                ))
                .child(self.button(
                    theme,
                    "private-setup-cancel",
                    "Cancel",
                    !self.busy,
                    |this, cx| {
                        this.setup = Setup::Choose;
                        this.code.update(cx, |input, cx| input.set_text("", cx));
                        cx.notify();
                    },
                    cx,
                )),
        )
        .into_any_element()
    }

    fn render_invitation(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let invitation = self.invitation.as_ref()?;
        let link = invitation_link(invitation);
        let expired = invitation.expires_at <= Utc::now();
        let mut card = div()
            .mt(px(12.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(widgets::row_title(
                theme,
                if expired {
                    "Invitation expired"
                } else {
                    "Pair another device"
                },
            ))
            .child(widgets::page_subtitle(
                theme,
                format!("Hub: {}", invitation.hub_url),
            ))
            .child(widgets::page_subtitle(
                theme,
                format!(
                    "Expires {}. Each invitation can be used once.",
                    invitation.expires_at.format("%H:%M UTC")
                ),
            ));
        if !expired {
            card = card.child(
                div()
                    .text_size(px(28.0))
                    .font_family(theme.font_mono.clone())
                    .child(invitation.code.clone()),
            );
            match qrcode::QrCode::new(link.as_bytes()) {
                Ok(qr) => card = card.child(qr_code(qr)),
                Err(error) => card = card.child(widgets::error_strip(
                    theme,
                    format!(
                        "Could not render QR code: {error}. Use the hub address and pairing code."
                    ),
                )),
            }
            card = card.child(self.button(
                theme,
                "private-copy-invitation",
                "Copy invitation",
                true,
                move |this, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(link.clone()));
                    this.notice = Some("Invitation copied.".into());
                    cx.notify();
                },
                cx,
            ));
        }
        Some(card.into_any_element())
    }

    fn render_status(
        &self,
        status: PrivateStatus,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let PrivateStatus::Configured {
            workspace_id,
            hub_url,
            name,
            role,
            host_hub,
            enabled,
            nodes,
        } = status
        else {
            return self.render_unconfigured(theme, cx);
        };
        let mut content = div()
            .mt(px(20.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(widgets::row_title(theme, "Private via Tailscale"))
            .child(widgets::row_title(theme, name))
            .child(widgets::page_subtitle(theme, hub_url))
            .child(widgets::page_subtitle(
                theme,
                format!("Workspace {workspace_id}"),
            ))
            .child(widgets::page_subtitle(
                theme,
                format!(
                    "{}{} · {}",
                    role.label(),
                    if host_hub { " and sync hub" } else { "" },
                    if enabled {
                        "Private access enabled"
                    } else {
                        "Private access disabled"
                    }
                ),
            ));
        let (scope, connectivity, background, own_id) = {
            let state = self.state.read(cx);
            (
                state.workspace_scope,
                state.connectivity.state,
                state.engine().is_some_and(|engine| {
                    matches!(engine.mode(), crate::state::EngineMode::Remote { .. })
                }),
                state.local_device_id.clone(),
            )
        };
        if scope == Some(WorkspaceScope::Private) {
            content = content.child(widgets::page_subtitle(
                theme,
                format!("Connection: {connectivity:?}"),
            ));
        } else {
            content = content.child(widgets::page_subtitle(
                theme,
                "Configuration saved. Restart Zeron to finish switching workspaces.",
            ));
            content = content.child(self.button(
                theme,
                "private-retry-switch",
                "Finish switching workspace",
                !self.busy && !self.switch_blocked(cx),
                |this, cx| {
                    cx.emit(WorkspaceEvent::Restart {
                        target: WorkspaceScope::Private,
                        import: this.bring_work,
                    })
                },
                cx,
            ));
        }
        if scope == Some(WorkspaceScope::Private) {
            content = content.child(widgets::page_subtitle(
                theme,
                if background {
                    "Connected to a background engine. Closing this window leaves it running."
                } else {
                    "The engine runs in this app. Keep it open for remote access."
                },
            ));
            if !background && cfg!(any(target_os = "linux", target_os = "macos")) {
                content = content.child(widgets::page_subtitle(theme, "Run in background installs the Zeron user service and starts it when you sign in."))
                    .child(self.button(theme, "private-background", "Run in background", !self.busy && !self.switch_blocked(cx), |_, cx| cx.emit(WorkspaceEvent::RunInBackground), cx));
            }
        }
        if host_hub {
            content = content.child(self.button(
                theme,
                "private-access-toggle",
                if enabled {
                    "Disable private access"
                } else {
                    "Enable private access"
                },
                !self.busy,
                move |this, cx| {
                    this.call(
                        methods::SET_PRIVATE_ACCESS_ENABLED,
                        json!({"enabled": !enabled}),
                        None,
                        cx,
                    )
                },
                cx,
            ));
            if enabled {
                content = content
                    .child(widgets::field_label(theme, "Invite a device"))
                    .child(self.role_picker(theme, true, cx))
                    .child(self.button(
                        theme,
                        "private-create-invitation",
                        "Create invitation",
                        !self.busy,
                        |this, cx| {
                            this.call(
                                methods::CREATE_PRIVATE_INVITATION,
                                json!({"role": this.invitation_role.wire()}),
                                None,
                                cx,
                            )
                        },
                        cx,
                    ))
                    .children(self.render_invitation(theme, cx));
            }
            content = content.child(widgets::field_label(theme, "Paired devices"));
            for node in nodes {
                let id = node.device_id.clone();
                let confirm = self.revoke.as_ref() == Some(&id);
                let local = own_id.as_deref() == Some(&id);
                let row = div().flex().items_center().gap(px(10.0)).py(px(8.0)).child(
                    div()
                        .flex_1()
                        .child(widgets::row_title(theme, node.name))
                        .child(widgets::page_subtitle(
                            theme,
                            format!(
                                "{} · paired {}{}",
                                node.role.label(),
                                node.paired_at.format("%Y-%m-%d"),
                                if local { " · this device" } else { "" }
                            ),
                        )),
                );
                content = content.child(row.when(!local, |row| {
                    row.child(
                        widgets::ghost_action(theme)
                            .id(SharedString::from(format!("revoke-{id}")))
                            .child(if confirm { "Confirm revoke" } else { "Revoke" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.busy {
                                    return;
                                }
                                if this.revoke.as_ref() == Some(&id) {
                                    this.call(
                                        methods::REVOKE_PRIVATE_NODE,
                                        json!({"deviceId": id}),
                                        None,
                                        cx,
                                    );
                                } else {
                                    this.revoke = Some(id.clone());
                                    cx.notify();
                                }
                            })),
                    )
                }));
            }
        } else {
            content = content.child(widgets::page_subtitle(
                theme,
                "Manage invitations and paired devices on the sync hub.",
            ));
        }
        if self.confirm_leave {
            content = content.child(widgets::page_subtitle(theme, if host_hub { "Leaving stops this hub's remote access. Saved private data and your original local workspace remain on disk." } else { "Leave this private workspace and return to local work? Saved workspace data remains on disk." }))
                .child(div().flex().gap(px(8.0))
                    .child(self.button(theme, "private-confirm-leave", "Leave and use Local", !self.busy && !self.switch_blocked(cx), |this, cx| this.call(methods::LEAVE_PRIVATE_WORKSPACE, json!({}), Some((WorkspaceScope::Local, false)), cx), cx))
                    .child(self.button(theme, "private-cancel-leave", "Cancel", !self.busy, |this, cx| { this.confirm_leave = false; cx.notify(); }, cx)));
        } else {
            content = content.child(self.button(
                theme,
                "private-leave",
                "Leave private workspace",
                !self.busy,
                |this, cx| {
                    this.confirm_leave = true;
                    cx.notify();
                },
                cx,
            ));
        }
        content.into_any_element()
    }

    fn render_unconfigured(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if self.state.read(cx).workspace_scope == Some(WorkspaceScope::Private) {
            return div()
                .mt(px(20.0))
                .child(widgets::page_subtitle(
                    theme,
                    "Private configuration was removed. Finish switching to Local.",
                ))
                .child(self.button(
                    theme,
                    "private-retry-local",
                    "Switch to Local",
                    !self.busy && !self.switch_blocked(cx),
                    |_, cx| {
                        cx.emit(WorkspaceEvent::Restart {
                            target: WorkspaceScope::Local,
                            import: false,
                        })
                    },
                    cx,
                ))
                .into_any_element();
        }
        if self.setup != Setup::Choose {
            return self.render_setup(theme, cx);
        }
        let local = self.state.read(cx).workspace_scope == Some(WorkspaceScope::Local);
        div().mt(px(20.0)).flex().flex_col().gap(px(10.0))
            .child(widgets::row_title(theme, "Private via Tailscale"))
            .child(widgets::page_subtitle(theme, "Synchronize through a hub you operate. Paired devices can control your agents. No Zeron account is needed."))
            .when(!local, |el| el.child(widgets::page_subtitle(theme, "Switch to Local before setting up a private workspace.")))
            .child(div().flex().gap(px(8.0))
                .child(self.button(theme, "private-create", "Create private workspace", local && !self.busy, |this, cx| { this.setup = Setup::Create; this.error = None; cx.notify(); }, cx))
                .child(self.button(theme, "private-join", "Join private workspace", local && !self.busy, |this, cx| { this.setup = Setup::Join; this.error = None; cx.notify(); }, cx)))
            .into_any_element()
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }
}

fn qr_code(qr: qrcode::QrCode) -> AnyElement {
    let width = qr.width();
    let module = (216.0 / (width + 8) as f32).floor();
    gpui::canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            window.paint_quad(gpui::fill(bounds, gpui::rgb(0xffffff)));
            for y in 0..width {
                for x in 0..width {
                    if qr[(x, y)] == qrcode::Color::Dark {
                        let origin = bounds.origin
                            + gpui::point(px((x + 4) as f32 * module), px((y + 4) as f32 * module));
                        window.paint_quad(gpui::fill(
                            gpui::Bounds::new(origin, gpui::size(px(module), px(module))),
                            gpui::rgb(0x000000),
                        ));
                    }
                }
            }
        },
    )
    .size(px((width + 8) as f32 * module))
    .into_any_element()
}

impl popover::ScrollRailHost for WorkspacePage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }
    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

impl Render for WorkspacePage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let scope = self.state.read(cx).workspace_scope;
        let local = scope == Some(WorkspaceScope::Local);
        let cloud = scope == Some(WorkspaceScope::Synced);
        let active = self.switch_blocked(cx);
        let private = match self.status.clone() {
            Loadable::Idle | Loadable::Loading => {
                widgets::page_subtitle(&theme, "Loading private workspace settings…")
                    .into_any_element()
            }
            Loadable::Error(error) => widgets::error_strip(&theme, error).into_any_element(),
            Loadable::Ready(status) => self.render_status(status, &theme, cx),
        };
        let content = widgets::page_column()
            .child(widgets::page_header(&theme, "Workspace", None))
            .child(widgets::page_subtitle(&theme, "Choose where your workspace is stored and how your devices connect."))
            .when_some(self.error.clone(), |el, error| el.child(widgets::error_strip(&theme, error)))
            .when_some(self.notice.clone(), |el, notice| el.child(widgets::page_subtitle(&theme, notice)))
            .when(active, |el| el.child(widgets::page_subtitle(&theme, "Finish active agent turns and save or close edited files before switching workspaces.")))
            .child(div().mt(px(24.0)).child(widgets::row_title(&theme, if local { "Local · current" } else { "Local" }))
                .child(widgets::page_subtitle(&theme, "Work stays on this device. Model providers still receive requests from your agents."))
                .when(cloud, |el| el.child(self.button(&theme, "workspace-use-local", "Use Local", !self.busy && !active, |_, cx| cx.emit(WorkspaceEvent::LeaveCloud), cx))))
            .child(private)
            .child(div().mt(px(24.0)).child(widgets::row_title(&theme, if cloud { "Zeron Cloud · current" } else { "Zeron Cloud" }))
                .child(widgets::page_subtitle(&theme, "Sign in to synchronize through Zeron's hosted service."))
                .when(local, |el| el.child(self.button(&theme, "workspace-use-cloud", "Set up Zeron Cloud", !self.busy && !active, |_, cx| cx.emit(WorkspaceEvent::StartCloud), cx)))
                .when(scope == Some(WorkspaceScope::Private), |el| el.child(widgets::page_subtitle(&theme, "Leave the private workspace to set up Zeron Cloud."))))
            .child(div().mt(px(20.0)).child(self.button(&theme, "private-status-refresh", "Refresh status", !self.busy, |this, cx| this.load(cx), cx)));
        let scrollbar = popover::rail(self, "workspace-scrollbar", &theme, cx);
        div()
            .id("workspace-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                div()
                    .id("workspace-page")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.scroll)
                    .child(content),
            )
            .children(scrollbar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invitation_link_preserves_hub_and_code_without_credentials() {
        let invitation = Invitation {
            code: "012345".into(),
            expires_at: Utc::now(),
            hub_url: "https://host.example.ts.net:8443".into(),
        };
        assert_eq!(
            invitation_link(&invitation),
            "zeron://private/join?hub=https%3A%2F%2Fhost.example.ts.net%3A8443&code=012345"
        );
    }

    #[test]
    fn status_requires_known_roles_and_complete_configuration() {
        assert!(serde_json::from_value::<PrivateStatus>(json!({"state":"unconfigured"})).is_ok());
        assert!(
            serde_json::from_value::<PrivateStatus>(
                json!({"state":"configured","workspaceId":"a"})
            )
            .is_err()
        );
        assert!(serde_json::from_value::<NodeRole>(json!("administrator")).is_err());
    }

    #[test]
    fn status_and_invitation_accept_hub_millisecond_timestamps() {
        let status: PrivateStatus = serde_json::from_value(json!({
            "state":"configured", "workspaceId":"w", "hubUrl":"https://hub.example.ts.net:8443", "name":"Workspace", "role":"server", "hostHub":true, "enabled":true,
            "nodes":[{"deviceId":"client", "name":"Client", "role":"client", "pairedAt":1_700_000_000_000_i64}]
        })).unwrap();
        let PrivateStatus::Configured { nodes, .. } = status else {
            panic!("expected configured workspace");
        };
        assert_eq!(nodes[0].paired_at.timestamp_millis(), 1_700_000_000_000);
        let invite: Invitation = serde_json::from_value(json!({"code":"012345","expiresAt":1_700_000_300_000_i64,"hubUrl":"https://hub.example.ts.net:8443"})).unwrap();
        assert_eq!(invite.expires_at.timestamp_millis(), 1_700_000_300_000);
    }
}
