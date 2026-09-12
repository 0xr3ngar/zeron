//! One harness, all devices. Installation state is always read from its execution host.
use super::{accounts::PROVIDERS, widgets};
use crate::{composer::ComposerInput, popover::Loadable, state::AppState, theme::Theme};
use gpui::{
    AnyElement, Context, Entity, IntoElement, Render, Subscription, Task, Window, div, prelude::*,
    px,
};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};
use zeron_engine::registry::{HarnessDescriptor, descriptor_enabled};
use zeron_harness::installations::Installation;
use zeron_proto::HarnessId;
use zeron_rpc::methods;

#[derive(Clone, Deserialize)]
struct Catalog {
    harnesses: Vec<HarnessDescriptor>,
    installations: Vec<Installation>,
}
#[derive(Clone)]
struct Device {
    id: String,
    name: String,
    online: bool,
    local: bool,
}

pub struct HarnessesPage {
    state: Entity<AppState>,
    expanded: HashSet<HarnessId>,
    catalogs: HashMap<String, Loadable<Catalog>>,
    menu: Option<(HarnessId, String)>,
    path: Entity<ComposerInput>,
    busy: bool,
    loading: bool,
    error: Option<String>,
    load_task: Option<Task<()>>,
    _poll: Task<()>,
    _observe: Subscription,
    titles: Entity<super::harnesses::TitleSettingsSection>,
}
impl HarnessesPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |_, _, cx| cx.notify());
        let titles = cx.new(|cx| super::harnesses::TitleSettingsSection::new(state.clone(), cx));
        let path = cx.new(|cx| ComposerInput::new("Absolute path to executable", cx));
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(3)).await;
                if this
                    .update(cx, |page, cx| {
                        let active = page
                            .catalogs
                            .values()
                            .filter_map(|s| s.ready())
                            .any(|c| c.installations.iter().any(|i| i.installing));
                        if active
                            || page
                                .devices(cx)
                                .iter()
                                .any(|d| d.online && !page.catalogs.contains_key(&d.id))
                        {
                            page.load(cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut page = Self {
            state,
            expanded: HashSet::from([HarnessId::Codex]),
            catalogs: HashMap::new(),
            menu: None,
            path,
            busy: false,
            loading: false,
            error: None,
            load_task: None,
            _poll: poll,
            _observe: observe,
            titles,
        };
        page.load(cx);
        page
    }
    fn devices(&self, cx: &Context<Self>) -> Vec<Device> {
        let state = self.state.read(cx);
        let now = chrono::Utc::now();
        let mut result = state
            .devices
            .iter()
            .map(|d| Device {
                id: d.id.clone(),
                name: d.name.clone(),
                local: state.local_device_id.as_deref() == Some(d.id.as_str()),
                online: state.local_device_id.as_deref() == Some(d.id.as_str())
                    || super::devices::device_online(d.last_seen_at, now),
            })
            .collect::<Vec<_>>();
        if let Some(id) = &state.local_device_id
            && !result.iter().any(|d| &d.id == id)
        {
            result.push(Device {
                id: id.clone(),
                name: "This device".into(),
                local: true,
                online: true,
            });
        }
        result.sort_by(|a, b| b.local.cmp(&a.local).then(a.name.cmp(&b.name)));
        result
    }
    fn load(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let devices = self
            .devices(cx)
            .into_iter()
            .filter(|d| d.online)
            .collect::<Vec<_>>();
        for d in &devices {
            self.catalogs
                .entry(d.id.clone())
                .or_insert(Loadable::Loading);
        }
        self.loading = true;
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let results = futures::future::join_all(devices.into_iter().map(|d| {
                let engine = engine.clone();
                async move {
                    let result = engine
                        .client()
                        .call(
                            methods::LIST_HARNESS_INSTALLATIONS,
                            serde_json::json!({"targetDeviceId":d.id}),
                        )
                        .await
                        .map_err(|e| e.to_string())
                        .and_then(|v| {
                            serde_json::from_value::<Catalog>(v).map_err(|e| e.to_string())
                        });
                    (d.id, result)
                }
            }))
            .await;
            this.update(cx, |page, cx| {
                for (id, result) in results {
                    page.catalogs.insert(
                        id,
                        match result {
                            Ok(c) => Loadable::Ready(c),
                            Err(e) => Loadable::Error(e),
                        },
                    );
                }
                page.loading = false;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
    fn action(
        &mut self,
        h: HarnessId,
        device: String,
        action: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let mut params = action;
        params["harness"] = serde_json::to_value(h).unwrap();
        params["targetDeviceId"] = device.clone().into();
        let method = if params.get("enabled").is_some() {
            methods::SET_HARNESS_ENABLED
        } else {
            methods::MANAGE_HARNESS_INSTALLATION
        };
        self.busy = true;
        self.error = None;
        self.menu = None;
        cx.spawn(async move |this, cx| {
            let result = engine.client().call(method, params).await;
            this.update(cx, |page, cx| {
                page.busy = false;
                crate::pickers::bump_harness_catalog(cx);
                if let Err(e) = result {
                    page.error = Some(e.to_string());
                }
                page.load(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
    #[cfg(feature = "browser-fixture")]
    pub(crate) fn fixture_expand(&mut self, h: HarnessId, cx: &mut Context<Self>) {
        self.expanded.insert(h);
        cx.notify();
    }

    #[cfg(feature = "browser-fixture")]
    pub(crate) fn fixture_manage(&mut self, h: HarnessId, device: String, cx: &mut Context<Self>) {
        self.expanded.insert(h);
        self.menu = Some((h, device));
        cx.notify();
    }
    #[cfg(feature = "browser-fixture")]
    pub(crate) fn fixture_refresh(&mut self, cx: &mut Context<Self>) {
        self.load(cx);
    }

    fn device_row(
        &self,
        h: HarnessId,
        d: &Device,
        index: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let catalog = self.catalogs.get(&d.id);
        let ready = catalog.and_then(|c| c.ready());
        let install = ready.and_then(|c| c.installations.iter().find(|i| i.harness == h));
        let enabled = ready
            .and_then(|c| c.harnesses.iter().find(|v| v.id == h))
            .is_some_and(descriptor_enabled);
        let working = install.is_some_and(|i| i.installing);
        let available = d.online && !self.busy && !working && ready.is_some();
        let status = if !d.online {
            match install.filter(|i| i.installed) {
                Some(i) => format!(
                    "Offline · Last seen with {}",
                    i.version.as_deref().unwrap_or("an unknown version")
                ),
                None => "Offline · Installation status unavailable".into(),
            }
        } else if working {
            "Installing…".into()
        } else if let Some(i) = install {
            if i.installed {
                format!(
                    "{} · {}",
                    i.version.as_deref().unwrap_or("Version unavailable"),
                    if i.managed {
                        "Managed by Zeron"
                    } else {
                        "Existing installation"
                    }
                )
            } else {
                "Not installed".into()
            }
        } else if matches!(catalog, Some(Loadable::Error(_))) {
            "Couldn’t reach this device".into()
        } else {
            "Checking installation…".into()
        };
        let id = d.id.clone();
        let manage_id = id.clone();
        let toggle_id = id.clone();
        let menu = self
            .menu
            .as_ref()
            .is_some_and(|(which, device)| *which == h && device == &d.id);
        let install_label = if install.is_some_and(|i| i.installed) {
            "Manage"
        } else if install.is_some_and(|i| i.can_install) {
            "Install"
        } else {
            "Choose installation"
        };
        let can_install = install.is_some_and(|i| i.can_install);
        let row = div()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(18.))
            .py(px(14.))
            .child(div().size(px(7.)).rounded_full().bg(if d.online {
                theme.accent
            } else {
                theme.text_muted
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .items_center()
                            .child(widgets::row_title(theme, d.name.clone()))
                            .when(d.local, |el| {
                                el.child(
                                    div()
                                        .text_size(crate::typography::ui_rems(10.))
                                        .text_color(theme.text_muted)
                                        .child("This device"),
                                )
                            }),
                    )
                    .child(
                        div()
                            .mt(px(4.))
                            .text_size(crate::typography::ui_rems(11.))
                            .text_color(theme.text_muted)
                            .child(status),
                    ),
            )
            .when(d.online, |el| {
                el.child(
                    widgets::ghost_action(theme)
                        .id(("runtime-manage", index))
                        .opacity(if available { 1. } else { 0.5 })
                        .when(available, |el| {
                            el.on_click(cx.listener(move |page, _, _, cx| {
                                if install_label.eq("Install") && can_install {
                                    page.action(
                                        h,
                                        manage_id.clone(),
                                        serde_json::json!({"action":"install"}),
                                        cx,
                                    );
                                } else {
                                    page.menu = if menu {
                                        None
                                    } else {
                                        Some((h, manage_id.clone()))
                                    };
                                    page.path.update(cx, |input, cx| input.set_text("", cx));
                                    cx.notify();
                                }
                            }))
                        })
                        .child(if working {
                            "Installing…"
                        } else {
                            install_label
                        }),
                )
            })
            .child(
                widgets::toggle_switch(theme, enabled)
                    .id(("runtime-enabled", index))
                    .opacity(if available { 1. } else { 0.4 })
                    .when(available && install.is_some_and(|i| i.installed), |el| {
                        el.cursor_pointer()
                            .on_click(cx.listener(move |page, _, _, cx| {
                                page.action(
                                    h,
                                    toggle_id.clone(),
                                    serde_json::json!({"enabled":!enabled}),
                                    cx,
                                );
                            }))
                    }),
            );
        let mut body = div().border_t_1().border_color(theme.border).child(row);
        if let Some(error) = install.and_then(|i| i.error.as_ref()) {
            body = body.child(
                div()
                    .px(px(18.))
                    .pb(px(10.))
                    .child(widgets::error_strip(theme, error.clone())),
            );
        }
        if menu && available {
            let existing_id = id.clone();
            let rollback_id = id.clone();
            let custom_id = id.clone();
            body=body.child(div().px(px(18.)).pb(px(16.)).flex().flex_col().gap(px(12.))
                .child(div().flex().gap(px(8.))
                    .when(can_install,|el|el.child(widgets::ghost_action(theme).id(("runtime-install",index))
                        .on_click(cx.listener(move |page,_,_,cx|page.action(h,id.clone(),serde_json::json!({"action":"install"}),cx)))
                        .child(format!("Install tested version {}",install.and_then(|i|i.recommended_version.as_deref()).unwrap_or("")))))
                    .when(h != HarnessId::Cursor, |el| el.child(widgets::ghost_action(theme).id(("runtime-existing",index)).on_click(cx.listener(move |page,_,_,cx|page.action(h,existing_id.clone(),serde_json::json!({"action":"useExisting"}),cx))).child("Use detected installation")))
                    .when(install.is_some_and(|i|i.previous_version.is_some()),|el|el.child(widgets::ghost_action(theme).id(("runtime-rollback",index))
                        .on_click(cx.listener(move |page,_,_,cx|page.action(h,rollback_id.clone(),serde_json::json!({"action":"rollback"}),cx))).child("Roll back"))))
                .when(h != HarnessId::Cursor, |el| el.child(div().flex().gap(px(8.)).items_center().child(div().flex_1().child(self.path.clone()))
                    .child(widgets::ghost_action(theme).id(("runtime-path",index)).on_click(cx.listener(move |page,_,_,cx|{
                        let path=page.path.read(cx).text().trim().to_string();
                        if !path.is_empty() {page.action(h,custom_id.clone(),serde_json::json!({"action":"useExisting","path":path}),cx);}
                    })).child("Use path"))))
                .child(div().text_size(crate::typography::ui_rems(11.)).text_color(theme.text_muted).child("Updates apply to new sessions. Your accounts, plugins, and configuration stay separate.")));
        }
        body.into_any_element()
    }
}
impl Render for HarnessesPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let devices = self.devices(cx);
        let rows = PROVIDERS
            .into_iter()
            .enumerate()
            .map(|(ix, (h, name, _))| {
                let open = self.expanded.contains(&h);
                let (icon, tint) = crate::pickers::harness_brand_icon(h);
                let installed = devices
                    .iter()
                    .filter(|d| {
                        self.catalogs
                            .get(&d.id)
                            .and_then(|c| c.ready())
                            .is_some_and(|c| {
                                c.installations
                                    .iter()
                                    .any(|i| i.harness == h && i.installed)
                            })
                    })
                    .count();
                widgets::section_card(&theme)
                    .mt(px(0.))
                    .mb(px(10.))
                    .child(
                        div()
                            .id(("harness-accordion", ix))
                            .px(px(18.))
                            .py(px(17.))
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .cursor_pointer()
                            .on_click(cx.listener(move |page, _, _, cx| {
                                if !page.expanded.remove(&h) {
                                    page.expanded.insert(h);
                                }
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .size(px(34.))
                                    .rounded(px(9.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(crate::theme::ink(0.035))
                                    .child(
                                        crate::icons::icon(icon)
                                            .size(px(18.))
                                            .text_color(tint.unwrap_or(theme.text)),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .child(widgets::row_title(&theme, name))
                                    .child(
                                        div()
                                            .mt(px(3.))
                                            .text_size(crate::typography::ui_rems(11.5))
                                            .text_color(theme.text_muted)
                                            .child(format!(
                                                "Installed on {installed} of {} devices",
                                                devices.len()
                                            )),
                                    ),
                            )
                            .child(
                                crate::icons::icon(if open {
                                    crate::icons::ALT_ARROW_DOWN
                                } else {
                                    crate::icons::ALT_ARROW_RIGHT
                                })
                                .size(px(14.))
                                .text_color(theme.text_muted),
                            ),
                    )
                    .when(open, |el| {
                        el.children(
                            devices
                                .iter()
                                .enumerate()
                                .map(|(di, d)| self.device_row(h, d, ix * 1000 + di, &theme, cx)),
                        )
                    })
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        div()
            .id("harnesses-page")
            .size_full()
            .overflow_y_scroll()
            .child(
                widgets::page_column()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(widgets::page_header(&theme, "Harnesses", None))
                            .child(div().flex_1())
                            .child(
                                widgets::ghost_action(&theme)
                                    .id("harnesses-refresh")
                                    .on_click(cx.listener(|page, _, _, cx| page.load(cx)))
                                    .child("Refresh"),
                            ),
                    )
                    .child(widgets::page_subtitle(
                        &theme,
                        "Install and enable your harnesses on each connected device.",
                    ))
                    .when_some(self.error.clone(), |el, e| {
                        el.child(widgets::error_strip(&theme, e))
                    })
                    .child(div().mt(px(24.)).children(rows))
                    .child(self.titles.clone()),
            )
    }
}
