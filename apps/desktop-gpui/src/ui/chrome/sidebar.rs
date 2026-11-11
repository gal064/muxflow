//! The 240px rail: workspaces on top, agents below with a divider between them,
//! and the host row at the bottom. Port of
//! `features/workspaces/WorkspaceSidebar.tsx`; the Agents section is the
//! agents feature's list ([`crate::agents::view::AgentsList`]).

use desktop_core::app_state::AgentSortMode;
use gpui_kit::{
    AnyElement, App, ClickEvent, Entity, FontWeight, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, RenderOnce, Rgba, Role, SharedString, StatefulInteractiveElement, Styled,
    WeakEntity, Window,
    assets::IconName,
    base::{Disableable, HandleEdge, TestSupportExt},
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem},
        v_flex,
    },
    div,
    prelude::FluentBuilder,
    relative,
};

use crate::{
    agents::{
        model::DisplayState,
        view::{AgentsList, agent_mark, state_indicator},
    },
    commands,
    connection_ui::{connection_reducer::ConnectionPhase, dialog::open_connection_dialog},
    context_menu::{self, Entry, Item},
    fonts,
    theme::{
        self,
        metrics::{RADIUS_ROW, TYPE_LABEL, TYPE_META, TYPE_ROW, TYPE_SECONDARY, WEIGHT_ROW, ui},
    },
    tooltips::with_shortcut,
    ui::{
        chrome::{
            Chrome,
            divider::{Divider, divider},
            layout,
        },
        title_bar::{bar_button, disabled_ink},
    },
    workspaces::{
        Target, Workspaces, focus_terminal, moved_cursor, rows::WorkspaceRow, run_command,
    },
};

#[derive(IntoElement)]
pub struct Sidebar {
    pub chrome: WeakEntity<Chrome>,
    pub width: f32,
    pub agents_ratio: f32,
    /// True while the title bar holds the tabs and has no room for New workspace.
    pub new_workspace_here: bool,
    pub pinned_only: bool,
    pub agent_sort: AgentSortMode,
    pub workspaces: Entity<Workspaces>,
}

fn section_label(id: &'static str, text: &'static str, cx: &App) -> impl IntoElement {
    let palette = theme::palette(cx);
    div()
        .id(id)
        .text_size(ui(TYPE_LABEL))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(palette.chrome_faint)
        .child(text.to_uppercase())
}

fn sort_toggle(
    id: &'static str,
    label: impl Into<SharedString>,
    text: impl Into<SharedString>,
    accent: Rgba,
) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .compact()
        .h(ui(14.))
        .accessibility_label(label)
        .child(
            div()
                .text_size(ui(TYPE_LABEL))
                .text_color(accent)
                .child(text.into()),
        )
}

/// A pushpin, drawn: the default icon set has none.
pub(super) fn pin_glyph(ink: Rgba) -> gpui_kit::Div {
    v_flex()
        .flex_none()
        .items_center()
        .w(ui(8.))
        .child(div().size(ui(6.)).rounded_full().bg(ink))
        .child(div().w(ui(1.5)).h(ui(4.)).bg(ink))
}

/// A link's phase as a color.
pub(crate) fn phase_color(phase: ConnectionPhase, cx: &App) -> Rgba {
    let palette = theme::palette(cx);
    match phase {
        ConnectionPhase::Connected => palette.ok,
        ConnectionPhase::Disconnected => palette.danger,
        _ => palette.warn,
    }
}

