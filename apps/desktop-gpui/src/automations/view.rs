//! The Automations panel and its editor dialog, with the Tauri copy. Port of
//! the view half of `AutomationsPanel.tsx` and `AutomationEditor.tsx`.

use std::cell::Cell;

use gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Render, Rgba, Role, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window,
    base::{RootPlugin, Selectable, TestSupportExt},
    component::{
        Disableable, Sizable, WindowExt,
        button::{Button, ButtonVariants},
        dialog::DialogFooter,
        h_flex,
        input::{Input, InputEvent, InputState, Textarea, TextareaState},
        menu::DropdownMenu,
        switch::Switch,
        v_flex,
    },
    div,
    prelude::FluentBuilder,
    relative,
};

use crate::{
    context_menu::{Entry, Item, fill},
    fonts,
    theme::{
        self, Palette,
        metrics::{RADIUS_CONTROL, TYPE_LABEL, TYPE_META, TYPE_ROW, TYPE_SECONDARY, ui},
    },
};

use super::{
    Automations,
    model::{
        AGENTS, AutomationRun, AutomationSummary, Missing, age, agent_text, command_preview,
        command_text, describe_duration, describe_minute, describe_schedule, is_home, overlap_text,
        preset_command, run_chip, status_text, unfinished,
    },
    panel::AutomationsPanel,
};

fn chip(status: &str, text: String, palette: &Palette) -> impl IntoElement {
    let wash = |color: Rgba| Rgba { a: 0.14, ..color };
    let (ink, background) = match status {
        "working" | "launched" => (palette.state_working, wash(palette.state_working)),
        "done" => (palette.state_done, wash(palette.state_done)),
        "needs_attention" | "failed" | "invalid" => (palette.danger_ink, palette.danger_wash),
        _ => (palette.chrome_dim, palette.chrome_raised),
    };
    div()
        .flex_none()
        .px(ui(6.))
        .py(ui(1.))
        .rounded(ui(4.))
        .bg(background)
        .text_color(ink)
        .text_size(ui(TYPE_META))
        .child(text)
}

