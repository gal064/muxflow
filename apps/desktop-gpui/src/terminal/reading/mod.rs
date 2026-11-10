//! T3.5: selection and copy, scrollback and history paging, find, and links.
//! Port of the reading half of `TerminalPane.tsx` and `TerminalRenderer.ts`.
//!
//! The selection lives on the grid (`Term::selection`), so output that scrolls
//! the screen carries it along. Everything else this feature knows about a pane
//! is [`PaneReading`]. What is drawn over the grid (the find bar, the
//! scrollbar, the new-output button) is an [`Overlay`] view per pane: a pane
//! cannot be read while it renders, and the overlay renders just after it.
//!
//! The mouse is this feature's while the program does not read it, and on a
//! Shift press when it does (see [`super`]). The scrollbar sits in the strip the
//! client size already leaves free, `CellMetrics::scrollbar_width`.

mod buffer;
mod find;
mod links;
mod scroll;
mod selection;

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionRange, SelectionType};
use alacritty_terminal::term::TermMode;
use desktop_core::app_state::load_app_state;
use gpui_kit::{
    AnyElement, AnyView, App, AppContext, Bounds, ClipboardItem, Context, CursorStyle,
    DispatchPhase, Div, Entity, EntityId, Focusable, Global, InteractiveElement, IntoElement,
    Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels,
    Render, Role, ScrollEvent, SharedString, Stateful, StatefulInteractiveElement, Styled,
    WeakEntity, Window, canvas,
    component::{
        IconName, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Escape, Input, InputEvent, InputState},
    },
    div,
    prelude::FluentBuilder,
    px,
};
use terminal_engine::{Command, Listener, TerminalReadingState};
use terminal_interactions::LinkKind;

use crate::app::Core;
use crate::commands::{self, Commands, dialogs::is_composing, registry::Platform};
use crate::notices;
use crate::theme::{
    self, Palette,
    metrics::{RADIUS_CONTROL, TYPE_SECONDARY, ui},
};

use super::grid::{CellHighlight, Rgb24};
use super::pane::TerminalPane;
use find::Find;
use links::Link;

/// Super+C in the kitty keyboard encoding: how kitty and Ghostty report Cmd+C
/// to a program, which agent composers read as "copy your own selection".
const KITTY_SUPER_C: &[u8] = b"\x1b[99;9u";
/// The shortest a scrollbar thumb gets, so it can always be grabbed.
const MIN_THUMB: f32 = 20.;
const TO_HISTORY: &str = "Returning from terminal history requires a current screen";
const TO_LIVE: &str = "Returning to live terminal output requires a current screen";

pub fn init(cx: &mut App) {
    commands::on_command("terminal.copy", cx, |window, cx| {
        with_target(window, cx, |pane, window, cx| {
            copy(pane, Platform::current(), window, cx)
        });
    });
    commands::on_command("terminal.search", cx, |window, cx| {
        with_target(window, cx, open_find);
    });
    commands::on_command("terminal.scrollBottom", cx, |window, cx| {
        with_target(window, cx, |pane, _, cx| return_to_live(pane, TO_LIVE, cx));
    });
}

#[derive(Clone, Copy, Default)]
struct Colors {
    selection_bg: Rgb24,
    selection_fg: Rgb24,
    found: Rgb24,
    active: Rgb24,
}

/// How a drag that began on the grid grows the selection.
#[derive(Clone, Copy)]
enum Grow {
    Cells,
    /// From a double click: by whole words, keeping the first one.
    Words(Point, Point),
    Lines,
}

enum Drag {
    Select {
        grow: Grow,
        /// A press with the link modifier on a link: a click opens it.
        link: Option<Link>,
    },
    Scrollbar,
}

struct Hover {
    line: Line,
    link: Option<Link>,
}