/// One workspace's row. `index` is its flat place in the list, which is the
/// number beside it and what ⌘1–9 selects; `moves` says whether it can move up
/// and down among its own host's rows.
fn workspace_row(
    row: WorkspaceRow,
    index: usize,
    moves: (bool, bool),
    keyboard: bool,
    workspaces: &Entity<Workspaces>,
    cx: &App,
) -> AnyElement {
    let palette = theme::palette(cx);
    let selected = row.active;
    let ink = if selected {
        palette.accent_ink
    } else {
        palette.chrome_ink
    };
    let meta = if selected {
        palette.accent_ink
    } else {
        palette.chrome_faint
    };
    let weak = workspaces.downgrade();
    let click = {
        let (weak, row) = (weak.clone(), row.clone());
        move |event: &ClickEvent, window: &mut Window, cx: &mut App| {
            let Some(this) = weak.upgrade() else {
                return;
            };
            this.update(cx, |this, _| this.list_cursor = Some(index));
            // Shift pins rather than selects: the row moving to the top is
            // often not the one you are working in.
            if event.modifiers().shift {
                this.update(cx, |this, cx| this.toggle_session_pin(&row, cx));
            } else if event.click_count() == 2 {
                if row.can_mutate {
                    let target = Target::Session(row.scope.clone(), row.session.id.clone());
                    run_command(&this, "session.rename", Some(target), window, cx);
                }
            } else {
                // The cursor follows the pointer, so a ring never sits on a
                // row other than the one just chosen.
                this.update(cx, |this, cx| this.select_row(&row, cx));
                focus_terminal(&this, window, cx);
            }
        }
    };
    let menu = {
        let (weak, row) = (weak.clone(), row.clone());
        move |menu: PopupMenu, _: &mut Window, _: &mut gpui_kit::Context<PopupMenu>| {
            let target = Target::Session(row.scope.clone(), row.session.id.clone());
            let command = |id: &'static str| {
                let (weak, target) = (weak.clone(), target.clone());
                move |window: &mut Window, cx: &mut App| {
                    if let Some(this) = weak.upgrade() {
                        run_command(&this, id, Some(target.clone()), window, cx);
                    }
                }
            };
            let pin = {
                let (weak, row) = (weak.clone(), row.clone());
                move |_: &mut Window, cx: &mut App| {
                    weak.update(cx, |this, cx| this.toggle_session_pin(&row, cx))
                        .ok();
                }
            };
            let pin_label = if row.pinned {
                "Unpin workspace"
            } else {
                "Pin workspace"
            };
            context_menu::fill(
                menu,
                &[
                    Item::new("Rename workspace…", command("session.rename"))
                        .disabled(!row.can_mutate)
                        .into(),
                    Item::new(pin_label, pin).into(),
                    Item::new("Move up", command("session.moveLeft"))
                        .disabled(!row.can_mutate || !moves.0)
                        .into(),
                    Item::new("Move down", command("session.moveRight"))
                        .disabled(!row.can_mutate || !moves.1)
                        .into(),
                    Entry::Separator,
                    Item::new("Close workspace…", command("session.close"))
                        .destructive()
                        .disabled(!row.can_mutate)
                        .into(),
                ],
            )
        }
    };
    let pin_label = if row.pinned {
        "Unpin workspace"
    } else {
        "Pin workspace"
    };
    let pin_toggle = {
        let (weak, row) = (weak.clone(), row.clone());
        Button::new(SharedString::from(format!("workspace-pin-{index}")))
            .ghost()
            .xsmall()
            .compact()
            .size(ui(20.))
            .accessibility_label(pin_label)
            .tooltip(pin_label)
            .child(pin_glyph(meta))
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                weak.update(cx, |this, cx| this.toggle_session_pin(&row, cx))
                    .ok();
            })
    };
    let agents = row.agents.clone();
    let prefs = crate::settings::get(cx);
    let (compact, glyphs) = (prefs.compact_workspaces, prefs.agent_state_glyphs);
    // The row's own colour, which a mark's badge is ringed with.
    let ring = if selected {
        palette.accent
    } else {
        palette.chrome_bg
    };
    // One workspace-level indicator, in the same place in both modes. The
    // agent lines already say blocked and done in words, so the full row shows
    // working alone; a compact row has no lines and shows the loudest state.
    // The slot stays while empty, so a state arriving cannot move the name.
    let indicator = if compact {
        let state = agents.state.filter(|state| {
            matches!(
                state,
                DisplayState::Blocked | DisplayState::Done | DisplayState::Working
            )
        });
        state_indicator(state.unwrap_or(DisplayState::Idle), glyphs, 7., cx)
    } else {
        // Working alone, and always as motion: the lines below carry the rest.
        let state = if agents.working {
            DisplayState::Working
        } else {
            DisplayState::Idle
        };
        state_indicator(state, false, 7., cx)
    };
    // Waiting agents: inline after a compact row's marks, else at the right edge.
    let unread = (agents.unread > 0).then(|| {
        let count = if agents.unread > 99 {
            "99+".to_owned()
        } else {
            agents.unread.to_string()
        };
        h_flex()
            .flex_none()
            .justify_center()
            .min_w(ui(15.))
            .h(ui(15.))
            .px(ui(4.))
            .rounded(ui(8.))
            .bg(palette.accent)
            .text_color(palette.accent_ink)
            .text_size(ui(9.))
            .font_weight(FontWeight::SEMIBOLD)
            .child(count)
    });
    let (inline_unread, edge_unread) = if compact {
        (unread, None)
    } else {
        (None, unread)
    };
    let group: SharedString = format!("workspace-row-{index}").into();
    h_flex()
        .id(("workspace-row", index))
        .group(group.clone())
        .test_support()
        .role(Role::Button)
        .aria_label(row.label())
        .aria_selected(selected)
        // The list keeps the keyboard; this tells a screen reader which row
        // the cursor is on.
        .when(keyboard, |this| this.aria_active_descendant())
        .relative()
        .min_h(ui(28.))
        .pl(ui(8.))
        .pr(ui(8.))
        .py(ui(4.))
        .mb(ui(1.))
        .gap(ui(6.))
        .items_start()
        .rounded(ui(RADIUS_ROW))
        .line_height(relative(1.25))
        .when(row.offline(), |this| this.opacity(0.55))
        .when(selected, |this| this.bg(palette.accent))
        .when(!selected, |this| {
            this.hover(|style| style.bg(palette.chrome_hover))
        })
        // Outside the row, as Tauri's focus outline: drawn inside, it would
        // vanish on the accent fill and push every row below it down.
        .when(keyboard, |this| {
            this.child(
                div()
                    .absolute()
                    .top(ui(-3.))
                    .left(ui(-3.))
                    .right(ui(-3.))
                    .bottom(ui(-3.))
                    .border_2()
                    .border_color(palette.accent)
                    .rounded(ui(RADIUS_ROW + 3.)),
            )
        })
        .on_click(click)
        .child(
            // The row's type is set here, not on the row: its context menu
            // would inherit it.
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(ui(2.))
                .text_size(ui(TYPE_ROW))
                .font_weight(FontWeight(WEIGHT_ROW))
                .text_color(ink)
                .child(
                    h_flex()
                        .min_w_0()
                        .items_center()
                        .gap(ui(6.))
                        .when(index < 9, |this| {
                            this.child(
                                div()
                                    .flex_none()
                                    .w(ui(12.))
                                    .text_center()
                                    .font_family(fonts::MONO)
                                    .text_size(ui(TYPE_META))
                                    .text_color(meta)
                                    .child((index + 1).to_string()),
                            )
                        })
                        .when(!row.letter.is_empty(), |this| {
                            this.child(
                                div()
                                    .flex_none()
                                    .w(ui(12.))
                                    .text_center()
                                    .font_family(fonts::MONO)
                                    .text_size(ui(TYPE_META))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    // A step darker than the number: an address.
                                    .text_color(if selected {
                                        palette.accent_ink
                                    } else {
                                        palette.chrome_dim
                                    })
                                    .child(row.letter.clone()),
                            )
                        })
                        .child(indicator)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(row.session.name.clone()),
                        )
                        // A compact row trades the agent lines for their marks
                        // on the name's line. Decorative: the row's name says it.
                        .when(compact && !agents.agents.is_empty(), |this| {
                            this.child(
                                h_flex()
                                    .flex_none()
                                    .items_center()
                                    .gap(ui(3.))
                                    .children(agents.agents.iter().map(|agent| {
                                        agent_mark(
                                            &agent.adapter_id,
                                            agent.state,
                                            glyphs,
                                            meta,
                                            ring,
                                            cx,
                                        )
                                    }))
                                    .when(agents.overflow > 0, |this| {
                                        this.child(
                                            div()
                                                .text_size(ui(TYPE_META))
                                                .text_color(meta)
                                                .child(format!("+{}", agents.overflow)),
                                        )
                                    }),
                            )
                        })
                        .children(inline_unread),
                )
                .when(!compact, |this| {
                    this.children(agents.agents.iter().map(|agent| {
                        h_flex()
                            .min_w_0()
                            .items_center()
                            .gap(ui(6.))
                            .pl(ui(18.))
                            .text_size(ui(TYPE_SECONDARY))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(meta)
                            .child(agent_mark(
                                &agent.adapter_id,
                                agent.state,
                                glyphs,
                                meta,
                                ring,
                                cx,
                            ))
                            .child(div().min_w_0().truncate().child(agent.name.clone()))
                    }))
                })
                .when(!compact && agents.overflow > 0, |this| {
                    this.child(
                        div()
                            .pl(ui(31.))
                            .text_size(ui(TYPE_SECONDARY))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(meta)
                            .child(format!("…{} more", agents.overflow)),
                    )
                }),
        )
        .children(edge_unread.map(|badge| {
            // The pin takes the count's place on hover.
            badge.group_hover(group.clone(), |style| style.invisible())
        }))
        // Over the row's right edge, on a backing of the row's own colour, so
        // showing it moves nothing and takes no width from the name.
        .child(
            div()
                .absolute()
                .top(ui(4.))
                .right(ui(4.))
                .p(ui(1.))
                .bg(if selected {
                    palette.accent
                } else {
                    palette.chrome_hover
                })
                .invisible()
                .group_hover(group, |style| style.visible())
                .child(pin_toggle),
        )
        .context_menu(menu)
        .into_any_element()
}