fn small_button(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Button {
    Button::new(gpui_kit::ElementId::Name(id.into()))
        .outline()
        .xsmall()
        .compact()
        .label(label)
}

fn code(text: impl Into<SharedString>, palette: &Palette) -> impl IntoElement {
    div()
        .font_family(fonts::MONO)
        .text_size(ui(TYPE_META))
        .text_color(palette.chrome_ink)
        .child(text.into())
}

fn quiet(text: impl Into<SharedString>, palette: &Palette) -> impl IntoElement {
    div()
        .px(ui(12.))
        .py(ui(10.))
        .text_size(ui(TYPE_SECONDARY))
        .text_color(palette.chrome_faint)
        .child(text.into())
}

fn alert(text: impl Into<SharedString>, palette: &Palette) -> impl IntoElement {
    div()
        .id("automations-error")
        .role(Role::Alert)
        .mx(ui(8.))
        .my(ui(6.))
        .px(ui(8.))
        .py(ui(6.))
        .rounded(ui(RADIUS_CONTROL))
        .bg(palette.danger_wash)
        .text_color(palette.danger_ink)
        .text_size(ui(TYPE_SECONDARY))
        .child(text.into())
}

fn centered(children: Vec<AnyElement>, palette: &Palette) -> impl IntoElement {
    v_flex()
        .items_center()
        .gap(ui(8.))
        .px(ui(16.))
        .py(ui(24.))
        .text_center()
        .text_size(ui(TYPE_SECONDARY))
        .line_height(relative(1.5))
        .text_color(palette.chrome_dim)
        .children(children)
}

fn strong(text: impl Into<SharedString>, palette: &Palette) -> AnyElement {
    div()
        .text_size(ui(TYPE_ROW))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(palette.chrome_ink_strong)
        .child(text.into())
        .into_any_element()
}

impl Render for AutomationsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx);
        let section = v_flex()
            .id("automations-panel")
            .test_support()
            .role(Role::Region)
            .aria_label("Automations")
            .size_full()
            .min_h_0();
        let Some(host) = self.host().cloned() else {
            return section.child(quiet("No host is shown.", &palette));
        };
        let listing = self.listing().cloned();
        let selected = self.selected_summary().cloned();
        let read_only = self.read_only();
        let empty = listing
            .as_ref()
            .is_some_and(|listing| listing.automations.is_empty() && listing.file_error.is_empty());

        let header = selected.is_none().then(|| {
            let chooser = if self.hosts().len() > 1 {
                let hosts: Vec<_> = self
                    .hosts()
                    .iter()
                    .map(|candidate| (candidate.profile_id.clone(), candidate.label.clone()))
                    .collect();
                let this = cx.entity().downgrade();
                let current = host.profile_id.clone();
                Button::new("automations-host")
                    .ghost()
                    .xsmall()
                    .label(host.label.clone())
                    .accessibility_label(format!("Host: {}", host.label))
                    .dropdown_menu(move |menu, _, _| {
                        let entries: Vec<Entry> = hosts
                            .iter()
                            .map(|(id, label)| {
                                let checked = *id == current;
                                let (id, this) = (id.clone(), this.clone());
                                Item::new(label.clone(), move |_, cx| {
                                    this.update(cx, |panel, cx| panel.choose(id.clone(), cx))
                                        .ok();
                                })
                                .checked(checked)
                                .into()
                            })
                            .collect();
                        fill(menu, &entries)
                    })
                    .into_any_element()
            } else {
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(ui(TYPE_SECONDARY))
                    .text_color(palette.chrome_dim)
                    .child(host.label.clone())
                    .into_any_element()
            };
            h_flex()
                .flex_none()
                .items_center()
                .justify_between()
                .gap(ui(8.))
                .px(ui(10.))
                .py(ui(6.))
                .child(chooser)
                .when(!empty, |this| {
                    this.child(
                        Button::new("automations-new")
                            .primary()
                            .xsmall()
                            .label("New")
                            .disabled(listing.is_none() || read_only)
                            .on_click(cx.listener(|this, _, _, cx| this.open_editor(None, cx))),
                    )
                })
        });

        let body: AnyElement = if host.client_id.is_none() {
            centered(
                vec![
                    strong(format!("{} is disconnected", host.label), &palette),
                    div()
                        .child("The helper keeps running automations while the desktop is closed. They stop if the host reboots, until the next time you connect.")
                        .into_any_element(),
                ],
                &palette,
            )
            .into_any_element()
        } else {
            let mut parts: Vec<AnyElement> = Vec::new();
            if let Some(error) = &self.error {
                parts.push(alert(error.clone(), &palette).into_any_element());
            }
            if let Some(listing) = listing
                .as_ref()
                .filter(|listing| !listing.file_error.is_empty())
            {
                parts.push(
                    v_flex()
                        .id("automations-banner")
                        .role(Role::Alert)
                        .gap(ui(2.))
                        .mx(ui(8.))
                        .my(ui(6.))
                        .px(ui(8.))
                        .py(ui(6.))
                        .rounded(ui(RADIUS_CONTROL))
                        .bg(palette.danger_wash)
                        .text_size(ui(TYPE_SECONDARY))
                        .text_color(palette.chrome_ink)
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("The file can't be read"))
                        .child(listing.file_error.clone())
                        .child("The last version that loaded is still running. Fix the file on the host to edit here.")
                        .into_any_element(),
                );
            }
            match (&listing, &selected) {
                (None, _) => {
                    if self.error.is_none() {
                        parts.push(quiet("Loading…", &palette).into_any_element());
                    }
                }
                (Some(_), Some(summary)) => parts.push(self.detail(summary, read_only, cx)),
                (Some(listing), None) if empty => parts.push(
                    centered(
                        vec![
                            strong(format!("No automations on {}", host.label), &palette),
                            div()
                                .child("Run an agent on a cron schedule. Each run opens a new tab in the automation workspace.")
                                .into_any_element(),
                            Button::new("automations-new-first")
                                .primary()
                                .xsmall()
                                .label("New automation")
                                .on_click(cx.listener(|this, _, _, cx| this.open_editor(None, cx)))
                                .into_any_element(),
                            code(format!("{}/automations.json", listing.state_dir), &palette).into_any_element(),
                        ],
                        &palette,
                    )
                    .into_any_element(),
                ),
                (Some(listing), None) => parts.push(
                    v_flex()
                        .id("automations-list")
                        .role(Role::List)
                        .aria_label("Automations")
                        .gap(ui(2.))
                        .px(ui(6.))
                        .pb(ui(10.))
                        .children(
                            listing
                                .automations
                                .iter()
                                .enumerate()
                                .map(|(index, summary)| self.row(index, summary, read_only, cx)),
                        )
                        .into_any_element(),
                ),
            }
            v_flex()
                .id("automations-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(parts)
                .into_any_element()
        };
        section.children(header).child(body)
    }
}