/// This feature's state on a pane.
#[derive(Default)]
pub struct PaneReading {
    drag: Option<Drag>,
    wheel_carry: f32,
    hover: Option<Hover>,
    find: Option<Find>,
    /// Set each time the overlay renders, which is before the grid paints.
    colors: Cell<Colors>,
    /// The pane's box at its last paint, for the scrollbar.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

/// Every pane's overlay, and the pane commands go to when none has focus.
#[derive(Default)]
struct Panes {
    overlays: HashMap<EntityId, Entity<Overlay>>,
    last_focused: Option<WeakEntity<TerminalPane>>,
}

impl Global for Panes {}

fn target(window: &Window, cx: &App) -> Option<Entity<TerminalPane>> {
    let panes = cx.try_global::<Panes>()?;
    panes
        .overlays
        .values()
        .filter_map(|overlay| overlay.read(cx).pane.upgrade())
        .find(|pane| pane.read(cx).focus_handle(cx).contains_focused(window, cx))
        .or_else(|| panes.last_focused.as_ref()?.upgrade())
}

fn with_target(
    window: &mut Window,
    cx: &mut App,
    run: impl FnOnce(&mut TerminalPane, &mut Window, &mut Context<TerminalPane>),
) {
    if let Some(pane) = target(window, cx) {
        pane.update(cx, |pane, cx| run(pane, window, cx));
    }
}

/// Only what changed is written: every write notifies the palette.
fn set_context(cx: &mut App, has_pane: Option<bool>, has_selection: Option<bool>) {
    if !cx.has_global::<Commands>() {
        return;
    }
    let context = cx.global::<Commands>().context();
    let pane_changed = has_pane.is_some_and(|value| value != context.has_pane);
    let selection_changed =
        has_selection.is_some_and(|value| value != context.has_terminal_selection);
    if pane_changed || selection_changed {
        commands::update_context(cx, |context| {
            context.has_pane = has_pane.unwrap_or(context.has_pane);
            context.has_terminal_selection =
                has_selection.unwrap_or(context.has_terminal_selection);
        });
    }
}

/// `copyOnSelect` and `cleanWrappedCommands`, both on unless turned off.
fn copy_settings(cx: &App) -> (bool, bool) {
    let shell = cx
        .try_global::<Core>()
        .and_then(|core| load_app_state(&core.app_state).ok())
        .map(|state| state.shell);
    let shell = shell.as_ref();
    (
        shell.and_then(|shell| shell.copy_on_select).unwrap_or(true),
        shell
            .and_then(|shell| shell.clean_wrapped_commands)
            .unwrap_or(true),
    )
}

/// Selection, search hits and the link under the pointer, in viewport rows.
/// Called once per repaint with the grid locked. Asking the pane to repaint
/// after a change is this feature's job: selection does not damage the grid.
pub fn highlights(pane: &TerminalPane, term: &Term<Listener>) -> Vec<CellHighlight> {
    let reading = &pane.reading;
    let colors = reading.colors.get();
    let offset = term.grid().display_offset() as i32;
    let (rows, columns) = (term.screen_lines(), term.columns());
    let mut out = Vec::new();
    // Later entries win where they overlap: the selection over a search hit,
    // both over a link's underline.
    let mut add = |range: &SelectionRange, bg: Option<Rgb24>, fg: Option<Rgb24>, underline| {
        for row in 0..rows {
            if let Some((start, end)) =
                selection::columns_on_line(range, row as i32 - offset, columns)
            {
                out.push(CellHighlight {
                    row,
                    start,
                    end,
                    bg,
                    fg,
                    underline,
                });
            }
        }
    };
    if let Some(link) = reading.hover.as_ref().and_then(|hover| hover.link.as_ref()) {
        add(
            &SelectionRange::new(link.start, link.end, false),
            None,
            None,
            true,
        );
    }
    if let Some(find) = reading.find.as_ref()
        && let Some(regex) = find.regex.borrow_mut().as_mut()
    {
        for found in find::visible(term, regex) {
            let color = if find.active.as_ref() == Some(&found) {
                colors.active
            } else {
                colors.found
            };
            let range = SelectionRange::new(*found.start(), *found.end(), false);
            add(&range, Some(color), None, false);
        }
    }
    if let Some(range) = selection::range(term) {
        add(
            &range,
            Some(colors.selection_bg),
            Some(colors.selection_fg),
            false,
        );
    }
    out
}

fn listen<E: 'static>(
    pane: &Entity<TerminalPane>,
    handler: fn(&mut TerminalPane, &E, &mut Window, &mut Context<TerminalPane>),
) -> impl Fn(&E, &mut Window, &mut App) + 'static {
    let pane = pane.downgrade();
    move |event, window, cx| {
        pane.update(cx, |pane, cx| handler(pane, event, window, cx))
            .ok();
    }
}

/// Adds selection and wheel listeners to a pane's root.
pub fn attach(
    element: Stateful<Div>,
    pane: &Entity<TerminalPane>,
    _window: &mut Window,
    _cx: &mut App,
) -> Stateful<Div> {
    element
        .on_mouse_down(MouseButton::Left, listen(pane, press))
        .on_mouse_down(MouseButton::Right, listen(pane, press_right))
        .on_mouse_move(listen(pane, hover))
        .on_scroll(listen(pane, wheel))
}

