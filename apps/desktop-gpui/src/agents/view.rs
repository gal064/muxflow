//! The agents section's rows, marks and menus, and the setup and hook-review
//! dialogs. Port of the agent half of `WorkspaceSidebar.tsx`, `AgentIdentity.tsx`,
//! `ui/AgentStateIndicator.tsx`, `ui/StateDot.tsx`, `AgentHostSetupDialog.tsx`
//! and `HookReviewDialog.tsx`, with their copy.

use std::{rc::Rc, time::Duration};

use desktop_core::app_state::AgentSortMode;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, EmptyView, Entity, FocusHandle, FontWeight,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, Render, RenderOnce,
    Rgba, Role, ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Subscription, Task,
    Window,
    base::{RootPlugin, TestSupportExt},
    component::{
        Disableable, Sizable, WindowExt,
        button::{Button, ButtonVariants},
        dialog::DialogFooter,
        h_flex,
        menu::{ContextMenuExt, PopupMenu},
        spinner::Spinner,
        tooltip::Tooltip,
        v_flex,
    },
    div,
    prelude::FluentBuilder,
    relative, svg,
};

use crate::{
    commands::{RowCommandSource, registry::CommandId},
    context_menu::{Entry, Item, fill},
    dragdrop::{self, DragPayload},
    fonts,
    theme::{
        self, Palette,
        metrics::{
            NARROW_DIALOGS, RADIUS_CONTROL, RADIUS_ROW, TYPE_LABEL, TYPE_META, TYPE_ROW,
            TYPE_SECONDARY, narrower_than, ui,
        },
    },
    ui::{
        chrome::host_letter,
        dialog::{self, Action, Prose},
        icons::Glyph,
        motion,
        surface_error::SurfaceError,
    },
    workspaces,
};

use super::{
    Agents, BRANCH_PANE_WAIT_MS, PendingOpen,
    labels::agent_session_label,
    list::{
        AgentListRow, group_by_pin, group_by_status, group_by_workspace, needs_attention,
        selected_agent_id_for_pane,
    },
    model::{AdapterDescriptor, AgentRecord, DisplayState, Lifecycle, Placement},
    open_row, open_when_present, report,
    runtime::AgentRuntime,
    setup::{Activity, OpenReview, SetupQuestion},
    wire::{HookAction, LaunchRequest},
    wiring::{hook_wiring_notice, host_hook_wiring},
};
use crate::workspaces::RowKeys;

// Adapter marks, as `AgentIcon` draws them. Rendered as masks: the ink is the
// text color, so only the shapes and their stroke widths are here.
const CLAUDE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-linecap="round" stroke-width="2"><path d="M8 1.5v13M1.5 8h13M3.4 3.4l9.2 9.2M12.6 3.4l-9.2 9.2"/></svg>"##;
const CODEX: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="146.694 227.042 267.198 264.812"><g transform="translate(280.293 359.448) scale(.86) translate(-280.293 -359.448)"><path d="M249.176 323.434V298.276C249.176 296.158 249.971 294.569 251.825 293.509L302.406 264.381C309.29 260.409 317.5 258.555 325.973 258.555C357.75 258.555 377.877 283.185 377.877 309.399C377.877 311.253 377.877 313.371 377.611 315.49L325.178 284.771C322.001 282.919 318.822 282.919 315.645 284.771L249.176 323.434ZM367.283 421.415V361.301C367.283 357.592 365.694 354.945 362.516 353.092L296.048 314.43L317.763 301.982C319.617 300.925 321.206 300.925 323.058 301.982L373.639 331.112C388.205 339.586 398.003 357.592 398.003 375.069C398.003 395.195 386.087 413.733 367.283 421.412V421.415ZM233.553 368.452L211.838 355.742C209.986 354.684 209.19 353.095 209.19 350.975V292.718C209.19 264.383 230.905 242.932 260.301 242.932C271.423 242.932 281.748 246.641 290.49 253.26L238.321 283.449C235.146 285.303 233.555 287.951 233.555 291.659V368.455L233.553 368.452ZM280.292 395.462L249.176 377.985V340.913L280.292 323.436L311.407 340.913V377.985L280.292 395.462ZM300.286 475.968C289.163 475.968 278.837 472.259 270.097 465.64L322.264 435.449C325.441 433.597 327.03 430.949 327.03 427.239V350.445L349.011 363.155C350.865 364.213 351.66 365.802 351.66 367.922V426.179C351.66 454.514 329.679 475.965 300.286 475.965V475.968ZM237.525 416.915L186.944 387.785C172.378 379.31 162.582 361.305 162.582 343.827C162.582 323.436 174.763 305.164 193.563 297.485V357.861C193.563 361.571 195.154 364.217 198.33 366.071L264.535 404.467L242.82 416.915C240.967 417.972 239.377 417.972 237.525 416.915ZM234.614 460.343C204.689 460.343 182.71 437.833 182.71 410.028C182.71 407.91 182.976 405.792 183.238 403.672L235.405 433.863C238.582 435.715 241.763 435.715 244.938 433.863L311.407 395.466V420.622C311.407 422.742 310.612 424.331 308.758 425.389L258.179 454.519C251.293 458.491 243.083 460.343 234.611 460.343H234.614ZM300.286 491.854C332.329 491.854 359.073 469.082 365.167 438.892C394.825 431.211 413.892 403.406 413.892 375.073C413.892 356.535 405.948 338.529 391.648 325.552C392.972 319.991 393.766 314.43 393.766 308.87C393.766 271.003 363.048 242.666 327.562 242.666C320.413 242.666 313.528 243.723 306.644 246.109C294.725 234.457 278.307 227.042 260.301 227.042C228.258 227.042 201.513 249.815 195.42 280.004C165.761 287.685 146.694 315.49 146.694 343.824C146.694 362.362 154.638 380.368 168.938 393.344C167.613 398.906 166.819 404.467 166.819 410.027C166.819 447.894 197.538 476.231 233.024 476.231C240.172 476.231 247.058 475.173 253.943 472.788C265.859 484.441 282.278 491.854 300.286 491.854Z"/></g></svg>"##;
const PI: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 560"><g transform="translate(280 280) scale(.76) translate(-280 -280)"><path d="M420 280H280V140H0V0H420V280ZM560 560H420V280H560V560ZM140 560H0V140H140V280H280V420H140V560Z"/></g></svg>"##;
const GENERIC: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-linecap="round" stroke-width="1.35"><circle cx="8" cy="8" r="5.5"/><circle cx="6" cy="7" r=".8" fill="#000" stroke="none"/><circle cx="10" cy="7" r=".8" fill="#000" stroke="none"/><path d="M5.5 10h5"/></svg>"##;
const BRANCH: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-linecap="round" stroke-linejoin="round" stroke-width="1.35"><circle cx="4.75" cy="3.75" r="1.75"/><circle cx="4.75" cy="12.25" r="1.75"/><circle cx="11.25" cy="3.75" r="1.75"/><path d="M4.75 5.5v5M11.25 5.5v1.25a2.5 2.5 0 0 1-2.5 2.5H6.5"/></svg>"##;

pub fn adapter_icon(adapter_id: &str) -> &'static [u8] {
    match adapter_id {
        "claude-code" => CLAUDE,
        "codex" => CODEX,
        "pi" => PI,
        _ => GENERIC,
    }
}

/// The state glyph the accessibility option draws inside a dot.
pub fn state_glyph(state: DisplayState) -> &'static str {
    match state {
        DisplayState::Blocked => "!",
        DisplayState::Done => "✓",
        DisplayState::Working => "•",
        DisplayState::Unknown => "?",
        DisplayState::Idle => "",
    }
}

fn state_color(state: DisplayState, palette: &Palette) -> Rgba {
    match state {
        DisplayState::Blocked => palette.state_blocked,
        DisplayState::Working => palette.state_working,
        // A done mark is a notification and takes the bright green.
        DisplayState::Done => palette.ok,
        DisplayState::Unknown => palette.state_unknown,
        DisplayState::Idle => palette.chrome_faint,
    }
}

/// One agent state, drawn the same way everywhere: a ring means running (held
/// still, see [`motion::WorkingIcon`]), a dot means a state that is not going
/// anywhere. With glyphs on, every state is a dot whose shape carries it. Idle
/// keeps its slot and draws nothing.
pub fn state_indicator(state: DisplayState, glyphs: bool, size: f32, cx: &App) -> AnyElement {
    let palette = theme::palette(cx);
    if state == DisplayState::Working && !glyphs {
        return motion::WorkingIcon::new(size, palette.chrome_ink).into_any_element();
    }
    let dot = div().flex_none().size(ui(size)).rounded_full();
    if state == DisplayState::Idle {
        return dot.into_any_element();
    }
    let color = state_color(state, &palette);
    let dot = if state == DisplayState::Unknown {
        dot.border_1().border_dashed().border_color(color)
    } else {
        dot.bg(color)
    };
    dot.when(glyphs, |dot| {
        dot.flex()
            .items_center()
            .justify_center()
            .text_size(ui(size * 0.8))
            .line_height(relative(1.))
            .font_weight(FontWeight::BOLD)
            .text_color(palette.chrome_bg)
            .child(state_glyph(state))
    })
    .into_any_element()
}

