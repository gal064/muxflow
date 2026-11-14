//! The History overlay, the woke placeholders at the top of the Agents list,
//! and the Snooze menu (docs/design/chat-mock.html, "History" and
//! "Snooze"). The overlay follows Quick Open: a search field, rows in groups,
//! arrows and Enter.

use std::cmp::Reverse;

use chrono::{DateTime, Local, TimeZone};
use desktop_core::connection::history::HistoryEntry;
use gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, Focusable, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Render, Role, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, Subscription, TestSupportExt, Window,
    component::{
        Icon, Sizable, WindowExt,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputEvent, InputState, MoveDown, MoveUp},
        v_flex,
    },
    div,
    prelude::FluentBuilder,
    transparent_black,
};

use crate::{
    commands::{
        dialogs::{
            TextPromptRequest, escape_ends_composition, is_composing, open_overlay,
            open_text_prompt,
        },
        quick_open::quick_open_score,
    },
    context_menu::{Entry, Item},
    theme::{
        self,
        metrics::{
            RADIUS_CONTROL, RADIUS_PANEL, TYPE_LABEL, TYPE_MENU, TYPE_META, TYPE_SECONDARY, ui,
        },
    },
    ui::icons::Glyph,
};

use super::{
    Agents,
    history::{self, History, HostHistory, entry_title},
    labels::adapter_fallback,
    model::{AgentRecord, DisplayState, Lifecycle, display_state},
    open_pane, report,
    snooze::{self, WakeTime},
    view::agent_mark,
};

const WIDTH: f32 = 680.;
const LIST_MAX_HEIGHT: f32 = 448.;
const VIEWPORT_LIMIT: f32 = 0.55;
const MAX_QUERY_LENGTH: usize = 256;

/// The overlay's two groups, after search: snoozed sessions by wake time, then
/// every other session, newest first. A search ranks within each group.
pub fn grouped<'a>(
    entries: &'a [HistoryEntry],
    query: &str,
) -> (Vec<&'a HistoryEntry>, Vec<&'a HistoryEntry>) {
    let query = query.trim();
    let mut scored: Vec<(i64, &HistoryEntry)> = entries
        .iter()
        .filter_map(|entry| {
            if query.is_empty() {
                return Some((0, entry));
            }
            let detail = format!(
                "{} {}",
                entry.workspace_name,
                adapter_fallback(&entry.adapter_id)
            );
            quick_open_score(&entry_title(entry), &detail, query).map(|score| (score, entry))
        })
        .collect();
    // Stable: equal scores keep the host's newest-first order.
    scored.sort_by_key(|(score, _)| Reverse(*score));
    let (snoozed, sessions): (Vec<_>, Vec<_>) = scored
        .into_iter()
        .map(|(_, entry)| entry)
        .partition(|entry| entry.snoozed_until_unix_millis != 0);
    let mut snoozed = snoozed;
    if query.is_empty() {
        snoozed.sort_by_key(|entry| entry.snoozed_until_unix_millis);
    }
    (snoozed, sessions)
}

/// "running", "woke", "14:05", "yesterday", "Mon", "12 Oct" or "12 Oct 2025".
pub fn when<Tz: TimeZone>(entry: &HistoryEntry, now: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    if entry.running {
        return "running".into();
    }
    if entry.woke {
        return "woke".into();
    }
    let Some(at) = now
        .timezone()
        .timestamp_millis_opt(entry.last_active_at_unix_millis)
        .single()
    else {
        return String::new();
    };
    let days = (now.date_naive() - at.date_naive()).num_days();
    match days {
        0 => at.format("%H:%M").to_string(),
        1 => "yesterday".into(),
        2..=6 => at.format("%a").to_string(),
        _ if at.format("%Y").to_string() == now.format("%Y").to_string() => {
            at.format("%-d %b").to_string()
        }
        _ => at.format("%-d %b %Y").to_string(),
    }
}

