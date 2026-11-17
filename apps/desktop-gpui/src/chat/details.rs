//! The session details card at the top right of a chat: the host and folder,
//! the branch with a menu to switch it, Commit…, the uncommitted changes and
//! the branch's against the default branch, and the context window's use.
//!
//! The Git rows are the active repository's, as the Git panel follows it, and
//! show only while the chat's folder lies in it. Clicking into the chat makes
//! its pane the active one, so they follow the chat that is being used.
//!
//! Where the chat is narrower than [`DETAILS_ROOM`] the card would cover the
//! conversation, so only a Details button shows, which opens the card under
//! it.

use std::rc::Rc;

use desktop_core::connection::chat::ChatUsage;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement,
    IntoElement, ParentElement, Render, Role, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Task, TestSupportExt as _, Window,
    base::Selectable as _,
    component::{
        Disableable, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Enter, Input, InputEvent, InputState},
        popover::Popover,
        tooltip::Tooltip,
        v_flex,
    },
    div,
    prelude::FluentBuilder,
};

use crate::{
    connection_ui::ConnectionUi,
    git::{
        self,
        model::{self, BranchSummary, Branches, CommandResult, StatusSnapshot, SummaryBase},
        panel::{GitPanel, Scope},
    },
    theme::{self, Palette, metrics::ui},
    ui::toast,
    workspaces::rows::{abbreviate_home, infer_home},
};

use super::{
    DETAILS_ROOM, DetailsContext,
    composer::tokens,
    icon::{IconName, icon},
    set_details_card,
};

/// `.dcard { width: 238px }`, which the branch menu matches.
const WIDTH: f32 = 238.;
/// The tallest the branch list grows before it scrolls.
const LIST_HEIGHT: f32 = 240.;

/// Called once from [`super::init`].
pub(super) fn init(cx: &mut App) {
    set_details_card(Rc::new(card), cx);
}

fn card(context: &DetailsContext, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    let key = SharedString::from(format!("chat-details-{}", context.pane.id));
    // Lives as long as the chat draws it.
    let card = window.use_keyed_state(key, cx, Card::new);
    let shown = Shown {
        profile_id: context.profile_id.clone(),
        server_identity: context.server_identity.clone(),
        path: context.pane.current_path.clone(),
        usage: context.usage,
        wide: context.width >= ui(DETAILS_ROOM).to_pixels(window.rem_size()),
    };
    card.update(cx, |card, cx| {
        if card.shown != shown {
            card.shown = shown;
            cx.notify();
        }
    });
    Some(card.into_any_element())
}

/// What the chat says about itself.
#[derive(Default, PartialEq)]
struct Shown {
    profile_id: String,
    server_identity: String,
    /// The pane's folder.
    path: String,
    usage: Option<ChatUsage>,
    /// Whether the card sits beside the conversation rather than over it.
    wide: bool,
}

pub(super) struct Card {
    shown: Shown,
    /// Over a narrow chat: whether the Details button has opened the card.
    open: bool,
    /// Whether the branch menu is open, so a press in it leaves the card open.
    menu: bool,
    /// The branch menu's list: `None` while it is read.
    branches: Option<Result<Branches, String>>,
    filter: Entity<InputState>,
    /// The branch being switched to.
    switching: Option<String>,
    /// Why the last switch did not happen.
    error: Option<String>,
    git: Option<Entity<GitPanel>>,
    /// The list being read; dropping it cancels the read at the host.
    listing: Option<Task<()>>,
    checkout: Option<Task<()>>,
    /// Whether this card has asked the panel to read the summaries.
    reading: bool,
    _subscriptions: Vec<Subscription>,
}