/// An adapter's icon alone: a terminal tab's glyph for the agent in it.
pub fn adapter_glyph(adapter_id: &str, ink: Rgba) -> gpui_kit::Svg {
    svg()
        .data(adapter_icon(adapter_id))
        .flex_none()
        .size(ui(13.))
        .text_color(ink)
}

/// The adapter mark with its state docked to it, or beside it with glyphs on.
pub fn agent_mark(
    adapter_id: &str,
    state: DisplayState,
    glyphs: bool,
    ink: Rgba,
    ring: Rgba,
    cx: &App,
) -> AnyElement {
    let palette = theme::palette(cx);
    let icon = adapter_glyph(adapter_id, ink);
    if glyphs {
        return h_flex()
            .flex_none()
            .items_center()
            .gap(ui(3.))
            .child(state_indicator(state, true, 9., cx))
            .child(icon)
            .into_any_element();
    }
    let badge = (state != DisplayState::Idle).then(|| {
        let size = if state == DisplayState::Working {
            7.
        } else {
            6.
        };
        let badge = div()
            .absolute()
            .right(ui(-2.))
            .bottom(ui(-2.))
            .size(ui(size))
            .rounded_full()
            .border_1()
            .border_color(ring);
        match state {
            DisplayState::Working => badge
                .bg(ring)
                .child(motion::WorkingIcon::new(5., palette.chrome_ink)),
            DisplayState::Unknown => badge
                .bg(ring)
                .border_dashed()
                .border_color(palette.state_unknown),
            other => badge.bg(state_color(other, &palette)),
        }
    });
    div()
        .relative()
        .flex_none()
        .child(icon)
        .children(badge)
        .into_any_element()
}

/// What the row's host allows, as the Tauri sidebar's `agentHost` said it.
struct RowHost {
    active: bool,
    can_mutate: bool,
    label: String,
}

fn row_host(row: &AgentListRow, cx: &App) -> RowHost {
    let Some(agents) = cx.try_global::<Agents>() else {
        return RowHost {
            active: false,
            can_mutate: false,
            label: row.location.host_label.clone(),
        };
    };
    let controller = agents.controller.read(cx);
    RowHost {
        active: controller.active_profile_id() == row.agent.host_profile_id,
        can_mutate: controller
            .link(&row.agent.host_profile_id)
            .is_some_and(|link| link.host_state.can_mutate),
        label: row.location.host_label.clone(),
    }
}

/// A resume opens a pane beside the one on screen, under the active root:
/// both facts of the host on screen, so only its agents offer it.
fn can_resume(row: &AgentListRow, cx: &App) -> bool {
    let host = row_host(row, cx);
    host.can_mutate && host.active
}

/// A branch needs a live pane and a conversation to copy. The CLIs refuse an
/// empty one, so a finished turn, or a turn in progress, is what proves it.
fn can_branch(row: &AgentListRow, cx: &App) -> bool {
    let agent = &row.agent;
    can_resume(row, cx)
        && row.routable
        && !agent.native_session_id.trim().is_empty()
        && (agent.attention_generation != 0
            || matches!(agent.lifecycle, Lifecycle::Working | Lifecycle::Blocked))
}

fn adapters_for(agent: &AgentRecord, cx: &App) -> Vec<AdapterDescriptor> {
    cx.try_global::<Agents>()
        .map(|agents| {
            agents
                .runtime
                .read(cx)
                .adapters(&agent.host_profile_id)
                .to_vec()
        })
        .unwrap_or_default()
}

fn resume_placements(agent: &AgentRecord, cx: &App) -> Vec<Placement> {
    adapters_for(agent, cx)
        .iter()
        .find(|adapter| adapter.id == agent.adapter_id && adapter.supports_resume)
        .map(|adapter| adapter.placements().to_vec())
        .unwrap_or_default()
}

/// The shell's pin toggle, aimed at the agent's window on its own host.
/// Nothing happens once that host's tmux is another server.
fn toggle_tab_pin(row: &AgentListRow, cx: &mut App) {
    let agent = &row.agent;
    let current = cx.try_global::<Agents>().is_some_and(|agents| {
        agents
            .controller
            .read(cx)
            .link(&agent.host_profile_id)
            .is_some_and(|link| {
                link.host_state.server_identity.as_deref() == Some(&agent.server_identity)
            })
    });
    if !row.routable || !current {
        return;
    }
    if let Some(workspaces) = crate::workspaces::current(cx) {
        workspaces.update(cx, |workspaces, cx| {
            workspaces.toggle_window_pin_on(&agent.host_profile_id, &agent.window_id, cx)
        });
    }
}

/// The shell's Rename terminal tab, aimed at the agent's window on its own
/// host. Nothing happens once that host's tmux is another server.
fn rename_tab(row: &AgentListRow, window: &mut Window, cx: &mut App) {
    let agent = &row.agent;
    let scope = cx.try_global::<Agents>().and_then(|agents| {
        agents
            .controller
            .read(cx)
            .link(&agent.host_profile_id)
            .map(|link| link.scope())
            .filter(|scope| scope.server_identity.as_deref() == Some(&agent.server_identity))
    });
    let (Some(scope), Some(workspaces)) = (scope, crate::workspaces::current(cx)) else {
        return;
    };
    let target = crate::workspaces::Target::Window(scope, agent.window_id.clone());
    crate::workspaces::run_command(&workspaces, "window.rename", Some(target), window, cx);
}

/// The root a launch or resume opens under: the Explorer's, once it exists.
/// Until then there is none, and a launch says what it needs.
pub struct ActiveRoot {
    pub path: String,
    pub token: String,
    /// Whether the root is inside a Git worktree, which the Git panel follows.
    pub git_worktree: bool,
}

impl gpui_kit::Global for ActiveRoot {}

pub(crate) fn launch_request(
    adapter_id: &str,
    placement: Placement,
    cx: &App,
) -> Option<LaunchRequest> {
    let agents = cx.try_global::<Agents>()?;
    let root = cx.try_global::<ActiveRoot>()?;
    let focus = &agents.runtime.read(cx).inputs.focus;
    if focus.pane_id.is_empty() || focus.session_id.is_empty() || focus.window_id.is_empty() {
        return None;
    }
    Some(LaunchRequest {
        adapter_id: adapter_id.to_owned(),
        placement,
        session_id: focus.session_id.clone(),
        window_id: focus.window_id.clone(),
        pane_id: focus.pane_id.clone(),
        active_root: root.path.clone(),
        root_token: root.token.clone(),
    })
}

fn with_runtime(cx: &mut App, f: impl FnOnce(&mut AgentRuntime, &mut Context<AgentRuntime>)) {
    if let Some(runtime) = cx
        .try_global::<Agents>()
        .map(|agents| agents.runtime.clone())
    {
        runtime.update(cx, f);
    }
}

fn launch(adapter: &AdapterDescriptor, placement: Placement, cx: &mut App) {
    let Some(request) = launch_request(&adapter.id, placement, cx) else {
        return report(
            cx,
            "Choose a live terminal pane with an authoritative active root before launching an agent.".into(),
        );
    };
    let name = adapter.display_name.clone();
    with_runtime(cx, move |runtime, cx| {
        let task = runtime.launch(request, cx);
        cx.spawn(async move |_, cx| {
            let message = match task.await {
                Ok(_) => format!("Launching {name} in a new {}…", placement.word()),
                Err(error) => error,
            };
            cx.update(|cx| report(cx, message));
        })
        .detach();
    });
}

fn resume(row: &AgentListRow, placement: Placement, cx: &mut App) {
    let host = row_host(row, cx);
    if !host.active {
        return report(
            cx,
            format!(
                "Resume opens a pane on the host on screen; select a workspace on {} first.",
                host.label
            ),
        );
    }
    let Some(request) = launch_request(&row.agent.adapter_id, placement, cx) else {
        return report(
            cx,
            "Choose a live terminal pane with an authoritative active root before resuming an agent.".into(),
        );
    };
    let agent = row.agent.clone();
    with_runtime(cx, move |runtime, cx| {
        let task = runtime.resume(&agent, request, cx);
        cx.spawn(async move |_, cx| {
            let message = match task.await {
                Ok(_) => format!(
                    "Resuming {} in a new {}…",
                    agent.display_name,
                    placement.word()
                ),
                Err(error) => error,
            };
            cx.update(|cx| report(cx, message));
        })
        .detach();
    });
}