fn meta(entry: &HistoryEntry, now: &DateTime<Local>) -> String {
    let when = if entry.snoozed_until_unix_millis != 0 {
        snooze::wake_label(entry.snoozed_until_unix_millis, now)
    } else {
        when(entry, now)
    };
    [entry.workspace_name.as_str(), when.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The live agent behind a running entry.
fn live_agent<'a>(profile_id: &str, entry: &HistoryEntry, cx: &'a App) -> Option<&'a AgentRecord> {
    let agents = cx.try_global::<Agents>()?;
    agents
        .runtime
        .read(cx)
        .agents(profile_id)
        .into_iter()
        .find(|agent| {
            agent.id == entry.agent_id
                || agent.adapter_id == entry.adapter_id
                    && agent.native_session_id == entry.native_session_id
        })
}

/// Enter on a session: its tab if it runs, else a resume.
fn open_entry(profile_id: &str, entry: &HistoryEntry, window: &mut Window, cx: &mut App) {
    if entry.running {
        let pane = live_agent(profile_id, entry, cx).map(|agent| agent.pane_id.clone());
        match pane.filter(|pane| !pane.is_empty()) {
            Some(pane) => {
                if let Err(reason) = open_pane(profile_id, &pane, cx) {
                    report(
                        cx,
                        format!("{}'s pane is not available: {reason}.", entry_title(entry)),
                    );
                } else if let Some(workspaces) = crate::workspaces::current(cx) {
                    crate::workspaces::focus_terminal(&workspaces, window, cx);
                }
            }
            None => report(
                cx,
                format!(
                    "{} is running, but its pane is not known here.",
                    entry_title(entry)
                ),
            ),
        }
        return;
    }
    history::resume(profile_id, entry, cx);
}

pub struct HistoryOverlay {
    input: Entity<InputState>,
    scroll: ScrollHandle,
    /// The host it lists: the one on screen when it opened.
    profile_id: String,
    /// The selected row, by session, so it stays put while the list changes.
    selected: Option<(String, String)>,
    _subscriptions: Vec<Subscription>,
}

