//! Automatic session-title preferences, embedded in the unified Agents page.

use gpui::{AnyElement, Context, Entity, IntoElement, Render, Task, Window, div, prelude::*, px};

use zeron_engine::registry::TitleSettings;
use zeron_engine::registry::{HarnessDescriptor, descriptor_enabled};
use zeron_proto::HarnessId;
use zeron_proto::Model;
use zeron_rpc::methods;

use crate::popover::{self, Loadable};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

/// One-line blurb per agent (the t3code models page pairs every toggle row
/// with a description; the catalog descriptor doesn't carry one).
pub fn blurb(harness: HarnessId) -> &'static str {
    match harness {
        HarnessId::ClaudeCode => "Anthropic's coding agent, driven through the Claude Code CLI.",
        HarnessId::Codex => "OpenAI's coding agent, driven through the Codex CLI.",
        HarnessId::Cursor => "Cursor's coding agent, driven through the cursor-agent CLI.",
        HarnessId::Devin => "Cognition's Devin agent (devin CLI).",
        HarnessId::Grok => "xAI's Grok Build agent (grok CLI).",
        HarnessId::Hermes => "Nous Research's Hermes Agent (hermes CLI).",
        HarnessId::Pi => "The pi coding agent (pi CLI).",
        HarnessId::Opencode => "SST's opencode agent (opencode CLI).",
        HarnessId::Mock => "Scripted test harness.",
    }
}

/// The CLI named in the not-installed hint.
pub fn cli_name(harness: HarnessId) -> &'static str {
    match harness {
        HarnessId::ClaudeCode => "claude",
        HarnessId::Codex => "codex",
        HarnessId::Cursor => "cursor-agent",
        HarnessId::Devin => "devin",
        HarnessId::Grok => "grok",
        HarnessId::Hermes => "hermes",
        HarnessId::Pi => "pi",
        HarnessId::Opencode => "opencode",
        HarnessId::Mock => "mock",
    }
}

pub struct TitleSettingsSection {
    title_settings: Loadable<TitleSettings>,
    title_models: Loadable<Vec<Model>>,
    title_menu: Option<bool>, // false = harness, true = model
    title_task: Option<Task<()>>,
    title_saving: bool,
    state: Entity<AppState>,
    harnesses: Loadable<Vec<HarnessDescriptor>>,
    error: Option<String>,
    load_task: Option<Task<()>>,
}