/// Clicks on one agent's Branch closer together than this are one click: the
/// host can answer inside a double-click, so waiting would not stop the second.
const BRANCH_REPEAT_MS: u64 = 1_000;

#[derive(Default)]
struct LastBranched(std::collections::HashMap<String, u64>);

impl gpui_kit::Global for LastBranched {}

fn branch(row: &AgentListRow, cx: &mut App) {
    if !can_branch(row, cx) {
        return;
    }
    let agent = row.agent.clone();
    let Some(now) = cx
        .try_global::<Agents>()
        .map(|agents| agents.runtime.read(cx).now_ms())
    else {
        return;
    };
    let last = cx.default_global::<LastBranched>();
    if last
        .0
        .get(&agent.id)
        .is_some_and(|at| now.saturating_sub(*at) < BRANCH_REPEAT_MS)
    {
        return;
    }
    last.0.insert(agent.id.clone(), now);
    report(
        cx,
        format!("Branching {} in a new tab…", agent.display_name),
    );
    with_runtime(cx, move |runtime, cx| {
        let task = runtime.branch(&agent, cx);
        cx.spawn(async move |runtime, cx| {
            let placed = task.await;
            cx.update(|cx| match placed {
                Err(error) => report(cx, error),
                Ok((_, pane_id)) => {
                    // Someone who has moved to another host is not pulled back.
                    let here = runtime
                        .read_with(cx, |runtime, _| {
                            runtime.inputs.focus.host_profile_id == agent.host_profile_id
                        })
                        .unwrap_or(false);
                    if here {
                        open_when_present(
                            PendingOpen {
                                profile_id: agent.host_profile_id.clone(),
                                pane_id,
                                source: format!("Branch of {}", agent.display_name),
                                expected_by: now + BRANCH_PANE_WAIT_MS,
                            },
                            cx,
                        );
                    }
                }
            });
        })
        .detach();
    });
}

fn row_menu(row: &AgentListRow, cx: &App) -> Vec<Entry> {
    let host = row_host(row, cx);
    let suffix = |label: String| {
        if host.active {
            label
        } else {
            format!("{label} (open {} first)", host.label)
        }
    };
    let mut entries: Vec<Entry> = Vec::new();
    let focus = row.clone();
    entries.push(
        Item::new("Focus this agent's pane", move |_, cx| open_row(&focus, cx))
            .disabled(!row.routable)
            .into(),
    );
    let pin = row.clone();
    entries.push(
        Item::new(
            if row.location.tab_pinned {
                "Unpin tab"
            } else {
                "Pin tab"
            },
            move |_, cx| toggle_tab_pin(&pin, cx),
        )
        .disabled(!row.routable)
        .into(),
    );
    let rename = row.clone();
    entries.push(
        Item::new("Rename tab…", move |window, cx| {
            rename_tab(&rename, window, cx)
        })
        .disabled(!host.can_mutate || !row.routable)
        .into(),
    );
    let resumable = can_resume(row, cx);
    for placement in resume_placements(&row.agent, cx) {
        let target = row.clone();
        entries.push(
            Item::new(
                suffix(format!("Resume in new {}", placement.word())),
                move |_, cx| resume(&target, placement, cx),
            )
            .disabled(!resumable)
            .into(),
        );
    }
    let target = row.clone();
    entries.push(
        Item::new(
            suffix("Branch conversation in new tab".into()),
            move |_, cx| branch(&target, cx),
        )
        .disabled(!can_branch(row, cx))
        .into(),
    );
    entries.push(Entry::Separator);
    entries.extend(move_entries(row, cx));
    entries
}

/// What dragging the row carries: its pane, on the host on screen, for an
/// agent that can be resumed and routed to.
fn agent_drag_payload(row: &AgentListRow, label: &str, cx: &App) -> Option<DragPayload> {
    if !can_resume(row, cx) || !row.routable || row.agent.pane_id.is_empty() {
        return None;
    }
    workspaces::current(cx)?.read(cx).pane_payload(
        &row.agent.host_profile_id,
        &row.agent.pane_id,
        Some(label),
        cx,
    )
}

/// Bringing an agent here and sending it elsewhere. Both are moves of its pane
/// on the host on screen, so they are offered only for its agents, like a
/// resume.
fn move_entries(row: &AgentListRow, cx: &App) -> Vec<Entry> {
    let (host, pane) = (row.agent.host_profile_id.clone(), row.agent.pane_id.clone());
    let moves = (can_resume(row, cx) && row.routable)
        .then(|| {
            workspaces::current(cx).and_then(|shell| shell.read(cx).agent_moves(&host, &pane, cx))
        })
        .flatten();
    let bring = {
        let (host, pane) = (host.clone(), pane.clone());
        Item::new("Bring here as split", move |_, cx| {
            if let Some(shell) = workspaces::current(cx) {
                shell.update(cx, |shell, cx| shell.bring_agent_here(&host, &pane, cx));
            }
        })
        .disabled(
            moves
                .as_ref()
                .is_none_or(|moves| moves.bring_here.is_none()),
        )
    };
    let choices: Vec<Entry> = moves
        .iter()
        .flat_map(|moves| moves.workspaces.iter())
        .map(|(workspace, name)| {
            let (host, pane, workspace) = (host.clone(), pane.clone(), workspace.clone());
            Item::new(name.clone(), move |_, cx| {
                if let Some(shell) = workspaces::current(cx) {
                    shell.update(cx, |shell, cx| {
                        shell.move_agent_to_workspace(&host, &pane, workspace.clone(), cx)
                    });
                }
            })
            .into()
        })
        .collect();
    let none = choices.is_empty();
    vec![
        bring.into(),
        Entry::submenu("Move to workspace…", choices, none),
    ]
}

/// The section's own menu: set up, launch, and hook review, for the host on screen.
pub(crate) fn section_menu(cx: &App) -> Vec<Entry> {
    let Some(agents) = cx.try_global::<Agents>() else {
        return vec![
            Item::new("No agent adapters available", |_, _| {})
                .disabled(true)
                .into(),
        ];
    };
    let runtime = agents.runtime.read(cx);
    let host = runtime.inputs.focus.host_profile_id.clone();
    let can_mutate = agents
        .controller
        .read(cx)
        .link(&host)
        .is_some_and(|link| link.host_state.can_mutate);
    let adapters = runtime.adapters(&host).to_vec();
    let mut sections: Vec<Vec<Entry>> = Vec::new();
    // First, and only when there is something to set up: on a host that
    // cannot report status it is the item anyone wants, and the way back for
    // someone who answered "not now".
    if runtime.setup_offerable() {
        sections.push(vec![
            Item::new("Set up agent status on this host…", |_, cx| {
                with_runtime(cx, |runtime, cx| runtime.offer_setup(cx))
            })
            .disabled(!can_mutate)
            .into(),
        ]);
    }
    let launches: Vec<Entry> = adapters
        .iter()
        .filter(|adapter| adapter.supports_launch)
        .flat_map(|adapter| {
            adapter.placements().iter().map(move |placement| {
                let (adapter, placement) = (adapter.clone(), *placement);
                Item::new(
                    format!("New {} in a {}", adapter.display_name, placement.word()),
                    move |_, cx| launch(&adapter, placement, cx),
                )
                .disabled(!can_mutate)
                .into()
            })
        })
        .collect();
    if !launches.is_empty() {
        sections.push(launches);
    }
    let hooks: Vec<Entry> = adapters
        .iter()
        .filter(|adapter| adapter.supports_hooks)
        .flat_map(|adapter| {
            [HookAction::Install, HookAction::Uninstall]
                .into_iter()
                .map(move |action| {
                    let id = adapter.id.clone();
                    let verb = if action == HookAction::Install {
                        "Review"
                    } else {
                        "Remove"
                    };
                    Item::new(
                        format!("{verb} {} hooks…", adapter.display_name),
                        move |_, cx| {
                            let id = id.clone();
                            with_runtime(cx, move |runtime, cx| {
                                runtime.review_hooks_for(&id, action, cx)
                            })
                        },
                    )
                    .disabled(!can_mutate)
                    .into()
                })
        })
        .collect();
    if !hooks.is_empty() {
        sections.push(hooks);
    }
    if sections.is_empty() {
        return vec![
            Item::new("No agent adapters available", |_, _| {})
                .disabled(true)
                .into(),
        ];
    }
    let mut entries = Vec::new();
    for (index, section) in sections.into_iter().enumerate() {
        if index > 0 {
            entries.push(Entry::Separator);
        }
        entries.extend(section);
    }
    entries
}