impl AutomationsPanel {
    fn row(
        &self,
        index: usize,
        summary: &AutomationSummary,
        read_only: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = theme::palette(cx);
        let (now, today) = self.now();
        let definition = &summary.definition;
        // A run still going says so first, over a newer skip and over "Paused".
        let running = summary.recent_runs.iter().find(|run| unfinished(run));
        let last = running.or(summary.recent_runs.first());
        let runnable = summary.definition_error.is_empty();
        let paused = !(definition.enabled && runnable);
        let starting = self.starting(&definition.id);
        let status: Option<AnyElement> = if !runnable {
            Some(chip("invalid", "invalid".into(), &palette).into_any_element())
        } else if let Some(run) = running {
            Some(chip(&run.status, run_chip(run, now, today), &palette).into_any_element())
        } else if !definition.enabled {
            Some(chip("", "Paused".into(), &palette).into_any_element())
        } else {
            last.map(|run| {
                chip(&run.status, run_chip(run, now, today), &palette).into_any_element()
            })
        };
        let schedule = if runnable {
            let next = if summary.next_run.is_empty() {
                String::new()
            } else {
                format!(" · next {}", describe_minute(&summary.next_run, today))
            };
            div()
                .truncate()
                .child(format!("{}{next}", describe_schedule(&definition.schedule)))
        } else {
            div()
                .text_color(palette.danger_ink)
                .child(summary.definition_error.clone())
        };
        let name = match (definition.name.as_str(), definition.id.as_str()) {
            ("", "") => "Unnamed".to_owned(),
            ("", id) => id.to_owned(),
            (name, _) => name.to_owned(),
        };
        let (open_id, run_id, toggle_id) = (
            definition.id.clone(),
            definition.id.clone(),
            definition.id.clone(),
        );
        let enabled = definition.enabled;
        let switch_label = if enabled {
            format!("Pause {}", definition.name)
        } else {
            format!("Resume {}", definition.name)
        };
        h_flex()
            .id(SharedString::from(format!(
                "automation-{index}-{}",
                definition.id
            )))
            .role(Role::ListItem)
            .items_start()
            .gap(ui(8.))
            .p(ui(8.))
            .rounded(ui(6.))
            .hover(|style| style.bg(palette.chrome_hover))
            .child(
                v_flex()
                    .id(SharedString::from(format!("automation-open-{index}")))
                    .test_support()
                    .role(Role::Button)
                    .aria_label(name.clone())
                    .flex_1()
                    .min_w_0()
                    .gap(ui(4.))
                    .cursor_pointer()
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.select(Some(open_id.clone()), cx)),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(ui(TYPE_ROW))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if paused {
                                palette.chrome_dim
                            } else {
                                palette.chrome_ink
                            })
                            .child(name),
                    )
                    .child(
                        h_flex()
                            .flex_wrap()
                            .items_center()
                            .gap_x(ui(10.))
                            .gap_y(ui(4.))
                            .text_size(ui(TYPE_META))
                            .text_color(palette.chrome_dim)
                            .children(status)
                            .child(schedule)
                            .child(
                                div()
                                    .truncate()
                                    .font_family(fonts::MONO)
                                    .text_color(palette.chrome_faint)
                                    .child(agent_text(summary).to_owned()),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap(ui(8.))
                    .child(
                        small_button(
                            format!("automation-run-{index}"),
                            if starting { "Starting…" } else { "Run now" },
                        )
                        .disabled(!runnable || starting)
                        .on_click(cx.listener(move |this, _, _, cx| this.run_now(&run_id, cx))),
                    )
                    .child(
                        Switch::new(SharedString::from(format!("automation-switch-{index}")))
                            .checked(enabled)
                            .accessibility_label(switch_label)
                            .disabled(!runnable || read_only)
                            .on_click(cx.listener(move |this, _: &bool, _, cx| {
                                this.toggle(&toggle_id, cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    fn detail(
        &self,
        summary: &AutomationSummary,
        read_only: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = theme::palette(cx);
        let (now, today) = self.now();
        let definition = &summary.definition;
        let starting = self.starting(&definition.id);
        let next = if summary.next_run.is_empty() {
            String::new()
        } else {
            format!(" · next {}", describe_minute(&summary.next_run, today))
        };
        let fact = |term: &'static str, value: AnyElement| {
            h_flex()
                .items_start()
                .gap(ui(8.))
                .child(
                    div()
                        .w(ui(64.))
                        .flex_none()
                        .text_size(ui(TYPE_SECONDARY))
                        .text_color(palette.chrome_faint)
                        .child(term),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(ui(TYPE_SECONDARY))
                        .child(value),
                )
        };
        let (edit_id, run_id) = (definition.id.clone(), definition.id.clone());
        let runs: AnyElement = if summary.recent_runs.is_empty() {
            quiet("No runs yet.", &palette).into_any_element()
        } else {
            v_flex()
                .id("automation-runs")
                .role(Role::List)
                .aria_label("Runs")
                .gap(ui(4.))
                .children(
                    summary
                        .recent_runs
                        .iter()
                        .map(|run| self.run_line(run, now, today, cx)),
                )
                .into_any_element()
        };
        v_flex()
            .id("automation-detail")
            .gap(ui(10.))
            .px(ui(12.))
            .pt(ui(8.))
            .pb(ui(12.))
            .child(
                h_flex()
                    .items_center()
                    .gap(ui(8.))
                    .text_size(ui(TYPE_SECONDARY))
                    .child(
                        Button::new("automation-back")
                            .ghost()
                            .xsmall()
                            .label("‹ All")
                            .on_click(cx.listener(|this, _, _, cx| this.select(None, cx))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(if definition.name.is_empty() {
                                definition.id.clone()
                            } else {
                                definition.name.clone()
                            }),
                    )
                    .child(
                        small_button("automation-edit", "Edit")
                            .disabled(read_only)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_editor(Some(&edit_id), cx)
                            })),
                    )
                    .child(
                        small_button(
                            "automation-detail-run",
                            if starting { "Starting…" } else { "Run now" },
                        )
                        .disabled(!summary.definition_error.is_empty() || starting)
                        .on_click(cx.listener(move |this, _, _, cx| this.run_now(&run_id, cx))),
                    ),
            )
            .when(!summary.definition_error.is_empty(), |this| {
                this.child(alert(summary.definition_error.clone(), &palette))
            })
            .child(
                v_flex()
                    .gap(ui(4.))
                    .child(fact(
                        "Schedule",
                        h_flex()
                            .flex_wrap()
                            .gap(ui(4.))
                            .child(code(definition.schedule.clone(), &palette))
                            .child(format!(
                                "· {}{next}",
                                describe_schedule(&definition.schedule)
                            ))
                            .into_any_element(),
                    ))
                    .child(fact(
                        "Runs",
                        code(command_text(summary).to_owned(), &palette).into_any_element(),
                    ))
                    .child(fact(
                        "Directory",
                        code(definition.cwd.clone(), &palette).into_any_element(),
                    ))
                    .child(fact(
                        "Tabs",
                        div()
                            .child(format!(
                                "Keep {} · overlap limit {}",
                                definition.keep_tabs,
                                overlap_text(definition.max_runtime_minutes)
                            ))
                            .into_any_element(),
                    )),
            )
            .child(
                div()
                    .text_size(ui(TYPE_SECONDARY))
                    .text_color(palette.chrome_ink)
                    .child(definition.prompt.clone()),
            )
            .child(
                div()
                    .id("automation-runs-heading")
                    .role(Role::Heading)
                    .text_size(ui(TYPE_LABEL))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(palette.chrome_faint)
                    .child("RUNS"),
            )
            .child(runs)
            .into_any_element()
    }

    fn run_line(
        &self,
        run: &AutomationRun,
        now: u64,
        today: chrono::NaiveDate,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = theme::palette(cx);
        let when = if run.scheduled_for.is_empty() {
            "manual".to_owned()
        } else {
            describe_minute(&run.scheduled_for, today)
        };
        let length = if unfinished(run) {
            format!(" · {}", age(run, now))
        } else if run.finished_at > run.started_at
            && run.status != "skipped"
            && run.status != "failed"
        {
            format!(" · {}", describe_duration(run.finished_at - run.started_at))
        } else {
            String::new()
        };
        let message = if run.last_message.is_empty() {
            &run.error
        } else {
            &run.last_message
        };
        let open = run.clone();
        h_flex()
            .id(SharedString::from(format!("automation-run-{}", run.number)))
            .role(Role::ListItem)
            .flex_wrap()
            .items_center()
            .gap(ui(8.))
            .text_size(ui(TYPE_META))
            .text_color(palette.chrome_dim)
            .child(
                div()
                    .font_family(fonts::MONO)
                    .child(format!("#{}", run.number)),
            )
            .child(format!("{when}{length}"))
            .child(chip(
                &run.status,
                status_text(&run.status).to_owned(),
                &palette,
            ))
            .child(if self.openable(run) {
                Button::new(SharedString::from(format!(
                    "automation-run-open-{}",
                    run.number
                )))
                .ghost()
                .xsmall()
                .label("Open tab")
                .on_click(cx.listener(move |this, _, _, cx| this.open_run(&open, cx)))
                .into_any_element()
            } else {
                div()
                    .text_color(palette.chrome_faint)
                    .child(if run.pane_id.is_empty() {
                        "no tab"
                    } else {
                        "tab closed"
                    })
                    .into_any_element()
            })
            .when(!message.is_empty(), |this| {
                this.child(div().w_full().truncate().child(message.clone()))
            })
            .into_any_element()
    }
}

/// The editor's fields, each pushing its value into the panel's draft.
pub struct EditorView {
    panel: Entity<AutomationsPanel>,
    name: Entity<InputState>,
    command: Entity<InputState>,
    preset: Entity<InputState>,
    prompt: Entity<TextareaState>,
    cwd: Entity<InputState>,
    schedule: Entity<InputState>,
    keep_tabs: Entity<InputState>,
    overlap: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl EditorView {
    fn new(panel: Entity<AutomationsPanel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let draft = panel
            .read(cx)
            .editor
            .as_ref()
            .map(|editor| editor.draft.clone())
            .unwrap_or_default();
        let mut subscriptions = Vec::new();
        let mut field =
            |value: String,
             window: &mut Window,
             cx: &mut Context<Self>,
             apply: fn(&mut super::model::AutomationDefinition, String)| {
                let input = cx.new(|cx| InputState::new(window, cx).default_value(value));
                let panel = panel.clone();
                subscriptions.push(cx.subscribe(
                    &input,
                    move |_, input, event: &InputEvent, cx| {
                        if matches!(event, InputEvent::Change) {
                            let value = input.read(cx).value().to_string();
                            panel.update(cx, |panel, cx| {
                                panel.update_draft(|draft| apply(draft, value), cx)
                            });
                        }
                    },
                ));
                input
            };
        let name = field(draft.name.clone(), window, cx, |draft, value| {
            draft.name = value
        });
        let command = field(draft.command.clone(), window, cx, |draft, value| {
            draft.command = value
        });
        let cwd = field(draft.cwd.clone(), window, cx, |draft, value| {
            draft.cwd = value
        });
        let schedule = field(draft.schedule.clone(), window, cx, |draft, value| {
            draft.schedule = value
        });
        // `Number(value)` in Tauri: what is not a number reaches the host as 0,
        // and the host says what is wrong with it.
        let keep_tabs = field(draft.keep_tabs.to_string(), window, cx, |draft, value| {
            draft.keep_tabs = value.trim().parse().unwrap_or(0)
        });
        let overlap = field(
            draft.max_runtime_minutes.to_string(),
            window,
            cx,
            |draft, value| draft.max_runtime_minutes = value.trim().parse().unwrap_or(0),
        );
        let preset = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(preset_command(&draft.agent).unwrap_or_default())
        });
        let prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(5)
                .default_value(draft.prompt.clone())
        });
        let prompt_panel = panel.clone();
        subscriptions.push(
            cx.subscribe(&prompt, move |_, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().to_string();
                    prompt_panel.update(cx, |panel, cx| {
                        panel.update_draft(|draft| draft.prompt = value, cx)
                    });
                }
            }),
        );
        subscriptions.push(cx.observe(&panel, |_, _, cx| cx.notify()));
        Self {
            panel,
            name,
            command,
            preset,
            prompt,
            cwd,
            schedule,
            keep_tabs,
            overlap,
            _subscriptions: subscriptions,
        }
    }

    fn in_field(&self, window: &Window, cx: &App) -> bool {
        [
            &self.name,
            &self.command,
            &self.cwd,
            &self.schedule,
            &self.keep_tabs,
            &self.overlap,
        ]
        .iter()
        .any(|input| input.focus_handle(cx).is_focused(window))
    }

    fn focus_of(&self, field: Missing, cx: &App) -> FocusHandle {
        match field {
            Missing::Name => self.name.focus_handle(cx),
            Missing::Prompt => self.prompt.focus_handle(cx),
            Missing::Cwd => self.cwd.focus_handle(cx),
            Missing::Command => self.command.focus_handle(cx),
        }
    }

    fn save(&mut self, run_after: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.panel.update(cx, |panel, cx| panel.save(run_after, cx));
        // Points the user at the field the message is about.
        let flagged = self
            .panel
            .read(cx)
            .editor
            .as_ref()
            .and_then(|editor| editor.flagged);
        if let Some(field) = flagged.filter(|field| {
            self.panel
                .read(cx)
                .editor
                .as_ref()
                .is_some_and(|editor| editor.invalid(*field))
        }) {
            self.focus_of(field, cx).focus(window, cx);
        }
    }

    fn choose_agent(&mut self, agent: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.panel.update(cx, |panel, cx| {
            panel.update_draft(|draft| draft.agent = agent.to_owned(), cx)
        });
        self.preset.update(cx, |input, cx| {
            input.set_value(preset_command(agent).unwrap_or_default(), window, cx)
        });
    }
}