impl HistoryOverlay {
    fn new(profile_id: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search sessions…")
                .validate(|value, _| value.chars().count() <= MAX_QUERY_LENGTH)
        });
        let mut subscriptions = vec![cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.selected = None;
                this.scroll.scroll_to_item(0);
                cx.notify();
            }
        })];
        if let Some(history) = cx.try_global::<History>().map(|history| history.0.clone()) {
            subscriptions.push(cx.observe(&history, |_, _, cx| cx.notify()));
            // Clients list on open (chat-plan §4).
            history.update(cx, |history, cx| history.refresh(&profile_id, cx));
        }
        Self {
            input,
            scroll: ScrollHandle::new(),
            profile_id,
            selected: None,
            _subscriptions: subscriptions,
        }
    }

    fn entries(&self, cx: &App) -> (bool, Vec<HistoryEntry>) {
        self.host(cx)
            .map_or((false, Vec::new()), |host| (host.loaded, host.entries))
    }

    fn host(&self, cx: &App) -> Option<HostHistory> {
        cx.try_global::<History>()
            .and_then(|history| history.0.read(cx).host(&self.profile_id))
            .cloned()
    }

    /// The rows in drawing order.
    fn ordered(&self, cx: &App) -> Vec<HistoryEntry> {
        let (_, entries) = self.entries(cx);
        let query = self.input.read(cx).value().to_string();
        let (snoozed, sessions) = grouped(&entries, &query);
        snoozed.into_iter().chain(sessions).cloned().collect()
    }

    fn active_index(&self, rows: &[HistoryEntry]) -> Option<usize> {
        match &self.selected {
            Some((adapter, session)) => rows
                .iter()
                .position(|entry| {
                    entry.adapter_id == *adapter && entry.native_session_id == *session
                })
                .or_else(|| (!rows.is_empty()).then_some(0)),
            None => (!rows.is_empty()).then_some(0),
        }
    }

    fn step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if is_composing(&self.input, window, cx) {
            return;
        }
        cx.stop_propagation();
        let rows = self.ordered(cx);
        let Some(index) = self.active_index(&rows) else {
            return;
        };
        let next = index
            .saturating_add_signed(delta)
            .min(rows.len().saturating_sub(1));
        self.selected = Some((
            rows[next].adapter_id.clone(),
            rows[next].native_session_id.clone(),
        ));
        self.scroll.scroll_to_item(next);
        cx.notify();
    }

    fn choose(&mut self, entry: HistoryEntry, window: &mut Window, cx: &mut Context<Self>) {
        window.close_dialog(cx);
        open_entry(&self.profile_id, &entry, window, cx);
    }

    fn choose_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if is_composing(&self.input, window, cx) {
            return;
        }
        let rows = self.ordered(cx);
        if let Some(entry) = self.active_index(&rows).map(|index| rows[index].clone()) {
            self.choose(entry, window, cx);
        }
    }

    fn render_row(
        &self,
        index: usize,
        entry: &HistoryEntry,
        selected: bool,
        query: &str,
        now: &DateTime<Local>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        let glyphs = crate::settings::get(cx).agent_state_glyphs;
        let state = live_agent(&self.profile_id, entry, cx)
            .filter(|_| entry.running)
            .map_or(DisplayState::Idle, display_state);
        let title = entry_title(entry);
        let key = format!("{}-{}", entry.adapter_id, entry.native_session_id);
        let snoozed = entry.snoozed_until_unix_millis != 0;
        let chosen = entry.clone();
        let hovered = (entry.adapter_id.clone(), entry.native_session_id.clone());
        let woken = entry.clone();
        let profile = self.profile_id.clone();
        h_flex()
            .id(SharedString::from(format!("history-row-{key}")))
            .role(Role::ListBoxOption)
            .aria_label(SharedString::from(format!("{title}, {}", meta(entry, now))))
            .aria_selected(selected)
            .flex_none()
            .min_h(ui(32.))
            .px(ui(9.))
            .gap(ui(9.))
            .rounded(ui(RADIUS_CONTROL))
            .border_1()
            .border_color(transparent_black())
            .when(selected, |row| {
                row.bg(colors.accent_wash)
                    .text_color(colors.chrome_ink_strong)
                    .border_color(colors.accent)
            })
            .when(!selected, |row| {
                row.hover(|row| row.bg(colors.chrome_hover))
            })
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                if this.selected.as_ref() != Some(&hovered) {
                    this.selected = Some(hovered.clone());
                    cx.notify();
                }
            }))
            .on_click(
                cx.listener(move |this, _, window, cx| this.choose(chosen.clone(), window, cx)),
            )
            .child(agent_mark(
                &entry.adapter_id,
                state,
                glyphs,
                colors.chrome_dim,
                if selected {
                    colors.accent_wash
                } else {
                    colors.chrome_bg
                },
                cx,
            ))
            .child(div().flex_none().max_w_1_2().truncate().child(
                crate::commands::quick_open::highlighted(&title, query, colors.accent),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(colors.chrome_dim)
                    .text_size(ui(TYPE_SECONDARY))
                    .child(meta(entry, now)),
            )
            .when(snoozed, |row| {
                row.child(
                    Button::new(SharedString::from(format!("history-wake-{index}")))
                        .label("Wake now")
                        .xsmall()
                        .ghost()
                        .tab_stop(false)
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            history::wake(&profile, &woken, cx);
                        }),
                )
            })
            .test_support()
            .into_any_element()
    }
}

/// Opens History on the host on screen. The palette's, the shortcut's and
/// the Agents header's way in.
pub fn open(window: &mut Window, cx: &mut App) {
    let Some(profile_id) = cx
        .try_global::<Agents>()
        .map(|agents| agents.controller.read(cx).active_profile_id())
    else {
        return;
    };
    let overlay = cx.new(|cx| HistoryOverlay::new(profile_id, window, cx));
    let focus = overlay.read(cx).input.focus_handle(cx);
    let view = overlay.clone();
    open_overlay(
        overlay,
        WIDTH,
        focus,
        true,
        move |window, cx| view.update(cx, |this, cx| this.choose_active(window, cx)),
        window,
        cx,
    );
}