/// Drawn over the grid: the find bar, the scrollbar, the new-output button.
pub fn children(
    pane: &Entity<TerminalPane>,
    _window: &mut Window,
    cx: &mut App,
) -> Vec<AnyElement> {
    let id = pane.entity_id();
    let existing = cx.default_global::<Panes>().overlays.get(&id).cloned();
    let overlay = existing.unwrap_or_else(|| {
        let overlay = cx.new(|_| Overlay {
            pane: pane.downgrade(),
        });
        cx.default_global::<Panes>()
            .overlays
            .insert(id, overlay.clone());
        cx.observe_release(pane, move |_, cx| {
            let panes = cx.default_global::<Panes>();
            panes.overlays.remove(&id);
            let any = !panes.overlays.is_empty();
            set_context(cx, Some(any), None);
        })
        .detach();
        cx.defer(|cx| set_context(cx, Some(true), None));
        overlay
    });
    vec![AnyView::from(overlay).into_any_element()]
}

/// Input this pane sends returns it to live output, as xterm's
/// `scrollOnUserInput` does, and drops the selection, as xterm does on user
/// input. On Linux that is also what makes the next Ctrl+C the interrupt the
/// user is typing towards rather than a copy.
pub fn before_input(pane: &TerminalPane, cx: &mut Context<TerminalPane>) {
    clear_selection(pane, cx);
    if pane.reading_state() != TerminalReadingState::Live {
        return_to_live(pane, TO_HISTORY, cx);
    }
}

fn return_to_live(pane: &TerminalPane, reason: &str, cx: &mut Context<TerminalPane>) {
    pane.link().read(cx).send(Command::ReturnToLive {
        pane_id: pane.pane_id().to_string(),
        reason: reason.to_owned(),
    });
}

fn clear_selection(pane: &TerminalPane, cx: &mut Context<TerminalPane>) {
    let cleared = pane
        .terminal()
        .is_some_and(|terminal| terminal.lock().selection.take().is_some());
    if cleared {
        pane.repaint_all(cx);
    }
}

/// The cell under a pointer and which half of it, clamped to the grid.
fn cell_at(pane: &TerminalPane, position: gpui_kit::Point<Pixels>) -> Option<(Point, Side)> {
    let painted = pane.painted_at()?;
    let term = pane.terminal()?.lock();
    let (row, column, side) = grid_cell(
        f32::from(position.x - painted.origin.x),
        f32::from(position.y - painted.origin.y),
        (
            f32::from(painted.cell.width),
            f32::from(painted.cell.height),
        ),
        (term.columns(), term.screen_lines()),
    )?;
    let line = Line(row as i32 - term.grid().display_offset() as i32);
    Some((Point::new(line, Column(column)), side))
}

fn grid_cell(
    x: f32,
    y: f32,
    (width, height): (f32, f32),
    (columns, rows): (usize, usize),
) -> Option<(usize, usize, Side)> {
    if width <= 0. || height <= 0. || columns == 0 || rows == 0 {
        return None;
    }
    let column = (x / width).floor().clamp(0., (columns - 1) as f32) as usize;
    let side = if x >= (column as f32 + 0.5) * width {
        Side::Right
    } else {
        Side::Left
    };
    let row = (y / height).floor().clamp(0., (rows - 1) as f32) as usize;
    Some((row, column, side))
}

/// Whether this feature owns a press: the program does not read the mouse,
/// or the press forces a local selection anyway. Shift does on both platforms;
/// on the Mac Option does too, as xterm's `macOptionClickForcesSelection`.
fn owns_press(pane: &TerminalPane, modifiers: &Modifiers) -> bool {
    let option = Platform::current() == Platform::Mac && modifiers.alt;
    !pane.mouse_reporting() || modifiers.shift || option
}

fn word_selection(start: Point, end: Point) -> Selection {
    let mut word = Selection::new(SelectionType::Simple, start, Side::Left);
    word.update(end, Side::Right);
    word
}