impl Card {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter branches"));
        let mut subscriptions = vec![cx.subscribe(&filter, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })];
        let git = git::panel(cx);
        if let Some(panel) = &git {
            subscriptions.push(cx.observe(panel, |_, _, cx| cx.notify()));
            subscriptions.push(cx.on_release({
                let panel = panel.downgrade();
                move |card, cx| {
                    if card.reading {
                        panel
                            .update(cx, |panel, cx| panel.read_summaries(false, cx))
                            .ok();
                    }
                }
            }));
        }
        Self {
            shown: Shown::default(),
            open: false,
            menu: false,
            branches: None,
            filter,
            switching: None,
            error: None,
            git,
            listing: None,
            checkout: None,
            reading: false,
            _subscriptions: subscriptions,
        }
    }

    /// Asks the panel to keep the summaries read while the card shows them,
    /// whichever scope the panel lists, and no longer. A menu whose row is
    /// gone is closed.
    fn follow_repository(&mut self, cx: &mut Context<Self>) {
        let ours = self.repository(cx).is_some();
        if !ours {
            self.menu = false;
        }
        let reading = ours && (self.shown.wide || self.open);
        if reading == self.reading {
            return;
        }
        self.reading = reading;
        if let Some(panel) = self.git.clone() {
            panel.update(cx, |panel, cx| panel.read_summaries(reading, cx));
        }
    }

    /// The repository the chat's folder lies in, while it is the one the Git
    /// panel follows on the same host.
    fn repository(&self, cx: &App) -> Option<Rc<StatusSnapshot>> {
        let panel = self.git.as_ref()?.read(cx);
        let scope = panel.context().scope.as_ref()?;
        if scope.host_profile_id != self.shown.profile_id
            || scope.server_identity != self.shown.server_identity
        {
            return None;
        }
        let status = panel.status(cx)?;
        within(&self.shown.path, &status.repository.worktree_root).then_some(status)
    }

    /// The summary against `base` as last read, and whether reading it failed.
    fn summary(&self, base: SummaryBase, cx: &App) -> (Option<Rc<BranchSummary>>, bool) {
        self.git.as_ref().map_or((None, false), |panel| {
            let panel = panel.read(cx);
            (panel.summary(base), panel.summary_failed(base))
        })
    }

    /// The host's home, inferred from its panes' folders as the workspace
    /// rows do.
    fn home(&self, cx: &App) -> Option<String> {
        let ui = cx.try_global::<ConnectionUi>()?;
        let controller = ui.0.read(cx);
        let link = controller.link(&self.shown.profile_id)?;
        infer_home(
            link.host_state
                .snapshot
                .panes
                .iter()
                .map(|pane| pane.current_path.as_str()),
        )
    }

    fn host(&self, cx: &App) -> String {
        cx.try_global::<ConnectionUi>()
            .and_then(|ui| {
                ui.0.read(cx)
                    .profiles()
                    .iter()
                    .find(|profile| profile.id == self.shown.profile_id)
                    .map(|profile| profile.label.clone())
            })
            .unwrap_or_else(|| self.shown.profile_id.clone())
    }

    fn list_branches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter
            .update(cx, |filter, cx| filter.set_value("", window, cx));
        self.branches = None;
        let read = self
            .git
            .as_ref()
            .and_then(|panel| panel.read(cx).branches(cx));
        let Some(read) = read else {
            self.branches = Some(Err("Git is not available right now.".into()));
            return;
        };
        self.listing = Some(cx.spawn(async move |this, cx| {
            let branches = read.await;
            this.update(cx, |this, cx| {
                this.branches = Some(branches);
                cx.notify();
            })
            .ok();
        }));
    }

    /// The branches the filter leaves, in the host's order.
    fn matches(&self, cx: &App) -> Vec<String> {
        let Some(Ok(branches)) = &self.branches else {
            return Vec::new();
        };
        filtered(&branches.names, &self.filter.read(cx).value())
    }

    fn switch(&mut self, branch: String, cx: &mut Context<Self>) {
        if self.switching.is_some() {
            return;
        }
        // The panel may have moved to another repository since the menu opened.
        let Some(status) = self.repository(cx) else {
            return;
        };
        if status.repository.head_name.as_deref() == Some(branch.as_str()) {
            return;
        }
        let task = self
            .git
            .clone()
            .and_then(|panel| panel.update(cx, |panel, cx| panel.checkout(branch.clone(), cx)));
        let Some(task) = task else {
            self.error = Some("Git is not available right now.".into());
            cx.notify();
            return;
        };
        self.error = None;
        self.switching = Some(branch);
        cx.notify();
        self.checkout = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.switching = None;
                this.error = match result {
                    Ok(result) if result.applied() => None,
                    Ok(result) => Some(refusal(&result)),
                    Err(error) => Some(error),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    /// Done with the card over a narrow chat.
    fn close(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.open = false;
            self.error = None;
            cx.notify();
        }
    }

    /// A changes row that opens the Git panel listing `scope`.
    fn opens(&self, row: Button, scope: Scope, narrow: bool, cx: &mut Context<Self>) -> Button {
        row.on_click(cx.listener(move |this, _, window, cx| {
            git::show(scope, window, cx);
            if narrow {
                this.close(cx);
            }
        }))
    }

    fn branch_row(
        &self,
        status: &StatusSnapshot,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let repository = &status.repository;
        let name: SharedString = match (&self.switching, &repository.head_name) {
            (Some(branch), _) => branch.clone().into(),
            (None, Some(name)) if !repository.detached_head => name.clone().into(),
            _ => "Detached HEAD".into(),
        };
        let note = if self.switching.is_some() {
            extra("Switching…".into(), p).into_any_element()
        } else {
            // The chevron says the row is a menu; the faint `.x` is too slight.
            icon(IconName::ChevronDown, 14., p.chrome_dim).into_any_element()
        };
        let trigger = action("chat-details-branch", format!("Branch: {name}"), p)
            .disabled(self.switching.is_some())
            .child(row(IconName::GitBranch, name, None, p).child(note));
        if self.switching.is_some() {
            // No menu opens while a switch runs.
            return trigger.into_any_element();
        }
        let this = cx.entity().downgrade();
        let opened = this.clone();
        Popover::new("chat-details-branches")
            .trigger(trigger)
            .trigger_style(gpui_kit::StyleRefinement::default().w_full())
            .track_focus(&self.filter.read(cx).focus_handle(cx))
            // As wide as the row it opens from, inside the card's edges.
            .w(ui(WIDTH - 22.))
            .p(ui(6.))
            .on_open_change(move |open, window, cx| {
                opened
                    .update(cx, |card, cx| {
                        card.menu = *open;
                        if *open {
                            card.error = None;
                            card.list_branches(window, cx);
                        } else {
                            card.listing = None;
                        }
                        cx.notify();
                    })
                    .ok();
            })
            .content(move |_, _, cx| {
                let popover = cx.entity().downgrade();
                let Some(card) = this.upgrade() else {
                    return div().into_any_element();
                };
                let pick = {
                    let card = card.downgrade();
                    move |branch: String, window: &mut Window, cx: &mut App| {
                        popover
                            .update(cx, |popover, cx| popover.dismiss(window, cx))
                            .ok();
                        card.update(cx, |card, cx| card.switch(branch, cx)).ok();
                    }
                };
                card.read(cx).branch_menu(pick, cx)
            })
            .into_any_element()
    }

    /// The filter and the branches it leaves. `pick` switches to one.
    fn branch_menu(
        &self,
        pick: impl Fn(String, &mut Window, &mut App) + Clone + 'static,
        cx: &App,
    ) -> AnyElement {
        let p = theme::palette(cx);
        let current = self
            .repository(cx)
            .and_then(|status| status.repository.head_name.clone());
        let matches = self.matches(cx);
        let note = |text: String| {
            div()
                .px(ui(6.))
                .py(ui(4.))
                .text_size(ui(13.))
                .text_color(p.chrome_dim)
                .child(text)
                .into_any_element()
        };
        let body: Vec<AnyElement> = match &self.branches {
            None => vec![note("Reading branches…".into())],
            Some(Err(error)) => vec![note(error.clone())],
            Some(Ok(_)) if matches.is_empty() => vec![note("No branch matches.".into())],
            Some(Ok(branches)) => {
                let mut rows: Vec<AnyElement> = matches
                    .iter()
                    .enumerate()
                    .map(|(index, name)| {
                        let pick = pick.clone();
                        let branch = name.clone();
                        Button::new(("chat-details-branch-item", index))
                            .ghost()
                            .xsmall()
                            .w_full()
                            .h_auto()
                            .px(ui(6.))
                            .py(ui(3.))
                            .justify_start()
                            .text_size(ui(13.5))
                            .accessibility_label(name.clone())
                            // A label of its own, since Kit centres `label`.
                            .child(div().w_full().min_w_0().truncate().child(name.clone()))
                            .selected(current.as_deref() == Some(name.as_str()))
                            .on_click(move |_, window, cx| pick(branch.clone(), window, cx))
                            .into_any_element()
                    })
                    .collect();
                if branches.truncated {
                    rows.push(note("Only the 500 most recent are listed.".into()));
                }
                rows
            }
        };
        let first = matches.first().cloned();
        v_flex()
            .gap(ui(4.))
            .child(
                div()
                    // Enter takes the first branch left.
                    .capture_action(move |_: &Enter, window, cx| {
                        if let Some(branch) = first.clone() {
                            cx.stop_propagation();
                            pick(branch, window, cx);
                        }
                    })
                    .child(
                        Input::new(&self.filter)
                            .id("chat-details-branch-filter")
                            .aria_label("Filter branches")
                            .xsmall(),
                    ),
            )
            .child(
                v_flex()
                    .id("chat-details-branch-list")
                    .test_support()
                    .role(Role::List)
                    .aria_label("Branches")
                    .max_h(ui(LIST_HEIGHT))
                    .overflow_y_scroll()
                    .children(body),
            )
            .into_any_element()
    }

    fn card(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = theme::palette(cx);
        let narrow = !self.shown.wide;
        let repository = self.repository(cx);
        let host = group(false, &p).child(row(IconName::Server, self.host(cx).into(), None, &p));
        let host = if self.shown.path.is_empty() {
            host
        } else {
            let root = repository
                .as_ref()
                .map(|status| status.repository.worktree_root.as_str());
            let label = folder_label(&self.shown.path, self.home(cx).as_deref(), root);
            let path = SharedString::from(self.shown.path.clone());
            host.child(
                h_flex()
                    .id("chat-details-folder")
                    .test_support()
                    .role(Role::Label)
                    .aria_label(SharedString::from(format!("Folder: {path}")))
                    .tooltip(move |window, cx| Tooltip::new(path.clone()).build(window, cx))
                    .gap(ui(8.))
                    .min_w_0()
                    .child(mark(IconName::Folder, &p))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis_start()
                            .text_color(p.chrome_ink_strong)
                            .child(label),
                    ),
            )
        };
        let git = repository.map(|status| {
            let commit = action("chat-details-commit", "Commit…".into(), &p)
                .child(row(
                    IconName::GitCommitHorizontal,
                    "Commit…".into(),
                    None,
                    &p,
                ))
                .on_click(cx.listener(move |this, _, window, cx| {
                    git::start_commit(window, cx);
                    if narrow {
                        this.close(cx);
                    }
                }));
            let (uncommitted, uncommitted_failed) = self.summary(SummaryBase::Head, cx);
            let (branch, branch_failed) = self.summary(SummaryBase::DefaultBranch, cx);
            let uncommitted = changes_row(
                Changes::Uncommitted,
                uncommitted.as_deref(),
                uncommitted_failed,
                &p,
            )
            .map(|row| self.opens(row, Scope::Uncommitted, narrow, cx));
            let branch = changes_row(Changes::Branch, branch.as_deref(), branch_failed, &p)
                .map(|row| self.opens(row, Scope::Branch, narrow, cx));
            group(true, &p)
                .gap(ui(3.))
                .child(self.branch_row(&status, &p, cx))
                .when_some(self.error.clone(), |group, error| {
                    group.child(
                        div()
                            .id("chat-details-branch-error")
                            .test_support()
                            .role(Role::Alert)
                            .aria_label(SharedString::from(error.clone()))
                            .pl(ui(26.))
                            .pr(ui(4.))
                            .text_size(ui(12.5))
                            .text_color(p.danger_ink)
                            .line_clamp(3)
                            .text_ellipsis()
                            .child(error),
                    )
                })
                .child(commit)
                .children(uncommitted)
                .children(branch)
        });
        let context = self.shown.usage.map(|usage| {
            group(true, &p).child(
                h_flex()
                    .id("chat-details-context")
                    .test_support()
                    .role(Role::Meter)
                    .aria_label(SharedString::from(format!(
                        "Context used: {}",
                        usage_text(&usage)
                    )))
                    .gap(ui(8.))
                    .child(
                        div()
                            .flex_none()
                            .text_size(ui(12.))
                            .text_color(p.chrome_dim)
                            .child("Context"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h(ui(4.))
                            .rounded(ui(2.))
                            .bg(p.chrome_hairline)
                            .overflow_hidden()
                            .child(div().h_full().w(gpui_kit::relative(share(&usage))).bg(
                                if share(&usage) >= 0.9 {
                                    p.warn
                                } else {
                                    p.chrome_dim
                                },
                            )),
                    )
                    .child(extra(usage_text(&usage).into(), &p)),
            )
        });
        v_flex()
            .id("chat-details-card")
            .test_support()
            .role(Role::Group)
            .aria_label("Session details")
            .w(ui(WIDTH))
            .border_1()
            .border_color(p.chrome_border)
            .rounded(ui(10.))
            .bg(p.chrome_bg)
            .shadow(toast::shadow(&p))
            .text_size(ui(13.5))
            .line_height(gpui_kit::relative(1.45))
            .text_color(p.chrome_ink)
            // Nothing under the card takes a press meant for it.
            .occlude()
            .child(host)
            .children(git)
            .children(context)
            .into_any_element()
    }
}