impl Render for HistoryOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        let now = Local::now();
        let query = self.input.read(cx).value().trim().to_owned();
        let (loaded, entries) = self.entries(cx);
        let (snoozed, sessions) = grouped(&entries, &query);
        let ordered: Vec<HistoryEntry> = snoozed
            .iter()
            .chain(&sessions)
            .map(|e| (*e).clone())
            .collect();
        let active = self.active_index(&ordered);
        let max_height = ui(LIST_MAX_HEIGHT)
            .to_pixels(window.rem_size())
            .min(window.viewport_size().height * VIEWPORT_LIMIT);
        let label = |id: &'static str, text: &'static str| {
            div()
                .id(id)
                .role(Role::Heading)
                .aria_label(text)
                .px(ui(9.))
                .pt(ui(8.))
                .pb(ui(2.))
                .text_size(ui(TYPE_LABEL))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors.chrome_faint)
                .child(text.to_uppercase())
                .into_any_element()
        };
        let mut body: Vec<AnyElement> = Vec::new();
        let snoozed_count = snoozed.len();
        for (index, entry) in ordered.iter().enumerate() {
            if index == 0 && snoozed_count > 0 {
                body.push(label("history-snoozed", "Snoozed"));
            }
            if index == snoozed_count && !sessions.is_empty() {
                body.push(label("history-sessions", "Sessions"));
            }
            body.push(self.render_row(index, entry, Some(index) == active, &query, &now, cx));
        }
        let empty: SharedString = match self.host(cx) {
            None => "This host is not connected.".into(),
            Some(HostHistory {
                failed: Some(error),
                loaded: false,
                ..
            }) => format!("History could not be listed: {error}").into(),
            Some(_) if !loaded => "Loading sessions…".into(),
            Some(_) if entries.is_empty() => "No sessions recorded on this host yet.".into(),
            Some(_) => "No matching sessions.".into(),
        };
        let hint = |text: &'static str| div().child(text);
        v_flex()
            .id("history")
            .role(Role::Group)
            .aria_label("History")
            .capture_action(escape_ends_composition)
            .capture_action(cx.listener(|this, _: &MoveDown, window, cx| this.step(1, window, cx)))
            .capture_action(cx.listener(|this, _: &MoveUp, window, cx| this.step(-1, window, cx)))
            .child(
                div().p(ui(9.)).child(
                    h_flex()
                        .h(ui(36.))
                        .px(ui(11.))
                        .gap(ui(9.))
                        .border_1()
                        .border_color(colors.accent)
                        .rounded(ui(RADIUS_CONTROL))
                        .bg(colors.chrome_bg)
                        .child(
                            Icon::new(Glyph::Search)
                                .xsmall()
                                .text_color(colors.chrome_dim),
                        )
                        .child(
                            div().flex_1().min_w_0().child(
                                Input::new(&self.input)
                                    .id("history-input")
                                    .role(Role::EditableComboBox)
                                    .aria_label("Search sessions by title, workspace or provider")
                                    .appearance(false)
                                    .xsmall()
                                    .p_0()
                                    .text_size(ui(TYPE_MENU)),
                            ),
                        ),
                ),
            )
            .child(if body.is_empty() {
                div()
                    .id("history-empty")
                    .aria_label(empty.clone())
                    .test_support()
                    .px(ui(14.))
                    .py(ui(12.))
                    .text_color(colors.chrome_dim)
                    .child(empty)
                    .into_any_element()
            } else {
                v_flex()
                    .id("history-results")
                    .role(Role::ListBox)
                    .aria_label("Sessions")
                    .max_h(max_height)
                    .px(ui(5.))
                    .pb(ui(5.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(body)
                    .test_support()
                    .into_any_element()
            })
            .child(
                h_flex()
                    .gap(ui(14.))
                    .px(ui(14.))
                    .py(ui(8.))
                    .border_t_1()
                    .border_color(colors.chrome_hairline)
                    .bg(colors.chrome_bg)
                    .rounded_b(ui(RADIUS_PANEL))
                    .text_size(ui(TYPE_META))
                    .text_color(colors.chrome_dim)
                    .child(hint("↵ Open or resume"))
                    .child(hint("Esc Close")),
            )
    }
}