fn press(
    pane: &mut TerminalPane,
    event: &MouseDownEvent,
    _: &mut Window,
    cx: &mut Context<TerminalPane>,
) {
    if !owns_press(pane, &event.modifiers) {
        return;
    }
    let (Some((point, side)), Some(terminal)) = (cell_at(pane, event.position), pane.terminal())
    else {
        return;
    };
    let mut term = terminal.lock();
    let link = links::is_link_activation(&event.modifiers, Platform::current())
        .then(|| links::link_at(&*term, point))
        .flatten();
    let grow = match event.click_count {
        2 => {
            let word = selection::word_at(&*term, point);
            term.selection = word.map(|(start, end)| word_selection(start, end));
            word.map_or(Grow::Cells, |(start, end)| Grow::Words(start, end))
        }
        count if count >= 3 => {
            term.selection = Some(Selection::new(SelectionType::Lines, point, side));
            Grow::Lines
        }
        _ => {
            match term.selection.as_mut() {
                // xterm extends a standing selection on a Shift click.
                Some(standing) if event.modifiers.shift => standing.update(point, side),
                _ => {
                    // Alt drags a rectangle, xterm's column selection. Not on
                    // the Mac, where Option forces a plain selection instead.
                    let kind = if event.modifiers.alt && Platform::current() != Platform::Mac {
                        SelectionType::Block
                    } else {
                        SelectionType::Simple
                    };
                    term.selection = Some(Selection::new(kind, point, side));
                }
            }
            Grow::Cells
        }
    };
    drop(term);
    pane.reading.drag = Some(Drag::Select { grow, link });
    pane.repaint_all(cx);
}

/// xterm's `rightClickSelectsWord`: a right click outside the selection
/// selects the word under it.
fn press_right(
    pane: &mut TerminalPane,
    event: &MouseDownEvent,
    _: &mut Window,
    cx: &mut Context<TerminalPane>,
) {
    if !owns_press(pane, &event.modifiers) {
        return;
    }
    let (Some((point, _)), Some(terminal)) = (cell_at(pane, event.position), pane.terminal())
    else {
        return;
    };
    {
        let mut term = terminal.lock();
        if selection::range(&*term).is_some_and(|range| range.contains(point)) {
            return;
        }
        let Some((start, end)) = selection::word_at(&*term, point) else {
            return;
        };
        term.selection = Some(word_selection(start, end));
    }
    copy_on_select(pane, cx);
    pane.repaint_all(cx);
}

fn hover(
    pane: &mut TerminalPane,
    event: &MouseMoveEvent,
    _: &mut Window,
    cx: &mut Context<TerminalPane>,
) {
    if pane.reading.drag.is_some() {
        return;
    }
    let point = (!pane.mouse_reporting())
        .then(|| cell_at(pane, event.position))
        .flatten();
    let (Some((point, _)), Some(terminal)) = (point, pane.terminal().cloned()) else {
        set_hover(pane, None, cx);
        return;
    };
    // A row's links are found once; moving along it costs nothing more.
    if let Some(current) = &pane.reading.hover
        && current.line == point.line
        && current
            .link
            .as_ref()
            .is_none_or(|link| link.contains(point))
    {
        return;
    }
    let link = links::link_at(&*terminal.lock(), point);
    set_hover(
        pane,
        Some(Hover {
            line: point.line,
            link,
        }),
        cx,
    );
}

fn set_hover(pane: &mut TerminalPane, hover: Option<Hover>, cx: &mut Context<TerminalPane>) {
    let link = |hover: &Option<Hover>| hover.as_ref().and_then(|hover| hover.link.clone());
    let changed = link(&pane.reading.hover) != link(&hover);
    pane.reading.hover = hover;
    if changed {
        pane.repaint_all(cx);
    }
}

/// A move anywhere in the window while a press that began here is held.
fn drag_to(
    pane: &mut TerminalPane,
    event: &MouseMoveEvent,
    window: &mut Window,
    cx: &mut Context<TerminalPane>,
) {
    if event.pressed_button != Some(MouseButton::Left) {
        // The release went somewhere this pane could not hear it.
        let release = MouseUpEvent {
            button: MouseButton::Left,
            position: event.position,
            modifiers: event.modifiers,
            click_count: 1,
        };
        return release_at(pane, &release, window, cx);
    }
    match pane.reading.drag {
        Some(Drag::Scrollbar) => scroll_to_pointer(pane, event.position, window, cx),
        Some(Drag::Select { grow, .. }) => {
            // Past the top or bottom edge, the view follows the selection.
            if let (Some(painted), Some(terminal)) = (pane.painted_at(), pane.terminal()) {
                let rows = terminal.lock().screen_lines() as f32;
                let bottom = painted.origin.y + painted.cell.height * rows;
                if event.position.y < painted.origin.y {
                    scroll::scroll(pane, 1, cx);
                } else if event.position.y >= bottom {
                    scroll::scroll(pane, -1, cx);
                }
            }
            let (Some((point, side)), Some(terminal)) =
                (cell_at(pane, event.position), pane.terminal())
            else {
                return;
            };
            grow_selection(&mut terminal.lock(), grow, point, side);
            pane.repaint_all(cx);
        }
        None => {}
    }
}