impl Render for Card {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.follow_repository(cx);
        if self.shown.wide {
            self.open = false;
            return self.card(cx);
        }
        let p = theme::palette(cx);
        let open = self.open;
        let toggle = Button::new("chat-details-toggle")
            .label("Details")
            .accessibility_label(if open {
                "Hide session details"
            } else {
                "Show session details"
            })
            .xsmall()
            .h_auto()
            .px(ui(9.))
            .py(ui(3.))
            .border_1()
            .text_size(ui(13.))
            .map(|button| {
                if open {
                    button
                        .ghost()
                        .border_color(p.accent)
                        // Opaque: it may lie over the conversation.
                        .bg(p.chrome_bg)
                        .text_color(p.link_ink)
                } else {
                    button
                        .ghost()
                        .border_color(p.chrome_border)
                        .bg(p.chrome_bg)
                        .text_color(p.chrome_dim)
                }
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.open = !this.open;
                cx.notify();
            }));
        if !open {
            // Nothing of the card is drawn, its branch menu included.
            self.menu = false;
        }
        let card = open.then(|| self.card(cx));
        v_flex()
            .id("chat-details")
            .items_end()
            .gap(ui(6.))
            .child(toggle)
            .children(card)
            .when(open, |this| {
                // A press anywhere else puts the card away, unless it is in
                // the branch menu, which lies outside it.
                this.on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    if !this.menu {
                        this.close(cx);
                    }
                }))
            })
            .into_any_element()
    }
}