/// The woke placeholders, for the top of the Agents list: one per session
/// whose snooze ended on a shown host. Clicking one resumes it.
pub fn woke_rows(cx: &App) -> Vec<AnyElement> {
    let (Some(history), Some(agents)) = (cx.try_global::<History>(), cx.try_global::<Agents>())
    else {
        return Vec::new();
    };
    let shown = agents.runtime.read(cx).inputs.shown.clone();
    let colors = theme::palette(cx);
    let glyphs = crate::settings::get(cx).agent_state_glyphs;
    history
        .0
        .read(cx)
        .woke(&shown)
        .into_iter()
        .map(|(profile, entry)| {
            let title = entry_title(entry);
            let key = format!("{profile}-{}-{}", entry.adapter_id, entry.native_session_id);
            let (profile, target) = (profile.to_owned(), entry.clone());
            h_flex()
                .id(SharedString::from(format!("agent-woke-{key}")))
                .test_support()
                .role(Role::Button)
                .aria_label(SharedString::from(format!(
                    "{title}, woke, click to resume"
                )))
                .mx(ui(5.))
                .mb(ui(4.))
                .items_center()
                .gap(ui(6.))
                .px(ui(7.))
                .py(ui(5.))
                .rounded(ui(RADIUS_CONTROL))
                .border_1()
                .border_color(colors.accent)
                .bg(colors.accent_wash)
                .text_size(ui(crate::theme::metrics::TYPE_ROW))
                .text_color(colors.chrome_ink)
                .cursor_pointer()
                .hover(|style| style.bg(colors.chrome_hover))
                .on_click(move |_, _, cx| history::resume(&profile, &target, cx))
                .child(agent_mark(
                    &entry.adapter_id,
                    DisplayState::Idle,
                    glyphs,
                    colors.chrome_dim,
                    colors.accent_wash,
                    cx,
                ))
                .child(div().flex_1().min_w_0().truncate().child(title))
                .child(
                    div()
                        .flex_none()
                        .text_size(ui(12.))
                        .text_color(colors.accent)
                        .child("Woke · click to resume"),
                )
                .into_any_element()
        })
        .collect()
}

/// Whether the agent can be snoozed now: it has a session to come back to,
/// is not mid-turn or waiting on a person, and its host is connected.
pub fn can_snooze(agent: &AgentRecord, cx: &App) -> bool {
    !agent.native_session_id.trim().is_empty()
        && !matches!(agent.lifecycle, Lifecycle::Working | Lifecycle::Blocked)
        && cx.try_global::<Agents>().is_some_and(|agents| {
            agents
                .runtime
                .read(cx)
                .scope(&agent.host_profile_id)
                .is_some_and(|scope| scope.server_identity == agent.server_identity)
        })
}

/// The Snooze submenu for an agent: the wake times Settings lists, then a
/// time typed in.
pub fn snooze_entry(agent: &AgentRecord, cx: &App) -> Entry {
    let disabled = !can_snooze(agent, cx);
    let mut entries: Vec<Entry> = snooze::menu_times(&crate::settings::get(cx).snooze_times)
        .into_iter()
        .map(|time| {
            let agent = agent.clone();
            Item::new(snooze::label(time), move |_, cx| {
                snooze_at(&agent, time, cx);
            })
            .into()
        })
        .collect();
    let agent = agent.clone();
    entries.push(Entry::Separator);
    entries.push(
        Item::new("Pick a time…", move |window, cx| {
            pick_time(&agent, window, cx)
        })
        .into(),
    );
    Entry::submenu("Snooze", entries, disabled)
}

/// The Snooze submenu for a terminal tab's agent, when the tab holds exactly
/// one: a split with two agents has no single session to close.
pub fn snooze_entry_for_window(profile_id: &str, window_id: &str, cx: &App) -> Option<Entry> {
    let agents = cx.try_global::<Agents>()?;
    let runtime = agents.runtime.read(cx);
    let mut in_window = runtime
        .agents(profile_id)
        .into_iter()
        .filter(|agent| agent.window_id == window_id);
    let agent = in_window.next()?.clone();
    in_window.next().is_none().then(|| snooze_entry(&agent, cx))
}