/// The palette's commands for one agent row.
pub fn row_command_source(row: AgentListRow, cx: &App) -> RowCommandSource {
    let mut available: Vec<CommandId> = Vec::new();
    if row.routable {
        available.push("agents.focusRow");
    }
    if row_host(&row, cx).can_mutate && row.routable {
        available.push("agents.renameRow");
    }
    let resume_placement = resume_placements(&row.agent, cx).first().copied();
    if can_resume(&row, cx) && resume_placement.is_some() {
        available.push("agents.resumeRow");
    }
    if can_branch(&row, cx) {
        available.push("agents.branchRow");
    }
    RowCommandSource {
        available,
        run: Rc::new(move |id, window, cx| match id {
            "agents.focusRow" => open_row(&row, cx),
            "agents.renameRow" => rename_tab(&row, window, cx),
            // The palette is one line: the adapter's first declared placement.
            "agents.resumeRow" => {
                if let Some(placement) = resume_placement {
                    resume(&row, placement, cx);
                }
            }
            "agents.branchRow" => branch(&row, cx),
            _ => {}
        }),
    }
}

/// The accessible name: label, state, waiting, workspace, host, tab, reach.
pub fn row_label(row: &AgentListRow, session_label: &str) -> String {
    let tab = row.location.tab_index.map(|index| format!("tab {index}"));
    [
        Some(session_label.to_owned()),
        Some(row.state.word().to_owned()),
        needs_attention(row.state).then(|| "waiting".to_owned()),
        Some(row.location.workspace_name.clone()),
        Some(row.location.host_label.clone()).filter(|label| !label.is_empty()),
        tab,
        (!row.routable).then(|| "unmapped, navigation unavailable".to_owned()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(", ")
}

fn row_title(row: &AgentListRow, session_label: &str) -> String {
    format!(
        "{session_label} · {} · {} · {} · {}{}",
        row.agent.display_name,
        row.state.word(),
        row.location.workspace_name,
        row.location.host_label,
        if row.routable {
            ""
        } else {
            " · navigation unavailable"
        }
    )
}

fn row_action(
    id: SharedString,
    icon: &'static [u8],
    label: &'static str,
    disabled: bool,
    ink: Rgba,
) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .compact()
        .size(ui(18.))
        .disabled(disabled)
        // A pointer affordance: the row's menu is the keyboard's way to both.
        .tab_stop(false)
        .accessibility_label(label)
        .tooltip(label)
        .child(svg().data(icon).size(ui(12.)).text_color(ink))
}

fn agent_row(
    row: &AgentListRow,
    key: &str,
    selected: bool,
    glyphs: bool,
    keys: RowKeys,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let palette = theme::palette(cx);
    let adapters = adapters_for(&row.agent, cx);
    let session_label = agent_session_label(&row.agent, &adapters);
    let (ink, dim, background) = if selected {
        (palette.accent_ink, palette.accent_ink, palette.accent)
    } else {
        (palette.chrome_ink, palette.chrome_dim, palette.chrome_bg)
    };
    let id = SharedString::from(format!("agent-row-{key}"));
    let click = row.clone();
    let pin = row.clone();
    let branch_row = row.clone();
    let menu_row = row.clone();
    let key_row = row.clone();
    let focused = keys.focus.is_focused(window);
    let title = row_title(row, &session_label);
    let unavailable = !row.routable;
    let drag = row.clone();
    let drag_label = session_label.clone();
    let button = h_flex()
        .id(id.clone())
        .test_support()
        .role(Role::Button)
        .aria_label(row_label(row, &session_label))
        .aria_selected(selected)
        .track_focus(&keys.focus)
        .on_key_down({
            let keys = keys.clone();
            move |event: &KeyDownEvent, window, cx| {
                if crate::context_menu::is_menu_key(&event.keystroke) {
                    let entries = row_menu(&key_row, cx);
                    crate::context_menu::open(
                        move |menu, window, cx| fill(menu, &entries, window, cx),
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                    return;
                }
                if event.keystroke.modifiers.modified() {
                    return;
                }
                // Enter and Space click it.
                if keys.step(&event.keystroke.key, window, cx) {
                    cx.stop_propagation();
                }
            }
        })
        .flex_1()
        .min_w_0()
        .items_center()
        .gap(ui(6.))
        .px(ui(7.))
        .py(ui(5.))
        .rounded(ui(RADIUS_CONTROL))
        .text_size(ui(TYPE_ROW))
        .text_color(ink)
        .when(selected, |this| this.bg(palette.accent))
        .when(!selected && !unavailable, |this| {
            this.hover(|style| style.bg(palette.chrome_hover))
        })
        .when(unavailable, |this| this.opacity(0.5))
        // The row keeps the pointer while its menu is open over it, so the
        // tooltip's delay would end with it drawn over the menu's first item.
        .tooltip(move |window, cx| {
            if crate::context_menu::is_open(window, cx) {
                cx.new(|_| EmptyView).into()
            } else {
                Tooltip::new(title.clone()).build(window, cx)
            }
        })
        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
            let payload = agent_drag_payload(&drag, &drag_label, cx);
            dragdrop::press(payload, event.position, cx);
        })
        // Shift-click pins the agent's tab rather than navigating to it, as it
        // does in the tab strip and the workspace list.
        .on_click(move |event, window, cx| {
            if dragdrop::just_ended(cx) {
                return;
            }
            if event.modifiers().shift {
                toggle_tab_pin(&click, cx);
            } else if click.routable {
                open_row(&click, cx);
                // The keyboard goes with it, as the pane Tauri made active
                // took it.
                if let Some(workspaces) = crate::workspaces::current(cx) {
                    crate::workspaces::focus_terminal(&workspaces, window, cx);
                }
            }
        })
        .child(agent_mark(
            &row.agent.adapter_id,
            row.state,
            glyphs,
            if selected { ink } else { dim },
            background,
            cx,
        ))
        .when(!row.location.host_letter.is_empty(), |this| {
            this.child(host_letter(&row.location.host_letter, selected, &palette))
        })
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(ui(4.))
                .child(div().min_w_0().truncate().child(session_label))
                .when(row.location.tab_pinned, |this| {
                    this.child(
                        svg()
                            .data(Glyph::Pin.svg())
                            .flex_none()
                            .size(ui(11.))
                            .text_color(dim),
                    )
                }),
        )
        .child(
            div()
                .flex_none()
                .max_w(relative(0.45))
                .truncate()
                .text_size(ui(12.))
                .text_color(dim)
                .child(row.location.workspace_name.clone()),
        );
    let group = SharedString::from(format!("agent-row-group-{key}"));
    let action_ink = if selected {
        palette.accent_ink
    } else {
        palette.chrome_dim
    };
    h_flex()
        .id(SharedString::from(format!("agent-row-item-{key}")))
        .group(group.clone())
        .role(Role::ListItem)
        .relative()
        .w_full()
        .mb(ui(1.))
        .child(button)
        // Outside the row, as the workspace list draws its own, and only
        // for the keyboard, as `:focus-visible`.
        .when(focused, |this| {
            this.when(window.last_input_was_keyboard(), |this| {
                this.child(
                    div()
                        .absolute()
                        .top(ui(-2.))
                        .left(ui(-2.))
                        .right(ui(-2.))
                        .bottom(ui(-2.))
                        .border_2()
                        .border_color(palette.accent)
                        .rounded(ui(RADIUS_CONTROL + 2.)),
                )
            })
            .child(crate::context_menu::anchor(&keys.focus))
        })
        // Over the row's right edge, on a backing of the row's own colour, as
        // the workspace list's pin: showing them takes no width from the row.
        .child(
            h_flex()
                .absolute()
                .top(ui(4.))
                .right(ui(4.))
                .p(ui(1.))
                .bg(if selected {
                    palette.accent
                } else if unavailable {
                    palette.chrome_bg
                } else {
                    palette.chrome_hover
                })
                .invisible()
                .group_hover(group, |style| style.visible())
                .child(
                    row_action(
                        format!("agent-pin-{key}").into(),
                        Glyph::Pin.svg(),
                        if row.location.tab_pinned {
                            "Unpin tab"
                        } else {
                            "Pin tab"
                        },
                        !row.routable,
                        action_ink,
                    )
                    .on_click(move |_, _, cx| toggle_tab_pin(&pin, cx)),
                )
                .child(
                    row_action(
                        format!("agent-branch-{key}").into(),
                        BRANCH,
                        "Branch conversation",
                        !can_branch(row, cx),
                        action_ink,
                    )
                    .on_click(move |_, _, cx| branch(&branch_row, cx)),
                ),
        )
        .context_menu(move |menu: PopupMenu, window, cx| {
            fill(menu, &row_menu(&menu_row, cx), window, cx)
        })
        .into_any_element()
}