/// A block's label and a hairline filling the rest of the line. The first
/// sits right under the section header, which already gives the space above.
fn list_divider(id: &'static str, label: &'static str, first: bool, cx: &App) -> AnyElement {
    let palette = theme::palette(cx);
    h_flex()
        .id(id)
        .items_center()
        .gap(ui(6.))
        .px(ui(8.))
        .pt(ui(if first { 0. } else { 6. }))
        .pb(ui(4.))
        .text_size(ui(TYPE_LABEL))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(palette.chrome_faint)
        .child(label.to_uppercase())
        .child(div().flex_1().h(ui(1.)).bg(palette.chrome_hairline))
        .into_any_element()
}

fn host_row(workspaces: &Entity<Workspaces>, cx: &App) -> AnyElement {
    let palette = theme::palette(cx);
    let state = workspaces.read(cx);
    let hosts = state.hosts(cx);
    let Some(active) = hosts.iter().find(|host| host.active).cloned() else {
        return div()
            .id("host-row")
            .flex_none()
            .h(ui(32.))
            .border_t_1()
            .border_color(palette.chrome_hairline)
            .into_any_element();
    };
    let latency = state.latency().map(f64::round);
    let phase = active.phase.wire_name();
    let label = format!(
        "Host {} over {}, {phase}. Open the host menu.",
        active.label, active.transport
    );
    let tooltip: SharedString = match latency {
        Some(ms) => format!(
            "{} · {} · {phase} · last measured round-trip {ms} ms",
            active.label, active.transport
        ),
        None => format!("{} · {} · {phase}", active.label, active.transport),
    }
    .into();
    let weak = workspaces.downgrade();
    let controller = state.controller().cloned();
    let button = Button::new("host-row-button")
        .ghost()
        .small()
        .flex_1()
        .min_w_0()
        .justify_start()
        .accessibility_label(label)
        .tooltip(tooltip)
        .child(
            h_flex()
                .min_w_0()
                .items_center()
                .gap(ui(7.))
                .font_family(fonts::MONO)
                .text_size(ui(TYPE_META))
                .text_color(palette.chrome_dim)
                .child(
                    div()
                        .flex_none()
                        .size(ui(7.))
                        .rounded_full()
                        .bg(phase_color(active.phase, cx)),
                )
                .child(div().min_w_0().truncate().child(active.label.clone()))
                .child(
                    div()
                        .flex_none()
                        .text_color(palette.chrome_faint)
                        .child(active.transport),
                ),
        )
        .dropdown_menu(move |mut menu, _, _| {
            for host in &hosts {
                let label = if host.shown && host.phase != ConnectionPhase::Connected {
                    format!(
                        "{} {} · {}",
                        host.letter,
                        host.label,
                        host.phase.wire_name()
                    )
                } else {
                    format!("{} {}", host.letter, host.label)
                };
                let (weak, id) = (weak.clone(), host.profile_id.clone());
                menu = menu.item(
                    PopupMenuItem::new(label)
                        .checked(host.shown)
                        // The active host is always shown, so its item cannot
                        // be unchecked.
                        .disabled(host.active)
                        .on_click(move |_, _, cx| {
                            weak.update(cx, |this, cx| this.toggle_host_shown(&id, cx))
                                .ok();
                        }),
                );
            }
            let controller = controller.clone();
            menu.separator()
                .item(
                    PopupMenuItem::new("Connection settings…").on_click(move |_, window, cx| {
                        if let Some(controller) = controller.clone() {
                            open_connection_dialog(controller, window, cx);
                        }
                    }),
                )
        });
    div()
        .id("host-row")
        .flex_none()
        .h(ui(32.))
        .flex()
        .items_center()
        .border_t_1()
        .border_color(palette.chrome_hairline)
        .child(button)
        // Beside the button, not in it: its content does not stretch, and the
        // reading belongs at the row's right edge.
        .when_some(latency, |this, ms| {
            this.child(
                div()
                    .flex_none()
                    .ml_auto()
                    .pr(ui(8.))
                    .font_family(fonts::MONO)
                    .text_size(ui(TYPE_META))
                    .text_color(palette.chrome_faint)
                    .child(format!("{ms} ms")),
            )
        })
        .into_any_element()
}