fn grow_selection(term: &mut Term<Listener>, grow: Grow, point: Point, side: Side) {
    match grow {
        Grow::Cells | Grow::Lines => {
            if let Some(selection) = term.selection.as_mut() {
                selection.update(point, side);
            }
        }
        // Whichever way the drag goes, the double-clicked word stays whole.
        Grow::Words(start, end) => {
            let (word_start, word_end) = selection::word_at(term, point).unwrap_or((point, point));
            term.selection = Some(if point < start {
                word_selection(word_start, end)
            } else {
                word_selection(start, word_end)
            });
        }
    }
}

fn release_at(
    pane: &mut TerminalPane,
    event: &MouseUpEvent,
    window: &mut Window,
    cx: &mut Context<TerminalPane>,
) {
    let Some(drag) = pane.reading.drag.take() else {
        return;
    };
    let (Drag::Select { link, .. }, Some(terminal)) = (drag, pane.terminal().cloned()) else {
        cx.notify();
        return;
    };
    let selected = {
        let mut term = terminal.lock();
        let selected = selection::range(&*term).is_some();
        if !selected {
            term.selection = None;
        }
        selected
    };
    if selected {
        copy_on_select(pane, cx);
    } else if let Some(link) = link
        && cell_at(pane, event.position).is_some_and(|(point, _)| link.contains(point))
    {
        activate(&link, window, cx);
    }
    pane.repaint_all(cx);
}

fn activate(link: &Link, window: &mut Window, cx: &mut App) {
    match link.kind {
        LinkKind::Web => {
            let Some(url) = links::external_url(&link.text) else {
                return;
            };
            if let Err(error) = desktop_core::external_links::open_external_link(url, true) {
                notices::notify(
                    &format!("Could not open the link in your browser: {error}"),
                    window,
                    cx,
                );
            }
        }
        // Resolving a path on the host and opening it in a tab needs the file
        // tabs of Phase 4 and the editor of Phase 5.
        LinkKind::File => {
            notices::notify(
                &format!(
                    "Could not open {}: opening files from the terminal is not available yet.",
                    link.text
                ),
                window,
                cx,
            );
        }
    }
}

fn wheel(
    pane: &mut TerminalPane,
    event: &ScrollEvent,
    _: &mut Window,
    cx: &mut Context<TerminalPane>,
) {
    if pane.mouse_reporting() {
        return;
    }
    let Some(terminal) = pane.terminal().cloned() else {
        return;
    };
    let cell = pane.metrics().cell_height;
    let pixels = f32::from(event.delta.pixel_delta(cell).y);
    let lines = scroll::wheel_lines(&mut pane.reading.wheel_carry, pixels, f32::from(cell));
    if lines == 0 {
        return;
    }
    let mode = *terminal.lock().mode();
    if mode.contains(TermMode::ALT_SCREEN) {
        let keys = scroll::alternate_scroll_bytes(lines, mode.contains(TermMode::APP_CURSOR));
        // A pane with no live client has nowhere to send them, and nothing to
        // say about it that the pane does not already show.
        let _ = pane.send_input(keys, cx);
        return;
    }
    scroll::scroll(pane, lines, cx);
}

fn scroll_to_pointer(
    pane: &mut TerminalPane,
    position: gpui_kit::Point<Pixels>,
    window: &Window,
    cx: &mut Context<TerminalPane>,
) {
    let (Some(bounds), Some(terminal)) = (pane.reading.bounds.get(), pane.terminal()) else {
        return;
    };
    let (history, rows) = {
        let term = terminal.lock();
        (term.grid().history_size(), term.screen_lines())
    };
    let offset = scroll::offset_at(
        history,
        rows,
        f32::from(bounds.size.height),
        f32::from(ui(MIN_THUMB).to_pixels(window.rem_size())),
        f32::from(position.y - bounds.origin.y),
    );
    scroll::scroll_to(pane, offset, cx);
}

fn selected_text(pane: &TerminalPane, clean: bool) -> Option<String> {
    selection::copy_text(&*pane.terminal()?.lock(), clean)
}