fn heading(
    text: String,
    count: usize,
    dot: Option<(DisplayState, bool)>,
    tooltip: Option<String>,
    cx: &App,
) -> AnyElement {
    let palette = theme::palette(cx);
    h_flex()
        .id(SharedString::from(format!("agent-heading-{text}-{count}")))
        .role(Role::Heading)
        .items_center()
        .gap(ui(6.))
        .mx(ui(4.))
        .pt(ui(2.))
        .pb(ui(1.))
        .text_size(ui(TYPE_SECONDARY))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(palette.chrome_dim)
        .when_some(dot, |this, (state, glyphs)| {
            this.child(state_indicator(state, glyphs, 8., cx))
        })
        .child(div().min_w_0().truncate().child(text))
        .child(
            div()
                .text_color(palette.chrome_faint)
                .child(count.to_string()),
        )
        .when_some(tooltip, |this, tip| {
            this.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        })
        .into_any_element()
}

fn divider_label(text: &str, cx: &App) -> AnyElement {
    let palette = theme::palette(cx);
    div()
        .mx(ui(9.))
        .mt(ui(2.))
        .mb(ui(4.))
        .text_size(ui(TYPE_LABEL))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(palette.chrome_faint)
        .child(text.to_uppercase())
        .into_any_element()
}

fn group_box(children: Vec<AnyElement>) -> AnyElement {
    v_flex()
        .mx(ui(5.))
        .mb(ui(10.))
        .children(children)
        .into_any_element()
}

/// The sidebar's agents list: one line in `Sidebar` puts it under the section
/// header. Rows across every shown host, in the chosen order.
#[derive(IntoElement)]
pub struct AgentsList {
    pub sort: AgentSortMode,
    pub pinned_only: bool,
}

/// The rows in the order [`AgentsList`] draws them under `sort`.
fn display_order(rows: &[AgentListRow], sort: AgentSortMode) -> Vec<&AgentListRow> {
    match sort {
        AgentSortMode::Workspace => {
            let groups = group_by_workspace(rows);
            // Pinned workspaces' groups draw first when any are pinned.
            let (pinned, others): (Vec<_>, Vec<_>) =
                groups.into_iter().partition(|group| group.pinned);
            pinned
                .into_iter()
                .chain(others)
                .flat_map(|group| group.rows)
                .collect()
        }
        AgentSortMode::Pinned => group_by_pin(rows)
            .into_iter()
            .flat_map(|(_, half)| half)
            .collect(),
        AgentSortMode::Status => group_by_status(rows)
            .into_iter()
            .flat_map(|(_, _, group)| group)
            .collect(),
    }
}

impl RenderOnce for AgentsList {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = theme::palette(cx);
        let (_, rows) = super::rows(cx, self.sort, self.pinned_only);
        let scroll = window
            .use_keyed_state("agents-list-scroll", cx, |_, _| ScrollHandle::new())
            .read(cx)
            .clone();
        let (notice, glyphs, selected) = match cx.try_global::<Agents>() {
            Some(agents) => {
                let runtime = agents.runtime.read(cx);
                let focus = &runtime.inputs.focus;
                (
                    hook_wiring_notice(&host_hook_wiring(runtime.adapters(&focus.host_profile_id))),
                    crate::settings::get(cx).agent_state_glyphs,
                    selected_agent_id_for_pane(
                        &rows,
                        Some(&focus.pane_id),
                        Some(&focus.host_profile_id),
                    )
                    .map(|id| (focus.host_profile_id.clone(), id.to_owned())),
                )
            }
            None => (None, false, None),
        };
        let is_selected = |row: &AgentListRow| {
            selected
                .as_ref()
                .is_some_and(|(host, id)| *host == row.agent.host_profile_id && *id == row.agent.id)
        };
        let row_key =
            |row: &AgentListRow| format!("{}-{}", row.agent.host_profile_id, row.agent.id);
        // Up and Down follow the rows as they draw, so the keys come from the
        // display order rather than the list's.
        let order: Vec<String> = display_order(&rows, self.sort)
            .into_iter()
            .map(row_key)
            .collect();
        let handles: Vec<FocusHandle> = order
            .iter()
            .map(|key| {
                let focus = window
                    .use_keyed_state(
                        SharedString::from(format!("agent-focus-{key}")),
                        cx,
                        |_, cx| cx.focus_handle().tab_stop(true),
                    )
                    .read(cx)
                    .clone();
                crate::context_menu::reveal_when_focused(
                    format!("agent-reveal-{key}"),
                    &focus,
                    &scroll,
                    window,
                    cx,
                );
                focus
            })
            .collect();
        let keys_for = |row: &AgentListRow| {
            let at = order
                .iter()
                .position(|key| *key == row_key(row))
                .unwrap_or(0);
            RowKeys::for_rows(&handles, at)
        };
        let agent_row = |row: &AgentListRow, key: &str, selected: bool, glyphs: bool, cx: &App| {
            agent_row(row, key, selected, glyphs, keys_for(row), window, cx)
        };
        let quiet = |text: &'static str| {
            div()
                .px(ui(12.))
                .py(ui(10.))
                .text_size(ui(TYPE_SECONDARY))
                .text_color(palette.chrome_faint)
                .child(text)
                .into_any_element()
        };
        let mut body: Vec<AnyElement> = Vec::new();
        if rows.is_empty() {
            // Under the filter this list is not the whole host, so it must not
            // claim to be: the bell still counts every agent.
            body.push(quiet(if self.pinned_only {
                "No agents in pinned workspaces."
            } else {
                "No agents detected."
            }));
        } else {
            match self.sort {
                AgentSortMode::Workspace => {
                    let groups = group_by_workspace(&rows);
                    let render_group = |group: &super::list::WorkspaceGroup<'_>, cx: &App| {
                        let mut children = vec![heading(
                            group.workspace_name.clone(),
                            group.rows.len(),
                            None,
                            Some(format!("{} · {}", group.workspace_name, group.host_label)),
                            cx,
                        )];
                        children.extend(group.rows.iter().map(|row| {
                            agent_row(row, &row_key(row), is_selected(row), glyphs, cx)
                        }));
                        group_box(children)
                    };
                    if groups.iter().any(|group| group.pinned) {
                        for (label, pinned) in [("Pinned", true), ("Others", false)] {
                            let block: Vec<_> = groups
                                .iter()
                                .filter(|group| group.pinned == pinned)
                                .collect();
                            if block.is_empty() {
                                continue;
                            }
                            body.push(divider_label(label, cx));
                            body.extend(block.into_iter().map(|group| render_group(group, cx)));
                        }
                    } else {
                        body.extend(groups.iter().map(|group| render_group(group, cx)));
                    }
                }
                AgentSortMode::Pinned => {
                    for (label, half) in group_by_pin(&rows) {
                        body.push(divider_label(label, cx));
                        body.push(group_box(
                            half.iter()
                                .map(|row| {
                                    agent_row(
                                        row,
                                        &format!("{label}-{}", row_key(row)),
                                        is_selected(row),
                                        glyphs,
                                        cx,
                                    )
                                })
                                .collect(),
                        ));
                    }
                }
                AgentSortMode::Status => {
                    for (label, state, group) in group_by_status(&rows) {
                        let mut children = vec![heading(
                            label.to_owned(),
                            group.len(),
                            Some((state, glyphs)),
                            None,
                            cx,
                        )];
                        children.extend(group.iter().map(|row| {
                            agent_row(
                                row,
                                &format!("{label}-{}", row_key(row)),
                                is_selected(row),
                                glyphs,
                                cx,
                            )
                        }));
                        body.push(group_box(children));
                    }
                }
            }
        }
        v_flex()
            .id("sidebar-agents")
            .flex_1()
            .min_h_0()
            .when_some(notice, |this, notice| {
                // Above the list, not instead of it: the rows are real, and
                // what is missing is any way to know what they are doing.
                this.child(
                    div()
                        .id("agents-notice")
                        .role(Role::Note)
                        .aria_label(notice.clone())
                        .flex_none()
                        .px(ui(8.))
                        .py(ui(5.))
                        .text_size(ui(TYPE_SECONDARY))
                        .line_height(relative(1.35))
                        .text_color(palette.chrome_dim)
                        .child(notice),
                )
            })
            .child(
                v_flex()
                    .id("sidebar-agents-list")
                    .test_support()
                    .role(Role::List)
                    .aria_label("Agents")
                    .track_scroll(&scroll)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(body)
                    .context_menu(|menu: PopupMenu, window, cx| {
                        fill(menu, &section_menu(cx), window, cx)
                    }),
            )
    }
}