impl TitleSettingsSection {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut page = Self {
            title_settings: Loadable::Idle,
            title_models: Loadable::Idle,
            title_menu: None,
            title_task: None,
            title_saving: false,
            state,
            harnesses: Loadable::Idle,
            error: None,
            load_task: None,
        };
        page.load(cx);
        page
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = serde_json::json!({});
        self.load_titles(None, cx);
        self.harnesses = Loadable::Loading;
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::LIST_HARNESSES, params).await;
            this.update(cx, |page, cx| {
                page.harnesses = match result {
                    Ok(value) => match serde_json::from_value::<Vec<HarnessDescriptor>>(value) {
                        Ok(list) => Loadable::Ready(list),
                        Err(err) => Loadable::Error(err.to_string()),
                    },
                    Err(err) => Loadable::Error(err.to_string()),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    fn load_titles(&mut self, save: Option<TitleSettings>, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let saving = save.is_some();
        let method = if saving {
            methods::SET_TITLE_SETTINGS
        } else {
            methods::GET_TITLE_SETTINGS
        };
        let params = save
            .map(|s| serde_json::to_value(s).unwrap())
            .unwrap_or_else(|| serde_json::json!({}));
        self.title_menu = None;
        self.title_saving = saving;
        self.title_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(method, params)
                .await
                .map_err(|e| e.to_string())
                .and_then(|v| {
                    serde_json::from_value::<TitleSettings>(v).map_err(|e| e.to_string())
                });
            let settings = match result {
                Ok(settings) => settings,
                Err(error) => {
                    this.update(cx, |page, cx| {
                        if saving {
                            page.error = Some(error);
                        } else {
                            page.title_settings = Loadable::Error(error);
                        }
                        page.title_saving = false;
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let harness = settings.harness;
            this.update(cx, |page, cx| {
                page.title_settings = Loadable::Ready(settings);
                page.title_models = if harness.is_some() {
                    Loadable::Loading
                } else {
                    Loadable::Idle
                };
                page.title_saving = false;
                page.error = None;
                cx.notify();
            })
            .ok();
            if let Some(harness) = harness {
                let result = engine
                    .client()
                    .call(
                        methods::LIST_MODELS,
                        serde_json::json!({"harness": harness}),
                    )
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|v| {
                        serde_json::from_value::<Vec<Model>>(v).map_err(|e| e.to_string())
                    });
                this.update(cx, |page, cx| {
                    page.title_models = match result {
                        Ok(models) => Loadable::Ready(models),
                        Err(error) => Loadable::Error(error),
                    };
                    cx.notify();
                })
                .ok();
            }
        }));
        cx.notify();
    }

    fn render_titles(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut card = widgets::section_card(theme).mt(px(20.0)).p(px(16.0))
            .child(widgets::row_title(theme, "Session titles"))
            .child(widgets::page_subtitle(theme, "Choose the agent and model for automatic titles on this device. Claude Code and Codex support restricted title generation."));
        let Loadable::Ready(settings) = &self.title_settings else {
            let message = match &self.title_settings {
                Loadable::Error(error) => error.clone(),
                _ => "Loading title settings…".into(),
            };
            return card
                .child(div().mt(px(8.0)).child(message))
                .into_any_element();
        };
        for is_model in [false, true] {
            let label = if is_model {
                settings
                    .model
                    .as_ref()
                    .map(|id| {
                        if let Loadable::Ready(models) = &self.title_models {
                            models
                                .iter()
                                .find(|m| &m.id == id)
                                .map(|m| m.label.clone())
                                .unwrap_or_else(|| id.clone())
                        } else {
                            id.clone()
                        }
                    })
                    .unwrap_or_else(|| "Automatic (cheapest model)".into())
            } else {
                settings
                    .harness
                    .map(|id| match id {
                        HarnessId::ClaudeCode => "Claude Code".to_string(),
                        HarnessId::Codex => "Codex".to_string(),
                        _ => format!("{id:?}"),
                    })
                    .unwrap_or_else(|| "Automatic (session agent when supported)".into())
            };
            let interactive = !self.title_saving && (!is_model || settings.harness.is_some());
            let mut row = div()
                .mt(px(12.0))
                .child(widgets::row_title(
                    theme,
                    if is_model {
                        "Title model"
                    } else {
                        "Title harness"
                    },
                ))
                .child(
                    widgets::ghost_action(theme)
                        .id(if is_model {
                            "title-model"
                        } else {
                            "title-harness"
                        })
                        .when(interactive, |el| {
                            el.cursor_pointer()
                                .on_click(cx.listener(move |page, _, _, cx| {
                                    page.title_menu = if page.title_menu == Some(is_model) {
                                        None
                                    } else {
                                        Some(is_model)
                                    };
                                    cx.notify();
                                }))
                        })
                        .when(!interactive, |el| el.opacity(0.5))
                        .child(label),
                );
            if self.title_menu == Some(is_model) {
                let mut choices = vec![(
                    "Automatic".to_string(),
                    TitleSettings {
                        harness: if is_model { settings.harness } else { None },
                        model: None,
                    },
                )];
                if is_model {
                    if let Loadable::Ready(models) = &self.title_models {
                        choices.extend(models.iter().map(|m| {
                            (
                                m.label.clone(),
                                TitleSettings {
                                    harness: settings.harness,
                                    model: Some(m.id.clone()),
                                },
                            )
                        }));
                    }
                } else if let Loadable::Ready(harnesses) = &self.harnesses {
                    choices.extend(
                        harnesses
                            .iter()
                            .filter(|h| {
                                descriptor_enabled(h)
                                    && h.installed
                                    && zeron_harness::supports_titles(h.id)
                                    && h.id != HarnessId::Mock
                            })
                            .map(|h| {
                                (
                                    h.name.clone(),
                                    TitleSettings {
                                        harness: Some(h.id),
                                        model: None,
                                    },
                                )
                            }),
                    );
                }
                row =
                    row.child(
                        div()
                            .id(if is_model {
                                "title-model-options"
                            } else {
                                "title-harness-options"
                            })
                            .max_h(px(240.0))
                            .overflow_y_scroll()
                            .children(choices.into_iter().enumerate().map(
                                |(ix, (label, choice))| {
                                    popover::menu_row(
                                        theme,
                                        &choice == settings,
                                        format!("title-choice-{is_model}-{ix}"),
                                    )
                                    .id(("title-choice", ix))
                                    .on_click(cx.listener(move |page, _, _, cx| {
                                        page.load_titles(Some(choice.clone()), cx)
                                    }))
                                    .child(label)
                                },
                            )),
                    );
            }
            card = card.child(row);
        }
        if let Loadable::Error(error) = &self.title_models {
            card = card.child(widgets::error_strip(theme, error.clone()));
        }
        card.into_any_element()
    }
}

impl Render for TitleSettingsSection {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        div()
            .mt(px(24.0))
            .when_some(self.error.clone(), |el, e| {
                el.child(widgets::error_strip(&theme, e))
            })
            .child(self.render_titles(&theme, cx))
    }
}