/// `.dcard .g`: a group of rows, ruled off from the one above.
fn group(ruled: bool, p: &Palette) -> gpui_kit::Div {
    v_flex()
        .px(ui(10.))
        .py(ui(8.))
        .gap(ui(7.))
        .min_w_0()
        .when(ruled, |group| {
            group.border_t_1().border_color(p.chrome_hairline)
        })
}

/// `.gl`: the icon at the start of a row.
fn mark(name: IconName, p: &Palette) -> impl IntoElement {
    div()
        .flex_none()
        .w(ui(14.))
        .flex()
        .justify_center()
        .child(icon(name, 14., p.chrome_faint))
}

/// `.x`: what trails a row. Read, so in the dim ink, not the faint (S8, C1).
fn extra(text: SharedString, p: &Palette) -> gpui_kit::Div {
    div()
        .flex_none()
        .whitespace_nowrap()
        .text_size(ui(12.))
        .text_color(p.chrome_dim)
        .child(text)
}

/// `.l`: a mark, the value, and what trails it.
fn row(
    name: IconName,
    value: SharedString,
    trailing: Option<SharedString>,
    p: &Palette,
) -> gpui_kit::Div {
    h_flex()
        .w_full()
        .gap(ui(8.))
        .min_w_0()
        .child(mark(name, p))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(p.chrome_ink_strong)
                .child(value),
        )
        .children(trailing.map(|text| extra(text, p)))
}