/// Carries the setup prompt and the hook review, opened and closed as the
/// runtime's state says, as `ConnectionSurface` carries the helper dialog.
pub struct AgentsSurface {
    runtime: Option<Entity<AgentRuntime>>,
    question_open: bool,
    review_open: bool,
    /// Retries an unsolicited setup prompt held back behind another dialog.
    question_retry: Option<Task<()>>,
    _subscription: Option<Subscription>,
}

impl RootPlugin for AgentsSurface {}

impl Render for AgentsSurface {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl AgentsSurface {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let runtime = cx
            .try_global::<Agents>()
            .map(|agents| agents.runtime.clone());
        let subscription = runtime.as_ref().map(|runtime| {
            cx.observe_in(runtime, window, |this, _, window, cx| this.sync(window, cx))
        });
        Self {
            runtime,
            question_open: false,
            review_open: false,
            question_retry: None,
            _subscription: subscription,
        }
    }

    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let (question, review) = {
            let runtime = runtime.read(cx);
            (
                runtime.setup.question.is_some(),
                runtime.setup.review.is_some(),
            )
        };
        if question && !self.question_open && window.has_active_dialog(cx) {
            // The prompt is unsolicited: it never stacks on a dialog the user
            // is in. The runtime may not change again, so this polls instead.
            if self.question_retry.is_none() {
                self.question_retry = Some(cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(QUESTION_RETRY_MS))
                        .await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.question_retry = None;
                        this.sync(window, cx);
                    });
                }));
            }
        } else if question != self.question_open {
            self.question_open = question;
            if question {
                // Escape closes it inside the Kit: `sync` must not close another.
                let surface = cx.weak_entity();
                let closed: Rc<dyn Fn(&mut App)> = Rc::new(move |cx| {
                    surface
                        .update(cx, |this, _| this.question_open = false)
                        .ok();
                });
                open_setup_dialog(runtime.clone(), closed, window, cx);
            } else {
                window.close_dialog(cx);
            }
        }
        if review != self.review_open {
            self.review_open = review;
            if review {
                let surface = cx.weak_entity();
                let closed: Rc<dyn Fn(&mut App)> = Rc::new(move |cx| {
                    surface.update(cx, |this, _| this.review_open = false).ok();
                });
                open_review_dialog(runtime, closed, window, cx);
            } else {
                window.close_dialog(cx);
            }
        }
        if self.question_open || self.review_open {
            window.refresh();
        }
    }
}

const QUESTION_RETRY_MS: u64 = 500;

/// `.host-setup` and `.hook-review`.
const SETUP_WIDTH: f32 = 560.;
const REVIEW_WIDTH: f32 = 620.;

/// `.host-setup-body p { line-height: 1.45 }`.
const PARAGRAPH_LINE: f32 = 1.45;

fn paragraph(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    paragraph_of(text.into(), cx)
}

fn paragraph_of(content: impl IntoElement, cx: &App) -> AnyElement {
    div()
        .text_size(ui(TYPE_ROW))
        .line_height(relative(PARAGRAPH_LINE))
        .text_color(theme::palette(cx).chrome_dim)
        .child(content)
        .into_any_element()
}

fn code(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    div()
        .font_family(fonts::MONO)
        .text_size(ui(TYPE_META))
        .text_color(theme::palette(cx).chrome_ink)
        .child(text.into())
        .into_any_element()
}

fn error_line(error: &str) -> AnyElement {
    SurfaceError::new("agents-dialog-error", error).into_any_element()
}

fn progress(text: &'static str, cx: &App) -> AnyElement {
    h_flex()
        .id("agents-dialog-progress")
        .test_support()
        .role(Role::Status)
        .aria_label(text)
        .gap(ui(8.))
        .items_center()
        .text_size(ui(TYPE_SECONDARY))
        .text_color(theme::palette(cx).chrome_dim)
        .child(Spinner::new().small())
        .child(text)
        .into_any_element()
}

fn setup_body(question: &SetupQuestion, cx: &App) -> Vec<AnyElement> {
    let mut body = vec![paragraph(
        "Agents on this host cannot report what they are doing yet. Setting up adds this app’s lifecycle hooks alongside whatever is already configured — nothing existing is replaced, and a timestamped backup is written before the first change.",
        cx,
    )];
    let palette = theme::palette(cx);
    body.extend(question.targets.iter().map(|adapter| {
        // `.host-setup-paths li`.
        v_flex()
            .gap(ui(2.))
            .px(ui(10.))
            .py(ui(8.))
            .border_1()
            .border_color(palette.chrome_hairline)
            .rounded(ui(RADIUS_CONTROL))
            .bg(palette.chrome_bg)
            .child(
                div()
                    .font_weight(FontWeight::BOLD)
                    .text_color(palette.chrome_ink)
                    .child(adapter.display_name.clone()),
            )
            .child(
                div()
                    .font_family(fonts::MONO)
                    .text_size(ui(TYPE_META))
                    .text_color(palette.chrome_dim)
                    .child(adapter.hook_config_path.clone()),
            )
            .into_any_element()
    }));
    body.push(paragraph(
        "It also asks this host’s tmux server to name agent windows after what the agent is working on. Only agent windows — everything else keeps the name tmux gives it — and nothing is written to your tmux config.",
        cx,
    ));
    if question.targets.iter().any(|adapter| adapter.id == "codex") {
        // The host's own text is plain; this one names files and settings.
        body.push(paragraph_of(
            Prose::new("New tmux panes also get an empty `CODEX_EXEC_SERVER_URL`, which keeps Codex off its shared background server so its hooks can tell which pane they came from, and `~/.codex/config.toml` gets `daemon_auto_start = false`, so Codex on this host no longer starts that server on its own or warns about it."),
            cx,
        ));
    }
    body.push(
        div()
            .text_size(ui(TYPE_SECONDARY))
            .text_color(theme::palette(cx).chrome_faint)
            .child("Until then the agents list stays honest: it shows which agents exist and says nothing about what they are doing.")
            .into_any_element(),
    );
    if let Some(error) = &question.error {
        body.push(error_line(error));
    }
    if let Some(activity) = question.activity {
        body.push(progress(
            match activity {
                Activity::Install => "Setting up agent hooks…",
                Activity::Review => "Loading the hook changes…",
            },
            cx,
        ));
    }
    body
}

fn open_setup_dialog(
    runtime: Entity<AgentRuntime>,
    closed: Rc<dyn Fn(&mut App)>,
    window: &mut Window,
    cx: &mut App,
) {
    window.open_dialog(cx, move |dialog, window, cx| {
        let Some(question) = runtime.read(cx).setup.question.clone() else {
            return dialog;
        };
        let busy = question.activity.is_some();
        let palette = theme::palette(cx);
        let title = SharedString::from(format!("Set up agent status on {}?", question.label));
        let (decline, review, accept, escaped) = (
            runtime.clone(),
            runtime.clone(),
            runtime.clone(),
            runtime.clone(),
        );
        dialog::card(dialog, &palette)
            // The title block sits flush on the body, as the header does in `styles.css`.
            .title(
                v_flex()
                    .mb(ui(-8.))
                    .child(
                        div()
                            .text_size(ui(TYPE_META))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(palette.chrome_faint)
                            .child("One-time setup"),
                    )
                    .child(
                        div()
                            .mt(ui(2.))
                            .text_size(ui(16.))
                            .font_weight(FontWeight::BOLD)
                            .child(title.clone()),
                    ),
            )
            .centered()
            .w(ui(SETUP_WIDTH).to_pixels(window.rem_size()))
            .p(ui(20.))
            .close_button(false)
            .keyboard(!busy)
            .overlay_closable(false)
            .on_close({
                let closed = closed.clone();
                move |_, _, cx| {
                    closed(cx);
                    escaped.update(cx, |runtime, cx| runtime.decline_setup(cx));
                }
            })
            .child(
                v_flex()
                    .id("host-setup")
                    .test_support()
                    .role(Role::AlertDialog)
                    .aria_label(title)
                    .gap(ui(10.))
                    .children(setup_body(&question, cx)),
            )
            .footer(
                DialogFooter::new()
                    .gap(ui(8.))
                    .mt(ui(-6.))
                    .pt(ui(12.))
                    .border_t_1()
                    .border_color(palette.chrome_hairline)
                    .child(dialog::button(
                        Button::new("host-setup-decline")
                            .label("Not now")
                            .disabled(busy)
                            .on_click(move |_, _, cx| {
                                decline.update(cx, |runtime, cx| runtime.decline_setup(cx))
                            }),
                        Action::Plain,
                        busy,
                        &palette,
                    ))
                    .child(dialog::button(
                        Button::new("host-setup-review")
                            .label(if question.activity == Some(Activity::Review) {
                                "Loading review…"
                            } else {
                                "Review exact changes…"
                            })
                            .disabled(busy)
                            .on_click(move |_, _, cx| {
                                review.update(cx, |runtime, cx| runtime.review_setup(cx))
                            }),
                        Action::Plain,
                        busy,
                        &palette,
                    ))
                    .child(dialog::button(
                        Button::new("host-setup-accept")
                            .label(if question.activity == Some(Activity::Install) {
                                "Setting up…"
                            } else {
                                "Set up this host"
                            })
                            .disabled(busy)
                            .on_click(move |_, _, cx| {
                                accept.update(cx, |runtime, cx| runtime.accept_setup(cx))
                            }),
                        Action::Primary,
                        busy,
                        &palette,
                    )),
            )
    });
}