fn copy_on_select(pane: &TerminalPane, cx: &mut Context<TerminalPane>) {
    let (enabled, clean) = copy_settings(cx);
    if let Some(text) = enabled.then(|| selected_text(pane, clean)).flatten() {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }
}

/// `terminal.copy`. Port of the pane controller's `copy`.
pub fn copy(
    pane: &mut TerminalPane,
    platform: Platform,
    window: &mut Window,
    cx: &mut Context<TerminalPane>,
) {
    let (_, clean) = copy_settings(cx);
    if let Some(text) = selected_text(pane, clean) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    } else if platform == Platform::Mac && pane.mouse_reporting() {
        // No selection of ours, and the program holds the mouse: the
        // selection is its own, so it gets the copy chord.
        if let Err(error) = pane.send_input(KITTY_SUPER_C.to_vec(), cx) {
            notices::notify(
                &format!("Could not copy in the terminal: {error}"),
                window,
                cx,
            );
        }
    }
    // Linux Ctrl+C is copy and interrupt on one chord: once copied, the next
    // one interrupts.
    if platform == Platform::Linux {
        clear_selection(pane, cx);
    }
}

fn open_find(pane: &mut TerminalPane, window: &mut Window, cx: &mut Context<TerminalPane>) {
    if let Some(find) = &pane.reading.find {
        let focus = find.input.focus_handle(cx);
        window.focus(&focus, cx);
        return;
    }
    let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find"));
    let subscription = cx.subscribe(&input, |pane, _, event: &InputEvent, cx| match event {
        InputEvent::PressEnter { shift, .. } => find_next(pane, !shift, cx),
        InputEvent::Change => {
            if let Some(find) = pane.reading.find.as_mut()
                && find.miss
            {
                find.miss = false;
                cx.notify();
            }
        }
        _ => {}
    });
    window.focus(&input.focus_handle(cx), cx);
    pane.reading.find = Some(Find {
        input,
        regex: Default::default(),
        active: None,
        miss: false,
        _subscription: subscription,
    });
    cx.notify();
}

fn close_find(pane: &mut TerminalPane, window: &mut Window, cx: &mut Context<TerminalPane>) {
    if pane.reading.find.take().is_some() {
        window.focus(&pane.focus_handle(cx), cx);
        pane.repaint_all(cx);
    }
}

fn find_next(pane: &mut TerminalPane, forward: bool, cx: &mut Context<TerminalPane>) {
    let Some(terminal) = pane.terminal().cloned() else {
        return;
    };
    let Some(find) = pane.reading.find.as_mut() else {
        return;
    };
    let query = find.input.read(cx).value();
    let mut offset = None;
    let found = find::compile(&query).and_then(|mut regex| {
        let term = terminal.lock();
        let found = find::search(&term, &mut regex, find.active.as_ref(), forward);
        offset = found
            .as_ref()
            .and_then(|found| scroll::offset_showing(&term, found.start().line.0));
        *find.regex.borrow_mut() = Some(regex);
        found
    });
    find.miss = found.is_none();
    find.active = found;
    if let Some(offset) = offset {
        scroll::scroll_to(pane, offset, cx);
    }
    pane.repaint_all(cx);
}

fn blend(base: Rgb24, over: Rgb24, alpha: f32) -> Rgb24 {
    let channel = |shift: u32| {
        let (from, to) = (
            ((base >> shift) & 0xff) as f32,
            ((over >> shift) & 0xff) as f32,
        );
        ((from + (to - from) * alpha).round() as u32) << shift
    };
    channel(16) | channel(8) | channel(0)
}

fn rgb24(color: gpui_kit::Rgba) -> Rgb24 {
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u32;
    (channel(color.r) << 16) | (channel(color.g) << 8) | channel(color.b)
}

/// What this feature draws over one pane.
pub struct Overlay {
    pane: WeakEntity<TerminalPane>,
}