/// The two changes rows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Changes {
    /// Against HEAD.
    Uncommitted,
    /// Against the merge base with the default branch.
    Branch,
}

/// What trails a changes row.
#[derive(Debug, PartialEq, Eq)]
enum Count {
    /// Not read yet: `…`.
    Reading,
    /// Nothing changed: `none`, the row dim.
    None,
    Lines {
        added: u64,
        removed: u64,
    },
    /// The read failed; the Git panel says why.
    Unknown,
}

/// What a changes row shows: its trailing count and the default branch it
/// names (`origin/main`), or `None` when the row is left out.
fn changes(
    kind: Changes,
    summary: Option<&BranchSummary>,
    failed: bool,
) -> Option<(Count, Option<String>)> {
    let Some(summary) = summary else {
        return Some((
            if failed {
                Count::Unknown
            } else {
                Count::Reading
            },
            None,
        ));
    };
    let base = summary.base_ref.clone();
    // No default branch, or nothing on the branch: the second row is left out
    // rather than drawn empty.
    if kind == Changes::Branch && (base.is_none() || summary.total_file_count == 0) {
        return None;
    }
    let count = if summary.total_file_count == 0 {
        Count::None
    } else {
        Count::Lines {
            added: summary.added,
            removed: summary.removed,
        }
    };
    Some((count, base.filter(|_| kind == Changes::Branch)))
}