fn label(text: &'static str, invalid: bool, palette: &Palette) -> impl IntoElement {
    div()
        .text_size(ui(TYPE_SECONDARY))
        .text_color(if invalid {
            palette.danger_ink
        } else {
            palette.chrome_dim
        })
        .child(text)
}

fn hint(text: impl Into<SharedString>, palette: &Palette) -> impl IntoElement {
    div()
        .text_size(ui(TYPE_META))
        .line_height(relative(1.4))
        .text_color(palette.chrome_faint)
        .child(text.into())
}

impl Render for EditorView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx);
        let panel = self.panel.read(cx);
        let Some(editor) = panel.editor.as_ref() else {
            return v_flex().id("automation-editor");
        };
        let host_label = panel
            .host()
            .map(|host| host.label.clone())
            .unwrap_or_default();
        let state_dir = panel
            .listing()
            .map(|listing| listing.state_dir.clone())
            .unwrap_or_default();
        let (_, today) = panel.now();
        let draft = editor.draft.clone();
        let id = panel.editor_id();
        let custom = draft.agent == "custom";
        let preview = editor.preview.clone();
        let schedule_error = preview
            .as_ref()
            .filter(|preview| !preview.schedule_error.is_empty());
        let reading = match schedule_error {
            Some(preview) => preview.schedule_error.clone(),
            None => describe_schedule(&draft.schedule),
        };
        let next = preview
            .as_ref()
            .filter(|preview| preview.schedule_error.is_empty() && !preview.next_runs.is_empty())
            .map(|preview| {
                preview
                    .next_runs
                    .iter()
                    .map(|key| describe_minute(key, today))
                    .collect::<Vec<_>>()
                    .join(" · ")
            });
        let invalid = |field| editor.invalid(field);
        let agent_hint = if custom {
            "Typed exactly as written into an interactive shell, so aliases and functions work. The prompt is added at the end; making the command work is up to you."
        } else if draft.agent == "codex" {
            "Default Codex command: approval requests go to Codex's automatic reviewer, inside its workspace sandbox. The prompt is added at the end."
        } else {
            "Default Claude Code command in auto mode: actions are approved automatically after a safety check. The prompt is added at the end."
        };
        let cwd_hint = if custom {
            format!("On {host_label}. Checked when you save.")
        } else {
            format!(
                "On {host_label}. Checked when you save. Claude Code and Codex ask before working in a folder they don't trust yet, and an automation will wait on that question. Open the agent in this folder once and trust it, or use Run now to check."
            )
        };
        let preview_line = command_preview(
            &super::model::AutomationDefinition {
                id,
                ..draft.clone()
            },
            &state_dir,
        );
        let segments = AGENTS.iter().map(|(agent, title)| {
            let chosen = draft.agent == *agent;
            let agent = *agent;
            Button::new(SharedString::from(format!("automation-agent-{agent}")))
                .xsmall()
                .label(*title)
                .role(Role::RadioButton)
                .selected(chosen)
                .when(chosen, |button| button.primary())
                .when(!chosen, |button| button.outline())
                .on_click(
                    cx.listener(move |this, _, window, cx| this.choose_agent(agent, window, cx)),
                )
        });
        v_flex()
            .id("automation-editor")
            .gap(ui(8.))
            .max_h(ui(520.))
            .overflow_y_scroll()
            .child(label("Name", invalid(Missing::Name), &palette))
            .child(Input::new(&self.name).aria_label("Name"))
            .child(label("Runs", false, &palette))
            .child(
                h_flex()
                    .id("automation-agent")
                    .role(Role::RadioGroup)
                    .aria_label("Runs")
                    .gap(ui(4.))
                    .children(segments),
            )
            .child(label("Command", invalid(Missing::Command), &palette))
            .child(if custom {
                Input::new(&self.command).aria_label("Command").into_any_element()
            } else {
                Input::new(&self.preset).aria_label("Command").disabled(true).into_any_element()
            })
            .child(hint(agent_hint, &palette))
            .child(label("Prompt", invalid(Missing::Prompt), &palette))
            .child(Textarea::new(&self.prompt).aria_label("Prompt"))
            .child(label("Working directory", invalid(Missing::Cwd), &palette))
            .child(Input::new(&self.cwd).aria_label("Working directory"))
            .child(hint(cwd_hint, &palette))
            .when(draft.agent == "claude" && is_home(&draft.cwd), |this| {
                this.child(
                    div()
                        .id("automation-home-warning")
                        .role(Role::Note)
                        .text_size(ui(TYPE_META))
                        .text_color(palette.danger_ink)
                        .child("Claude Code never remembers trust for your home folder, so this automation will stop at the trust question every time. Choose a project folder."),
                )
            })
            .child(label("Schedule (cron)", schedule_error.is_some(), &palette))
            .child(Input::new(&self.schedule).aria_label("Schedule (cron)"))
            .child(
                v_flex()
                    .text_size(ui(TYPE_META))
                    .text_color(if schedule_error.is_some() { palette.danger_ink } else { palette.chrome_faint })
                    .child(
                        // One sentence: the suffix follows the reading inline.
                        h_flex()
                            .flex_wrap()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(reading))
                            .when(schedule_error.is_none(), |this| this.child(", host time")),
                    )
                    .when_some(next, |this, next| this.child(format!("Next: {next}"))),
            )
            .child(
                h_flex()
                    .gap(ui(10.))
                    .child(
                        v_flex()
                            .flex_1()
                            .gap(ui(4.))
                            .child(label("Keep tabs", false, &palette))
                            .child(Input::new(&self.keep_tabs).aria_label("Keep tabs")),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .gap(ui(4.))
                            .child(label("Overlap limit (minutes)", false, &palette))
                            .child(Input::new(&self.overlap).aria_label("Overlap limit (minutes)")),
                    ),
            )
            .child(hint(
                "The newest runs stay open; older tabs close once their run is done. A run is skipped while the last one is still working, for up to the overlap limit.",
                &palette,
            ))
            .child(
                v_flex()
                    .gap(ui(4.))
                    .child(
                        div()
                            .text_size(ui(TYPE_LABEL))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(palette.chrome_faint)
                            .child("WHAT RUNS"),
                    )
                    .child(code(preview_line, &palette)),
            )
    }
}