fn or_none(hash: &str) -> String {
    if hash.is_empty() {
        "none".into()
    } else {
        hash.to_owned()
    }
}

fn preview_block(id: String, label: String, text: &str, palette: &Palette) -> impl IntoElement {
    div()
        .id(SharedString::from(id))
        .test_support()
        .aria_label(label)
        .p(ui(8.))
        .rounded(ui(RADIUS_ROW))
        .bg(palette.chrome_raised)
        .max_h(ui(220.))
        .overflow_y_scroll()
        .font_family(fonts::MONO)
        .text_size(ui(TYPE_META))
        .whitespace_normal()
        .children(text.lines().map(|line| div().child(line.to_owned())))
}

/// `narrow` stacks Before over After, as `styles.css` does in a window 600 CSS
/// px or narrower.
fn change_rows(open: &OpenReview, narrow: bool, cx: &App) -> Vec<AnyElement> {
    let palette = theme::palette(cx);
    open.review
        .changes
        .iter()
        .map(|change| {
            let field = |term: &'static str, value: String, mono: bool| {
                h_flex()
                    .gap(ui(8.))
                    .child(
                        div()
                            .w(ui(120.))
                            .flex_none()
                            .text_size(ui(TYPE_SECONDARY))
                            .text_color(palette.chrome_faint)
                            .child(term),
                    )
                    // The value wraps: the exact command is what is being reviewed.
                    .child(div().flex_1().min_w_0().child(if mono {
                        code(value, cx)
                    } else {
                        paragraph(value, cx)
                    }))
            };
            let operation = if change.creates_config {
                "Create config"
            } else if change.removes_config {
                "Remove config"
            } else {
                "Update config"
            };
            v_flex()
                .gap(ui(4.))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(change.path.clone()))
                .child(paragraph(change.summary.clone(), cx))
                .child(field(
                    "Ownership marker",
                    if change.owner.is_empty() { "No ownership marker".into() } else { change.owner.clone() },
                    true,
                ))
                .child(field(
                    "Command",
                    if change.command.is_empty() { "No command supplied".into() } else { change.command.clone() },
                    true,
                ))
                .child(field(
                    "Events",
                    if change.events.is_empty() { "No events supplied".into() } else { change.events.join(", ") },
                    false,
                ))
                .child(field("Operation", operation.into(), false))
                .child(field("Before hash", or_none(&change.before_hash), true))
                .child(field("After hash", or_none(&change.after_hash), true))
                .when(!change.diff_preview.is_empty(), |this| {
                    this.child(div().font_weight(FontWeight::SEMIBOLD).child("Redacted configuration diff"))
                        .child(preview_block(
                            format!("hook-diff-{}", change.path),
                            format!("Redacted configuration diff for {}", change.path),
                            &change.diff_preview,
                            &palette,
                        ))
                })
                // A whole-file change has no diff: both sides are shown instead.
                .when(
                    change.diff_preview.is_empty()
                        && !(change.before_preview.is_empty() && change.after_preview.is_empty()),
                    |this| {
                        let side = |title: &'static str, text: &str| {
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(ui(4.))
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                                .child(preview_block(
                                    format!("hook-{title}-{}", change.path),
                                    format!("{title} for {}", change.path),
                                    if text.is_empty() { "(empty)" } else { text },
                                    &palette,
                                ))
                        };
                        this.child(
                            div()
                                .flex()
                                .map(|pair| if narrow { pair.flex_col() } else { pair.items_start() })
                                .gap(ui(8.))
                                .child(side("Before · redacted", &change.before_preview))
                                .child(side("After · redacted", &change.after_preview)),
                        )
                    },
                )
                .when(change.preview_truncated, |this| {
                    this.child(paragraph(
                        "Preview truncated at the host’s bounded redaction limit; hashes identify the complete configurations.",
                        cx,
                    ))
                })
                .into_any_element()
        })
        .collect()
}

fn open_review_dialog(
    runtime: Entity<AgentRuntime>,
    closed: Rc<dyn Fn(&mut App)>,
    window: &mut Window,
    cx: &mut App,
) {
    window.open_dialog(cx, move |dialog, window, cx| {
        let Some(open) = runtime.read(cx).setup.review.clone() else {
            return dialog;
        };
        let installing = open.review.action == HookAction::Install;
        let verb = if installing { "Install" } else { "Uninstall" };
        let title = SharedString::from(format!("{verb} {} hooks?", open.review.adapter_display_name));
        let (cancel, confirm, escaped) = (runtime.clone(), runtime.clone(), runtime.clone());
        let palette = theme::palette(cx);
        let confirm_button = dialog::button(
            Button::new("hook-review-confirm")
                .label(if open.applying {
                    "Applying…".to_owned()
                } else {
                    format!("{verb} reviewed hooks")
                })
                .disabled(open.applying)
                .on_click(move |_, _, cx| confirm.update(cx, |runtime, cx| runtime.confirm_review(cx))),
            if installing { Action::Primary } else { Action::Danger },
            open.applying,
            &palette,
        );
        dialog::card(dialog, &palette)
            .title(title.clone())
            .centered()
            .p(ui(20.))
            .w(ui(REVIEW_WIDTH).to_pixels(window.rem_size()))
            .close_button(false)
            .keyboard(!open.applying)
            .overlay_closable(false)
            .on_close({
                let closed = closed.clone();
                move |_, _, cx| {
                    closed(cx);
                    escaped.update(cx, |runtime, cx| runtime.cancel_review(cx));
                }
            })
            .child(
                v_flex()
                    .id("hook-review")
                    .test_support()
                    .role(Role::AlertDialog)
                    .aria_label(title)
                    .gap(ui(10.))
                    .max_h(ui(480.))
                    .overflow_y_scroll()
                    .child(
                        div()
                            .text_size(ui(TYPE_LABEL))
                            .text_color(theme::palette(cx).chrome_faint)
                            .child("Review before changing config"),
                    )
                    .child(paragraph(
                        format!(
                            "Review the exact host-proposed configuration changes. Unrelated configuration must remain untouched. Plan label: {}.",
                            open.review.managed_label
                        ),
                        cx,
                    ))
                    .children(change_rows(&open, narrower_than(NARROW_DIALOGS, window), cx))
                    .when_some(open.review.trust_guidance.clone(), |this, guidance| this.child(paragraph(guidance, cx)))
                    .when_some(open.review.backup_path.clone(), |this, backup| {
                        this.child(paragraph(format!("Backup: {backup}"), cx))
                    })
                    .when(open.review.already_installed, |this| {
                        this.child(paragraph("The managed hook is already current. Confirming is idempotent.", cx))
                    })
                    .when_some(open.error.clone(), |this, error| this.child(error_line(&error)))
                    .when(open.applying, |this| {
                        this.child(progress(
                            if installing { "Installing agent hooks…" } else { "Removing agent hooks…" },
                            cx,
                        ))
                    }),
            )
            .footer(
                DialogFooter::new()
                    .child(dialog::button(
                        Button::new("hook-review-cancel")
                            .label("Cancel")
                            .disabled(open.applying)
                            .on_click(move |_, _, cx| cancel.update(cx, |runtime, cx| runtime.cancel_review(cx))),
                        Action::Plain,
                        open.applying,
                        &palette,
                    ))
                    .child(confirm_button),
            )
    });
}

/// The dialogs' busy lines, as `SetupProgress` speaks them in Tauri.
#[cfg(test)]
mod tests {
    use std::path::Path;

    use desktop_core::connection::agent::AgentCommand;
    use futures::future::{BoxFuture, FutureExt};
    use gpui_kit::{TestAppContext, VisualTestContext, test::TestQueryExt};
    use serde_json::Value;