fn snooze_at(agent: &AgentRecord, time: WakeTime, cx: &mut App) {
    history::snooze(agent, snooze::wake_from_now(time), snooze::label(time), cx);
}

fn pick_time(agent: &AgentRecord, window: &mut Window, cx: &mut App) {
    let agent = agent.clone();
    open_text_prompt(
        TextPromptRequest {
            title: "Snooze until".into(),
            label: "A time such as 18:30, 45m, tomorrow 09:00 or fri 14:00".into(),
            initial_value: String::new(),
            submit: std::rc::Rc::new(move |text, _, cx| match snooze::parse(&text) {
                Some(time) => snooze_at(&agent, time, cx),
                None => report(
                    cx,
                    format!(
                        "\"{text}\" is not a time Snooze understands; try 18:30, 45m or tomorrow 09:00."
                    ),
                ),
            }),
        },
        window,
        cx,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, NaiveDate};

    fn entry(title: &str, workspace: &str, last_active: i64, snoozed: i64) -> HistoryEntry {
        HistoryEntry {
            adapter_id: "claude-code".into(),
            native_session_id: title.into(),
            title: title.into(),
            workspace_name: workspace.into(),
            last_active_at_unix_millis: last_active,
            snoozed_until_unix_millis: snoozed,
            ..HistoryEntry::default()
        }
    }

    fn titles(group: &[&HistoryEntry]) -> Vec<String> {
        group.iter().map(|entry| entry.title.clone()).collect()
    }

    #[test]
    fn snoozed_sessions_come_first_by_wake_time() {
        let entries = [
            entry("Wrapped link bug", "muxflow", 30, 0),
            entry("Docs link checker", "site", 20, 900),
            entry("Release notes", "muxflow", 10, 0),
            entry("Voice cleanup", "muxflow", 5, 100),
        ];
        let (snoozed, sessions) = grouped(&entries, "");
        assert_eq!(titles(&snoozed), ["Voice cleanup", "Docs link checker"]);
        assert_eq!(titles(&sessions), ["Wrapped link bug", "Release notes"]);
    }

    #[test]
    fn search_matches_title_workspace_and_provider() {
        let entries = [
            entry("Wrapped link bug", "muxflow", 30, 0),
            entry("Release notes", "site", 20, 0),
            HistoryEntry {
                adapter_id: "codex".into(),
                ..entry("Git panel hunks", "muxflow", 10, 0)
            },
        ];
        assert_eq!(titles(&grouped(&entries, "link").1), ["Wrapped link bug"]);
        assert_eq!(titles(&grouped(&entries, "site").1), ["Release notes"]);
        assert_eq!(titles(&grouped(&entries, "codex").1), ["Git panel hunks"]);
        assert!(grouped(&entries, "zzz").1.is_empty());
    }

    #[test]
    fn a_session_says_when_it_last_ran() {
        let zone = FixedOffset::east_opt(0).unwrap();
        let at = |month, day, hour| {
            zone.from_local_datetime(
                &NaiveDate::from_ymd_opt(2026, month, day)
                    .unwrap()
                    .and_hms_opt(hour, 5, 0)
                    .unwrap(),
            )
            .unwrap()
            .timestamp_millis()
        };
        let now = zone.timestamp_millis_opt(at(11, 13, 15)).unwrap();
        let when_at = |millis| when(&entry("x", "", millis, 0), &now);
        assert_eq!(when_at(at(11, 13, 9)), "09:05");
        assert_eq!(when_at(at(11, 12, 9)), "yesterday");
        assert_eq!(when_at(at(11, 9, 9)), "Mon");
        assert_eq!(when_at(at(10, 12, 9)), "12 Oct");
        let running = HistoryEntry {
            running: true,
            ..entry("x", "", 0, 0)
        };
        assert_eq!(when(&running, &now), "running");
    }
}