impl RenderOnce for Sidebar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = theme::palette(cx);
        let width = self.width;
        let state = self.workspaces.read(cx);
        let rows = state.rows(true, cx);
        let hosts = state.hosts(cx);
        let can_create = hosts.iter().any(|host| host.shown && host.can_mutate);
        let shown_hosts = hosts.iter().filter(|host| host.shown).count();
        let list_focus = state.list_focus.clone();
        let keyboard = list_focus
            .is_focused(window)
            .then(|| state.list_position(cx))
            .flatten();
        let section_head = |left: AnyElement, right: AnyElement| {
            h_flex()
                .flex_none()
                .items_center()
                .gap(ui(8.))
                .px(ui(8.))
                .pt(ui(4.))
                .pb(ui(6.))
                .child(left)
                .child(div().flex_1())
                .child(right)
        };

        let pinned_only = self.pinned_only;
        let toggle_chrome = self.chrome.clone();
        let filter = sort_toggle(
            "workspaces-filter",
            if pinned_only {
                "Showing pinned workspaces only. Show all workspaces."
            } else {
                "Showing all workspaces. Show pinned workspaces only."
            },
            if pinned_only { "pinned only" } else { "all" },
            palette.accent,
        )
        .on_click(move |_, _, cx| {
            toggle_chrome
                .update(cx, |chrome, cx| chrome.set_pinned_only(!pinned_only, cx))
                .ok();
        });

        let new_workspace = self.new_workspace_here.then(|| {
            bar_button(
                "sidebar-new-workspace",
                IconName::Plus,
                "New workspace",
                if can_create {
                    palette.chrome_dim
                } else {
                    disabled_ink(palette.chrome_faint)
                },
            )
            .my(ui(-4.))
            .tooltip(with_shortcut("New workspace", "session.new", cx))
            .disabled(!can_create)
            .on_click(|_, window, cx| commands::run("session.new", window, cx))
        });

        let sort = self.agent_sort;
        let sort_chrome = self.chrome.clone();
        let ordering = sort_toggle(
            "agents-sort",
            format!(
                "Agent ordering: {}. Next: {}.",
                layout::sort_mode_label(sort),
                layout::sort_mode_label(layout::next_sort_mode(sort))
            ),
            layout::sort_mode_label(sort),
            palette.accent,
        )
        .on_click(move |_, _, cx| {
            sort_chrome
                .update(cx, |chrome, cx| chrome.next_agent_sort(cx))
                .ok();
        });

        let quiet = |text: &'static str| {
            div()
                .px(ui(12.))
                .py(ui(10.))
                .text_size(ui(TYPE_SECONDARY))
                .text_color(palette.chrome_faint)
                .child(text)
        };

        // Up and down among its own host's rows, as the menu's Move items.
        // In tmux's order, not the screen's: pinned rows are lifted to the top
        // on screen, but a move swaps places in tmux.
        let moves = |row: &WorkspaceRow| {
            let mut host: Vec<&WorkspaceRow> = rows
                .iter()
                .filter(|other| other.host_profile_id == row.host_profile_id)
                .collect();
            host.sort_by_key(|other| other.session.order);
            let position = host.iter().position(|other| other.key == row.key);
            (
                position.is_some_and(|index| index > 0),
                position.is_some_and(|index| index + 1 < host.len()),
            )
        };
        // A pin draws a divider, not a mark: one label for the block says what
        // a glyph repeated down every pinned row said. Each row keeps its flat
        // number, which is its address.
        let any_pinned = rows.iter().any(|row| row.pinned);
        let mut items: Vec<AnyElement> = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            if any_pinned && index == 0 {
                items.push(list_divider(
                    "sidebar-workspaces-pinned",
                    "Pinned",
                    true,
                    cx,
                ));
            }
            let after_pinned = index == 0 || rows[index - 1].pinned;
            if any_pinned && !row.pinned && after_pinned {
                items.push(list_divider(
                    "sidebar-workspaces-others",
                    "Others",
                    false,
                    cx,
                ));
            }
            items.push(workspace_row(
                row.clone(),
                index,
                moves(row),
                keyboard == Some(index),
                &self.workspaces,
                cx,
            ));
        }
        let count = rows.len();
        let keys = self.workspaces.downgrade();
        let workspaces_list = div()
            .id("sidebar-workspaces-list")
            .role(Role::List)
            .aria_label("Workspaces")
            .track_focus(&list_focus)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                let Some(this) = keys.upgrade() else {
                    return;
                };
                // A modified arrow belongs to a binding, not to the cursor.
                if event.keystroke.modifiers.modified() {
                    return;
                }
                let key = event.keystroke.key.as_str();
                let cursor = this.read(cx).list_position(cx);
                if let Some(next) = moved_cursor(cursor, count, key, true) {
                    this.update(cx, |this, cx| {
                        this.list_cursor = Some(next);
                        cx.notify();
                    });
                    cx.stop_propagation();
                } else if matches!(key, "enter" | "space") {
                    let row = cursor
                        .and_then(|index| this.read(cx).rows(true, cx).into_iter().nth(index));
                    if let Some(row) = row {
                        this.update(cx, |this, cx| this.select_row(&row, cx));
                        focus_terminal(&this, window, cx);
                    }
                    cx.stop_propagation();
                }
            })
            .when(rows.is_empty(), |this| {
                this.child(quiet(if shown_hosts > 1 {
                    "No tmux sessions on the shown hosts yet."
                } else {
                    "No tmux sessions on this host yet."
                }))
            })
            // Said whenever the filter is on with nothing pinned: the selected
            // workspace sitting there alone is the ordinary way to meet it.
            .when(!rows.is_empty() && pinned_only && !any_pinned, |this| {
                this.child(quiet(
                    "No pinned workspaces — Shift-click a workspace to pin it.",
                ))
            })
            .children(items);

        let agents_list = AgentsList {
            sort: self.agent_sort,
            pinned_only: self.pinned_only,
        };

        let host = host_row(&self.workspaces, cx);

        v_flex()
            .id("sidebar")
            .test_support()
            .role(Role::Navigation)
            .aria_label("Workspaces and agents")
            // CSS `line-height: normal`, under GPUI's roomier default.
            .line_height(relative(1.2))
            .relative()
            .flex_none()
            .h_full()
            .w(ui(width))
            .min_h_0()
            .px(ui(6.))
            .pt(ui(6.))
            .bg(palette.chrome_bg)
            .border_r_1()
            .border_color(palette.chrome_hairline)
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(section_head(
                        section_label("sidebar-workspaces-label", "Workspaces", cx)
                            .into_any_element(),
                        h_flex()
                            .gap(ui(4.))
                            .child(filter)
                            .children(new_workspace)
                            .into_any_element(),
                    ))
                    .child(workspaces_list),
            )
            .child(
                div()
                    .relative()
                    .flex_none()
                    .h(ui(9.))
                    .my(ui(2.))
                    .border_t_1()
                    .border_color(palette.chrome_hairline)
                    .child(divider(
                        self.chrome.clone(),
                        Divider::Agents,
                        HandleEdge::Leading,
                        false,
                        self.agents_ratio,
                        window,
                        cx,
                    ))
                    .child(
                        div()
                            .absolute()
                            .top(ui(2.))
                            .left(relative(0.5))
                            .ml(ui(-14.))
                            .w(ui(28.))
                            .h(ui(3.))
                            .rounded(ui(2.))
                            .bg(palette.chrome_border),
                    ),
            )
            .child(
                v_flex()
                    .flex_none()
                    .min_h_0()
                    .h(relative(self.agents_ratio))
                    .child(section_head(
                        section_label("sidebar-agents-label", "Agents", cx).into_any_element(),
                        ordering.into_any_element(),
                    ))
                    .child(agents_list),
            )
            .child(host)
            .child(divider(
                self.chrome,
                Divider::Sidebar,
                HandleEdge::Trailing,
                false,
                width,
                window,
                cx,
            ))
    }
}