/// `Uncommitted  +12 −3`, or `Branch vs main  +182 −41`. `None` when the row
/// is left out.
fn changes_row(
    kind: Changes,
    summary: Option<&BranchSummary>,
    failed: bool,
    p: &Palette,
) -> Option<Button> {
    let (count, base) = changes(kind, summary, failed)?;
    // `origin/main` reads as `main`: the card has no room for both.
    let vs = base.as_deref().map(|base| {
        format!(
            "vs {}",
            base.split_once('/').map_or(base, |(_, branch)| branch)
        )
    });
    let (id, word, tip) = match kind {
        Changes::Uncommitted => (
            "chat-details-uncommitted",
            "Uncommitted",
            "Staged, unstaged and untracked files against the last commit.".to_owned(),
        ),
        Changes::Branch => (
            "chat-details-branch-changes",
            "Branch",
            format!(
                "Everything on this branch since it split from {}, uncommitted and untracked \
                 included.",
                base.as_deref().unwrap_or("the default branch")
            ),
        ),
    };
    let count_text = match &count {
        Count::Reading => "\u{2026}".to_owned(),
        Count::None => "none".to_owned(),
        Count::Lines { added, removed } => format!("+{added} \u{2212}{removed}"),
        Count::Unknown => String::new(),
    };
    let name = [Some(word.to_owned()), vs.clone(), Some(count_text.clone())]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let dim = count == Count::None;
    let trailing = match count {
        Count::Lines { added, removed } => Some(
            h_flex()
                .flex_none()
                .gap(ui(4.))
                .text_size(ui(12.))
                .child(div().text_color(p.ok_ink).child(format!("+{added}")))
                .child(
                    div()
                        .text_color(p.danger_ink)
                        .child(format!("\u{2212}{removed}")),
                )
                .into_any_element(),
        ),
        Count::Unknown => None,
        _ => Some(extra(count_text.into(), p).into_any_element()),
    };
    let row = h_flex()
        .w_full()
        .gap(ui(8.))
        .min_w_0()
        .child(mark(IconName::FileDiff, p))
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap(ui(4.))
                .overflow_hidden()
                .whitespace_nowrap()
                // A row with nothing to show is a step quieter, still in an
                // ink that reads (S8, C1).
                .child(
                    div()
                        .flex_none()
                        .text_color(if dim {
                            p.chrome_dim
                        } else {
                            p.chrome_ink_strong
                        })
                        .child(word),
                )
                .children(vs.map(|vs| {
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(p.chrome_dim)
                        .child(vs)
                })),
        )
        .children(trailing);
    Some(action(id, name, p).tooltip(tip).child(row))
}