/// Opens the editor dialog when the panel opens an editor and closes it when
/// the editor goes, as [`crate::agents::view::AgentsSurface`] does.
pub struct AutomationsSurface {
    panel: Option<Entity<AutomationsPanel>>,
    open: bool,
    _subscription: Option<Subscription>,
}

impl RootPlugin for AutomationsSurface {}

impl Render for AutomationsSurface {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl AutomationsSurface {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let panel = cx
            .try_global::<Automations>()
            .map(|automations| automations.panel.clone());
        let subscription = panel
            .as_ref()
            .map(|panel| cx.observe_in(panel, window, |this, _, window, cx| this.sync(window, cx)));
        Self {
            panel,
            open: false,
            _subscription: subscription,
        }
    }

    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.panel.clone() else {
            return;
        };
        let editing = panel.read(cx).editor.is_some();
        if editing == self.open {
            // The footer is built from the panel.
            if editing {
                window.refresh();
            }
            return;
        }
        self.open = editing;
        if !editing {
            window.close_dialog(cx);
            return;
        }
        let view = cx.new(|cx| EditorView::new(panel.clone(), window, cx));
        // Focused from inside the dialog, so the Kit returns focus to where it
        // was before, not to this field.
        let focus_once = Cell::new(Some(view.read(cx).name.focus_handle(cx)));
        // Escape closes it inside the Kit: `sync` must not close another.
        let surface = cx.weak_entity();
        let closed = move |cx: &mut App| {
            surface.update(cx, |this, _| this.open = false).ok();
        };
        window.open_dialog(cx, move |dialog, window, cx| {
            if let Some(focus) = focus_once.take() {
                focus.focus(window, cx);
            }
            let palette = theme::palette(cx);
            let state = panel.read(cx);
            let Some(editor) = state.editor.as_ref() else {
                return dialog;
            };
            let busy = editor.busy;
            let new = editor.is_new();
            let error = editor.error.clone();
            let confirming = editor.confirming_delete;
            let host_label = state
                .host()
                .map(|host| host.label.clone())
                .unwrap_or_default();
            let (cancel, escaped, remove) = (panel.clone(), panel.clone(), panel.clone());
            let (save_run, save) = (view.clone(), view.clone());
            dialog
                .title(
                    h_flex()
                        .gap(ui(8.))
                        .items_baseline()
                        .child(if new {
                            "New automation"
                        } else {
                            "Edit automation"
                        })
                        .child(
                            div()
                                .text_size(ui(TYPE_SECONDARY))
                                .font_weight(FontWeight::NORMAL)
                                .text_color(palette.chrome_faint)
                                .child(host_label),
                        ),
                )
                .w(ui(560.).to_pixels(window.rem_size()))
                .close_button(false)
                .overlay_closable(true)
                // Enter in a one-line field submits the form, as in Tauri; in the
                // prompt it is a new line, and on a button it presses the button.
                .on_ok({
                    let view = view.clone();
                    move |_, window, cx| {
                        if view.read(cx).in_field(window, cx) {
                            view.update(cx, |view, cx| view.save(false, window, cx));
                        } else {
                            cx.propagate();
                        }
                        false
                    }
                })
                .on_close({
                    let closed = closed.clone();
                    move |_, _, cx| {
                        closed(cx);
                        escaped.update(cx, |panel, cx| panel.close_editor(cx));
                    }
                })
                .child(view.clone())
                .footer(
                    v_flex()
                        .w_full()
                        .gap(ui(8.))
                        .when_some(error, |this, error| this.child(alert(error, &palette)))
                        .child(
                            DialogFooter::new()
                                .when(!new, |this| {
                                    this.child(
                                        Button::new("automation-delete")
                                            .danger()
                                            .label(if confirming {
                                                "Delete for good"
                                            } else {
                                                "Delete"
                                            })
                                            .disabled(busy)
                                            .on_click(move |_, _, cx| {
                                                remove.update(cx, |panel, cx| panel.remove(cx))
                                            }),
                                    )
                                })
                                .child(
                                    Button::new("automation-cancel")
                                        .label("Cancel")
                                        .disabled(busy)
                                        .on_click(move |_, _, cx| {
                                            cancel.update(cx, |panel, cx| panel.close_editor(cx))
                                        }),
                                )
                                .child(
                                    Button::new("automation-save-run")
                                        .label("Save and run now")
                                        .disabled(busy)
                                        .on_click(move |_, window, cx| {
                                            save_run
                                                .update(cx, |view, cx| view.save(true, window, cx))
                                        }),
                                )
                                .child(
                                    Button::new("automation-save")
                                        .primary()
                                        .label("Save")
                                        .disabled(busy)
                                        .on_click(move |_, window, cx| {
                                            save.update(cx, |view, cx| view.save(false, window, cx))
                                        }),
                                ),
                        ),
                )
        });
    }
}