    use super::*;
    use crate::{
        agents::{
            runtime::AgentTransport,
            setup::{Activity, ConsentedHost, OpenReview, SetupQuestion},
            tests::{Effects, Recorded},
            wire::HookReview,
        },
        app::{self, Core},
    };

    struct Unused;

    impl AgentTransport for Unused {
        fn agent_request(
            &self,
            _: String,
            _: AgentCommand,
        ) -> BoxFuture<'static, Result<Value, String>> {
            futures::future::ready(Err("not asked here".into())).boxed()
        }
    }

    /// A runtime and a window root that can hold its dialogs.
    fn rooted(cx: &mut TestAppContext) -> (Entity<AgentRuntime>, &mut VisualTestContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            app::init(Core::at(Path::new("/nonexistent/app-state.json")), cx);
        });
        let runtime = cx.new(|_| {
            AgentRuntime::new(
                Rc::new(Unused),
                Rc::new(Recorded(Rc::new(Effects::default()))),
            )
        });
        let (_, visual) = cx.add_window_view(|window, cx| {
            let empty = cx.new(|_| EmptyView);
            gpui_kit::base::Root::new(empty, window, cx)
        });
        (runtime, visual)
    }

    fn status_role(visual: &mut VisualTestContext, line: &str) -> Option<Role> {
        visual.update(|window, _| {
            window
                .try_find_by_label(line)
                .and_then(|element| element.role())
        })
    }

    fn alert_names(visual: &mut VisualTestContext) -> Vec<String> {
        visual.update(|window, _| {
            window
                .find_all_by_role(Role::AlertDialog)
                .iter()
                .filter_map(|element| element.label().map(str::to_owned))
                .collect()
        })
    }

    /// The dialogs read the runtime on every build, as the surface redraws them.
    fn redraw(visual: &mut VisualTestContext) {
        visual.update(|window, _| window.refresh());
        visual.run_until_parked();
    }

    #[gpui_kit::test]
    fn the_setup_prompt_says_what_it_is_doing_while_busy(cx: &mut TestAppContext) {
        let (runtime, visual) = rooted(cx);
        runtime.update(visual, |runtime, _| {
            runtime.setup.question = Some(SetupQuestion {
                id: 1,
                host: None,
                label: "This Mac".into(),
                targets: Vec::new(),
                activity: None,
                error: None,
            })
        });
        visual.update(|window, cx| open_setup_dialog(runtime.clone(), Rc::new(|_| {}), window, cx));
        visual.run_until_parked();
        assert_eq!(
            alert_names(visual),
            ["Set up agent status on This Mac?"],
            "named by its title"
        );
        let lines = ["Setting up agent hooks…", "Loading the hook changes…"];
        for line in lines {
            assert_eq!(status_role(visual, line), None, "idle: {line}");
        }
        for (activity, line) in [(Activity::Install, lines[0]), (Activity::Review, lines[1])] {
            runtime.update(visual, |runtime, _| {
                runtime.setup.question.as_mut().unwrap().activity = Some(activity)
            });
            redraw(visual);
            assert_eq!(status_role(visual, line), Some(Role::Status), "{line}");
        }
    }

    #[gpui_kit::test]
    fn the_hook_review_says_what_it_is_applying(cx: &mut TestAppContext) {
        let (runtime, visual) = rooted(cx);
        let review = |action, applying| OpenReview {
            review: HookReview {
                adapter_id: "claude-code".into(),
                adapter_display_name: "Claude Code".into(),
                action,
                revision: "confirm".into(),
                already_installed: false,
                changes: Vec::new(),
                managed_label: "muxflow/v1".into(),
                trust_guidance: None,
                backup_path: None,
            },
            host: ConsentedHost {
                profile_id: "local".into(),
                identity: "local".into(),
            },
            applying,
            error: None,
        };
        runtime.update(visual, |runtime, _| {
            runtime.setup.review = Some(review(HookAction::Install, false))
        });
        visual
            .update(|window, cx| open_review_dialog(runtime.clone(), Rc::new(|_| {}), window, cx));
        visual.run_until_parked();
        assert_eq!(
            alert_names(visual),
            ["Install Claude Code hooks?"],
            "named by its title"
        );
        assert_eq!(status_role(visual, "Installing agent hooks…"), None);
        for (action, line) in [
            (HookAction::Install, "Installing agent hooks…"),
            (HookAction::Uninstall, "Removing agent hooks…"),
        ] {
            runtime.update(visual, |runtime, _| {
                runtime.setup.review = Some(review(action, true))
            });
            redraw(visual);
            assert_eq!(status_role(visual, line), Some(Role::Status), "{line}");
        }
    }

    #[gpui_kit::test]
    fn a_narrow_window_stacks_before_over_after(cx: &mut TestAppContext) {
        let (runtime, visual) = rooted(cx);
        let change = crate::agents::wire::HookChange {
            path: "/cfg/settings.json".into(),
            summary: String::new(),
            owner: String::new(),
            command: String::new(),
            events: Vec::new(),
            creates_config: false,
            removes_config: false,
            diff_preview: String::new(),
            before_preview: "{}".into(),
            after_preview: "{ \"hooks\": {} }".into(),
            before_hash: String::new(),
            after_hash: String::new(),
            preview_truncated: false,
        };
        runtime.update(visual, |runtime, _| {
            runtime.setup.review = Some(OpenReview {
                review: HookReview {
                    adapter_id: "claude-code".into(),
                    adapter_display_name: "Claude Code".into(),
                    action: HookAction::Install,
                    revision: "confirm".into(),
                    already_installed: false,
                    changes: vec![change],
                    managed_label: "muxflow/v1".into(),
                    trust_guidance: None,
                    backup_path: None,
                },
                host: ConsentedHost {
                    profile_id: "local".into(),
                    identity: "local".into(),
                },
                applying: false,
                error: None,
            })
        });
        visual.simulate_resize(gpui_kit::size(gpui_kit::px(1200.), gpui_kit::px(900.)));
        visual
            .update(|window, cx| open_review_dialog(runtime.clone(), Rc::new(|_| {}), window, cx));
        visual.run_until_parked();
        let sides = |visual: &mut VisualTestContext| {
            redraw(visual);
            visual.update(|window, _| {
                let at = |label: &str| window.find_by_label(label).bounds().origin;
                (
                    at("Before · redacted for /cfg/settings.json"),
                    at("After · redacted for /cfg/settings.json"),
                )
            })
        };
        let (before, after) = sides(visual);
        assert!(
            after.x > before.x && after.y == before.y,
            "{before:?} {after:?}"
        );
        visual.simulate_resize(gpui_kit::size(gpui_kit::px(500.), gpui_kit::px(900.)));
        let (before, after) = sides(visual);
        assert!(
            after.y > before.y && after.x == before.x,
            "{before:?} {after:?}"
        );
    }

    struct OneRow(AgentListRow, FocusHandle);

    impl Render for OneRow {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let keys = workspaces::RowKeys::for_rows(std::slice::from_ref(&self.1), 0);
            div()
                .w(gpui_kit::px(240.))
                .child(agent_row(&self.0, "a", true, false, keys, window, cx))
        }
    }

    /// Tauri's `.agent-button { width: 100% }`, with `.row-actions` over it.
    #[gpui_kit::test]
    fn an_agent_row_spans_the_list_with_its_actions_over_it(cx: &mut TestAppContext) {
        use crate::agents::{
            list::{AgentLocation, PriorityBucket},
            model::fixtures,
        };
        use gpui_kit::test::TestWindowExt as _;

        cx.update(|cx| {
            gpui_kit::init(cx);
            app::init(Core::at(Path::new("/nonexistent/app-state.json")), cx);
        });
        let agent = fixtures::agent();
        let row = AgentListRow {
            state: DisplayState::Working,
            bucket: PriorityBucket::Working,
            location: AgentLocation {
                host_letter: "L".into(),
                ..AgentLocation::unlisted(&agent)
            },
            routable: true,
            pinned: false,
            agent,
        };
        let (_, visual) = cx.add_window_view(move |window, cx| {
            let shown = cx.new(|cx| OneRow(row, cx.focus_handle()));
            gpui_kit::base::Root::new(shown, window, cx)
        });
        visual.run_until_parked();
        visual.update(|window, _| {
            let row = window.find("agent-row-a").bounds();
            let pin = window.find_by_label("Pin tab").bounds();
            assert_eq!(
                row.size.width,
                gpui_kit::px(240.),
                "the row is the list's width"
            );
            assert!(
                pin.right() <= row.right()
                    && pin.top() >= row.top()
                    && pin.bottom() <= row.bottom(),
                "the actions sit over the row: {pin:?} in {row:?}"
            );
        });
    }
}