/// A row that acts: Kit's quiet button, laid out as the card's rows are.
fn action(id: &'static str, name: String, p: &Palette) -> Button {
    Button::new(id)
        .ghost()
        .accessibility_label(name)
        .w_full()
        .h_auto()
        // The row's text lines up with the rows that do not act.
        .mx(ui(-4.))
        .px(ui(4.))
        .py(ui(2.))
        .justify_start()
        .text_size(ui(13.5))
        .text_color(p.chrome_ink_strong)
}

/// Why Git did not switch, once: Git says it on stderr, and the host may
/// repeat it as the command's error.
fn refusal(result: &CommandResult) -> String {
    [&result.stderr, &result.error, &result.stdout]
        .into_iter()
        // One paragraph, so the card can cut it at three lines.
        .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
        .find(|text| !text.is_empty())
        .unwrap_or_else(|| model::result_message(result, "Switched."))
}

/// How much of the window is used, from 0 to 1.
fn share(usage: &ChatUsage) -> f32 {
    if usage.window_tokens == 0 {
        0.
    } else {
        (usage.used_tokens as f32 / usage.window_tokens as f32).clamp(0., 1.)
    }
}

fn usage_text(usage: &ChatUsage) -> String {
    format!(
        "{} / {}",
        tokens(usage.used_tokens),
        tokens(usage.window_tokens)
    )
}

/// Whether the folder `path` lies in the worktree at `root`.
fn within(path: &str, root: &str) -> bool {
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return false;
    }
    path.strip_prefix(root)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The chat's folder as the card names it, never absolute where something
/// shorter says it: inside the repository at `repo_root`, the repository's
/// folder and the path within it (`muxflow/apps/host`); else relative to
/// `home` (`~/notes`). A folder outside both stays as it is.
fn folder_label(path: &str, home: Option<&str>, repo_root: Option<&str>) -> String {
    if let Some(root) = repo_root.filter(|root| within(path, root)) {
        let root = root.trim_end_matches('/');
        let name = root.rsplit('/').next().unwrap_or(root);
        return format!("{name}{}", &path[root.len()..]);
    }
    abbreviate_home(Some(path.to_owned()), home).unwrap_or_default()
}