/// What the overlay needs from its pane, read before anything is built.
struct Frame {
    cursor: Option<CursorStyle>,
    focused: bool,
    selected: bool,
    outdated: bool,
    /// Thumb top and height, and the strip's width.
    scrollbar: Option<(f32, f32, Pixels)>,
    find: Option<(Entity<InputState>, bool)>,
    label: SharedString,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl Overlay {
    fn frame(&self, pane: &TerminalPane, palette: &Palette, window: &Window, cx: &App) -> Frame {
        let term_bg = rgb24(palette.term_bg);
        pane.reading.colors.set(Colors {
            selection_bg: rgb24(palette.term_selection_bg),
            selection_fg: rgb24(palette.term_selection_fg),
            // `searchDecorations`: translucent, so the glyph stays legible.
            found: blend(term_bg, rgb24(palette.chrome_dim), 0x66 as f32 / 255.),
            active: blend(term_bg, rgb24(palette.accent), 0x99 as f32 / 255.),
        });
        let bounds = pane.reading.bounds.clone();
        let (mut selected, mut scrollbar) = (false, None);
        if let Some(terminal) = pane.terminal() {
            let term = terminal.lock();
            selected = selection::range(&*term).is_some();
            let track = bounds.get().map(|bounds| f32::from(bounds.size.height));
            let min = f32::from(ui(MIN_THUMB).to_pixels(window.rem_size()));
            if !term.mode().contains(TermMode::ALT_SCREEN)
                && let Some(track) = track
            {
                let history = term.grid().history_size();
                let offset = term.grid().display_offset();
                scrollbar = scroll::thumb(history, offset, term.screen_lines(), track, min)
                    .map(|(top, height)| (top, height, pane.metrics().scrollbar_width));
            }
        }
        let on_link = pane
            .reading
            .hover
            .as_ref()
            .is_some_and(|hover| hover.link.is_some());
        Frame {
            cursor: if on_link {
                Some(CursorStyle::PointingHand)
            } else {
                (!pane.mouse_reporting()).then_some(CursorStyle::IBeam)
            },
            focused: pane.focus_handle(cx).contains_focused(window, cx),
            selected,
            outdated: pane.reading_state() == TerminalReadingState::Outdated,
            scrollbar,
            find: pane
                .reading
                .find
                .as_ref()
                .map(|find| (find.input.clone(), find.miss)),
            label: format!("Search terminal pane {}", pane.pane_id()).into(),
            bounds,
        }
    }

    fn scrollbar(
        &self,
        top: f32,
        height: f32,
        width: Pixels,
        palette: &Palette,
    ) -> impl IntoElement {
        let pane = self.pane.clone();
        div()
            .id("terminal-scrollbar")
            .absolute()
            .top_0()
            .right_0()
            .h_full()
            .w(width)
            .occlude()
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                pane.update(cx, |pane, cx| {
                    pane.reading.drag = Some(Drag::Scrollbar);
                    scroll_to_pointer(pane, event.position, window, cx);
                })
                .ok();
            })
            .child(
                div()
                    .absolute()
                    .top(px(top))
                    .h(px(height))
                    .left(ui(3.))
                    .right(ui(3.))
                    .rounded(ui(3.))
                    .bg(palette.chrome_faint),
            )
    }

    fn new_output(&self, palette: &Palette) -> impl IntoElement {
        let pane = self.pane.clone();
        div()
            .id("terminal-new-output")
            .role(Role::Button)
            .aria_label("New output")
            .absolute()
            .right(ui(12.))
            .bottom(ui(10.))
            .px(ui(9.))
            .py(ui(4.))
            .border_1()
            .border_color(palette.accent)
            .rounded(ui(12.))
            .bg(palette.chrome_raised)
            .text_color(palette.chrome_ink)
            .text_size(ui(TYPE_SECONDARY))
            .occlude()
            .cursor_pointer()
            .on_click(move |_, _, cx| {
                pane.update(cx, |pane, cx| return_to_live(pane, TO_LIVE, cx))
                    .ok();
            })
            .child("New output \u{2193}")
    }

    fn find_bar(
        &self,
        label: SharedString,
        input: Entity<InputState>,
        miss: bool,
        palette: &Palette,
    ) -> impl IntoElement {
        let button =
            |id: &'static str, icon: IconName, name: &'static str, forward: Option<bool>| {
                let pane = self.pane.clone();
                Button::new(id)
                    .ghost()
                    .xsmall()
                    .icon(icon)
                    .accessibility_label(name)
                    .on_click(move |_, window, cx| {
                        pane.update(cx, |pane, cx| match forward {
                            Some(forward) => find_next(pane, forward, cx),
                            None => close_find(pane, window, cx),
                        })
                        .ok();
                    })
            };
        let escape = {
            let pane = self.pane.clone();
            let input = input.clone();
            move |_: &Escape, window: &mut Window, cx: &mut App| {
                // Escape inside a composition ends the composition.
                if is_composing(&input, window, cx) {
                    return;
                }
                cx.stop_propagation();
                pane.update(cx, |pane, cx| close_find(pane, window, cx))
                    .ok();
            }
        };
        h_flex()
            .id("terminal-search")
            .role(Role::Search)
            .aria_label(label)
            .absolute()
            .top(ui(6.))
            .right(ui(6.))
            .gap(ui(3.))
            .p(ui(4.))
            .border_1()
            .border_color(palette.chrome_border)
            .rounded(ui(RADIUS_CONTROL))
            .bg(palette.chrome_raised)
            .occlude()
            .capture_action(escape)
            .child(
                div().w(ui(190.)).child(
                    Input::new(&input)
                        .id("terminal-search-input")
                        .aria_label("Find in terminal")
                        .small(),
                ),
            )
            .child(button(
                "terminal-search-previous",
                IconName::ArrowUp,
                "Previous match",
                Some(false),
            ))
            .child(button(
                "terminal-search-next",
                IconName::ArrowDown,
                "Next match",
                Some(true),
            ))
            .child(button(
                "terminal-search-close",
                IconName::Close,
                "Close search",
                None,
            ))
            .when(miss, |bar| {
                bar.child(
                    div()
                        .absolute()
                        .top(ui(34.))
                        .right_0()
                        .px(ui(7.))
                        .py(ui(3.))
                        .rounded(ui(3.))
                        .text_size(ui(TYPE_SECONDARY))
                        .text_color(palette.danger_ink)
                        .bg(palette.danger_wash)
                        .child("No match"),
                )
            })
    }
}

