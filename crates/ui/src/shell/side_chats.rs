use super::*;

pub(super) struct SideChatTab {
    pub state: Entity<AppState>,
    transcript: Entity<Transcript>,
    pub(super) composer: Entity<Composer>,
    _events: Vec<Subscription>,
}

impl Shell {
    pub(super) fn close_empty_right_pane(&mut self, key: &str, cx: &mut Context<Self>) {
        let empty = if key == self.panel_key(cx) {
            self.right_surface_rows(cx).is_empty()
        } else {
            self.right_tabs.get(key).is_none_or(Vec::is_empty)
        };
        if empty {
            if key == self.panel_key(cx) && self.right_pane_open(cx) {
                self.toggle_right_pane(cx);
            } else {
                self.panels.update(key, |p| p.changes_open = false);
            }
        }
    }

    pub(super) fn create_side_chat(&mut self, cx: &mut Context<Self>) {
        if self.side_chat_creating {
            return;
        }
        let state = self.state.read(cx);
        let Some(source) = state.selected_chat_row().cloned() else {
            self.side_chat_error = Some("Start a conversation before creating a side chat.".into());
            cx.notify();
            return;
        };
        let Some(engine) = state.engine().cloned() else {
            return;
        };
        let key = self.panel_key(cx);
        self.side_chat_creating = true;
        self.side_chat_error = None;
        let params = serde_json::json!({
            "chatId": uuid::Uuid::new_v4().to_string(),
            "sourceChatId": source.id,
            "targetDeviceId": source.device_id,
        });
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call_as::<zeron_proto::Chat>(methods::FORK_SIDE_CHAT, params)
                .await;
            let _ = this.update(cx, |this, cx| {
                this.side_chat_creating = false;
                match result {
                    Ok(chat) => {
                        if this
                            .state
                            .read(cx)
                            .chats
                            .iter()
                            .any(|c| Some(&c.id) == chat.parent_chat_id.as_ref())
                        {
                            this.open_side_chat(chat, key, cx);
                        }
                    }
                    Err(error) => this.side_chat_error = Some(error.to_string().into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_side_chat(&mut self, chat: zeron_proto::Chat, key: String, cx: &mut Context<Self>) {
        if let Some((&id, _)) = self
            .side_chats
            .iter()
            .find(|(_, tab)| tab.state.read(cx).selected_chat.as_deref() == Some(&chat.id))
        {
            self.set_right_active(RightSurface::SideChat(id), cx);
            return;
        }
        let parent = self.state.clone();
        let state = cx.new(|cx| AppState::side_chat_state(&parent, chat, cx));
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        let composer = cx.new(|cx| Composer::new(state.clone(), cx));
        let events = vec![
            cx.subscribe(&transcript, Self::on_transcript_event),
            cx.subscribe(&composer, {
                let transcript = transcript.clone();
                move |_: &mut Self, _, event, cx| {
                    transcript.update(cx, |t, cx| match event {
                        ComposerEvent::Sent {
                            chat_id,
                            message_id,
                        } => t.on_own_send(chat_id.clone(), message_id.clone(), cx),
                        ComposerEvent::Queued {
                            chat_id,
                            message_id,
                        } => t.on_own_queued_send(chat_id.clone(), message_id.clone(), cx),
                    })
                }
            }),
            cx.observe(&state, |_, _, cx| cx.notify()),
        ];
        self.side_chat_seq += 1;
        let id = self.side_chat_seq;
        self.side_chats.insert(
            id,
            SideChatTab {
                state,
                transcript,
                composer,
                _events: events,
            },
        );
        self.right_tabs
            .entry(key.clone())
            .or_default()
            .push(RightSurface::SideChat(id));
        self.panels
            .update(&key, |p| p.right_active = RightSurface::SideChat(id));
        cx.notify();
    }

    pub(super) fn side_chat_history(&self, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let chats: Vec<_> = state
            .chats
            .iter()
            .filter(|chat| {
                chat.parent_chat_id.is_some() && chat.parent_chat_id == state.selected_chat
            })
            .cloned()
            .collect();
        let theme = Theme::of(cx);
        let mut list = div()
            .id("side-chat-history")
            .max_h(px(220.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(4.0));
        if !chats.is_empty() {
            list = list.child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child("Previous side chats"),
            );
        }
        for chat in chats {
            let title = chat
                .title
                .clone()
                .or(chat.last_message_preview.clone())
                .unwrap_or_else(|| "New side chat".into());
            let time = format_time_ago(chat.last_message_at.unwrap_or(chat.created_at), Utc::now());
            list = list.child(
                div()
                    .id(SharedString::from(format!("side-chat-{}", chat.id)))
                    .p(px(8.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .text_color(theme.text)
                    .hover(|s| s.bg(crate::theme::ink(0.05)))
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(13.0))
                            .truncate()
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_muted)
                            .child(time),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_side_chat(chat.clone(), this.panel_key(cx), cx);
                    })),
            );
        }
        list.into_any_element()
    }

    pub(super) fn render_side_chat(&mut self, id: u64, cx: &mut Context<Self>) -> AnyElement {
        let Some(tab) = self
            .side_chats
            .get(&id)
            .filter(|tab| tab.state.read(cx).selected_chat.is_some())
        else {
            return self.render_surface_picker(cx);
        };
        let transcript = tab.transcript.clone();
        let composer = tab.composer.clone();
        let source_title = self
            .state
            .read(cx)
            .selected_chat_row()
            .and_then(|chat| chat.title.clone())
            .unwrap_or_else(|| "Main chat".into());
        let pill = transcript.read(cx).jump_button_shown().then(|| {
            div()
                .absolute()
                .bottom(px(12.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(self.jump_pill(
                    "side-chat-jump",
                    "side-chat-jump-pill",
                    transcript.clone(),
                    cx,
                ))
        });
        let theme = Theme::of(cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                crate::surface_chrome::toolbar(theme)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.text_muted)
                            .child(format!("From {source_title}")),
                    )
                    .child(
                        div()
                            .id("side-chat-history-button")
                            .cursor_pointer()
                            .child("Side chats")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_right_active(RightSurface::Picker, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(transcript)
                    .children(pill),
            )
            .child(div().flex_none().child(composer))
            .into_any_element()
    }
}