/// The names that contain `filter`, ignoring case, in their order.
fn filtered(names: &[String], filter: &str) -> Vec<String> {
    let filter = filter.trim().to_lowercase();
    names
        .iter()
        .filter(|name| name.to_lowercase().contains(&filter))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_is_in_a_worktree_at_or_under_its_root() {
        assert!(within("/w/repo", "/w/repo"));
        assert!(within("/w/repo/src", "/w/repo/"));
        assert!(!within("/w/repository", "/w/repo"));
        assert!(!within("/w", "/w/repo"));
        assert!(!within("/w/repo", ""));
    }

    #[test]
    fn a_folder_is_named_from_its_repository_else_from_home() {
        let home = Some("/home/operator");
        let repo = Some("/home/operator/dev/muxflow");
        // Home itself, below it, and outside it.
        assert_eq!(folder_label("/home/operator", home, None), "~");
        assert_eq!(
            folder_label("/home/operator/notes/drafts", home, None),
            "~/notes/drafts"
        );
        assert_eq!(folder_label("/srv/app", home, None), "/srv/app");
        assert_eq!(folder_label("/srv/app", None, None), "/srv/app");
        // The repository's root, and inside it.
        assert_eq!(
            folder_label("/home/operator/dev/muxflow", home, repo),
            "muxflow"
        );
        assert_eq!(
            folder_label("/home/operator/dev/muxflow/apps/desktop-gpui", home, repo),
            "muxflow/apps/desktop-gpui"
        );
        // A worktree goes by its own folder.
        assert_eq!(
            folder_label(
                "/home/operator/dev/muxflow-chat/apps",
                home,
                Some("/home/operator/dev/muxflow-chat/")
            ),
            "muxflow-chat/apps"
        );
        // A repository the folder is not in names nothing.
        assert_eq!(
            folder_label("/home/operator/dev/muxflow-old", home, repo),
            "~/dev/muxflow-old"
        );
        assert_eq!(
            folder_label("/srv/repo/src", home, Some("/srv/repo")),
            "repo/src"
        );
    }

    fn summary(base: Option<&str>, files: u64, added: u64, removed: u64) -> BranchSummary {
        BranchSummary {
            base_ref: base.map(str::to_owned),
            total_file_count: files,
            added,
            removed,
            ..BranchSummary::default()
        }
    }

    #[test]
    fn the_changes_rows_say_none_while_reading_and_drop_an_empty_branch() {
        let lines = |added, removed| Count::Lines { added, removed };
        let head = summary(Some("HEAD"), 2, 12, 3);
        assert_eq!(
            changes(Changes::Uncommitted, Some(&head), false),
            Some((lines(12, 3), None))
        );
        let clean = summary(Some("HEAD"), 0, 0, 0);
        assert_eq!(
            changes(Changes::Uncommitted, Some(&clean), false),
            Some((Count::None, None))
        );
        // A binary file changes no line but is still a change.
        let binary = summary(Some("HEAD"), 1, 0, 0);
        assert_eq!(
            changes(Changes::Uncommitted, Some(&binary), false),
            Some((lines(0, 0), None))
        );
        for kind in [Changes::Uncommitted, Changes::Branch] {
            assert_eq!(changes(kind, None, false), Some((Count::Reading, None)));
            assert_eq!(changes(kind, None, true), Some((Count::Unknown, None)));
        }
        let branch = summary(Some("origin/main"), 4, 182, 41);
        assert_eq!(
            changes(Changes::Branch, Some(&branch), false),
            Some((lines(182, 41), Some("origin/main".into())))
        );
        // No default branch, or nothing on the branch: no row.
        assert_eq!(
            changes(Changes::Branch, Some(&summary(None, 0, 0, 0)), false),
            None
        );
        let empty = summary(Some("origin/main"), 0, 0, 0);
        assert_eq!(changes(Changes::Branch, Some(&empty), false), None);
    }

    #[test]
    fn the_filter_keeps_the_host_s_order_and_ignores_case() {
        let names = ["main", "gpui/chat", "feature/chat"].map(String::from);
        assert_eq!(filtered(&names, "CHAT"), ["gpui/chat", "feature/chat"]);
        assert_eq!(filtered(&names, " "), names);
        assert!(filtered(&names, "nothing").is_empty());
    }

    #[test]
    fn the_meter_reads_as_the_composer_s() {
        let usage = ChatUsage {
            used_tokens: 209_000,
            window_tokens: 1_000_000,
        };
        assert_eq!(usage_text(&usage), "209k / 1M");
        assert!((share(&usage) - 0.209).abs() < 1e-6);
        assert_eq!(
            share(&ChatUsage {
                used_tokens: 5,
                window_tokens: 0
            }),
            0.
        );
    }
}