impl Render for Overlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div().absolute().top_0().left_0().size_full();
        let Some(pane) = self.pane.upgrade() else {
            return root;
        };
        let palette = theme::palette(cx);
        let frame = self.frame(pane.read(cx), &palette, window, cx);
        if frame.focused {
            let (pane, selected) = (self.pane.clone(), frame.selected);
            cx.defer(move |cx| {
                cx.default_global::<Panes>().last_focused = Some(pane);
                set_context(cx, None, Some(selected));
            });
        }
        let bounds = frame.bounds.clone();
        root.when_some(frame.cursor, |root, cursor| root.cursor(cursor))
            .child(
                canvas(
                    move |painted, _, _| bounds.set(Some(painted)),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(window_listeners(&self.pane))
            .when_some(frame.scrollbar, |root, (top, height, width)| {
                root.child(self.scrollbar(top, height, width, &palette))
            })
            .when(frame.outdated, |root| root.child(self.new_output(&palette)))
            .when_some(frame.find, |root, (input, miss)| {
                root.child(self.find_bar(frame.label, input, miss, &palette))
            })
    }
}

/// Moves and releases anywhere in the window, for a drag that leaves the pane
/// and for a pointer that leaves a link. Installed on every frame rather than
/// once a press begins, so a move that arrives before the next frame still
/// extends the selection.
fn window_listeners(pane: &WeakEntity<TerminalPane>) -> impl IntoElement {
    let pane = pane.clone();
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let moved = pane.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                moved
                    .update(cx, |pane, cx| {
                        if pane.reading.drag.is_some() {
                            drag_to(pane, event, window, cx);
                        } else if !bounds.contains(&event.position) {
                            set_hover(pane, None, cx);
                        }
                    })
                    .ok();
            });
            window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                    pane.update(cx, |pane, cx| release_at(pane, event, window, cx))
                        .ok();
                }
            });
        },
    )
    .absolute()
    .size_full()
}

#[cfg(test)]
mod tests {
    use gpui_kit::rgb;

    use super::*;

    #[test]
    fn a_pointer_maps_to_a_cell_and_the_half_it_is_in() {
        let cell = (8., 17.);
        let grid = (80, 24);
        assert_eq!(grid_cell(3., 5., cell, grid), Some((0, 0, Side::Left)));
        assert_eq!(grid_cell(13., 20., cell, grid), Some((1, 1, Side::Right)));
        assert_eq!(
            grid_cell(-5., 9999., cell, grid),
            Some((23, 0, Side::Left)),
            "clamped to the grid"
        );
        assert_eq!(grid_cell(9999., 0., cell, grid), Some((0, 79, Side::Right)));
        assert_eq!(grid_cell(1., 1., (0., 17.), grid), None);
    }

    #[test]
    fn a_translucent_highlight_is_mixed_into_the_terminal_background() {
        assert_eq!(blend(0x000000, 0xffffff, 0.4), 0x666666);
        assert_eq!(blend(0x102030, 0x102030, 0.6), 0x102030);
        assert_eq!(rgb24(rgb(0x7aa6da)), 0x7aa6da);
    }
}
