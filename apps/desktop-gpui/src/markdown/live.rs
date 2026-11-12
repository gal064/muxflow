//! A Markdown tab's Live view: the `live-markdown` crate's view over the tab's
//! document buffer. Port of `features/files/LiveMarkdownEditor.tsx`.
//!
//! The buffer stays the document's, so autosave, dirty state, changes on disk
//! and reconnects are handled where the source editor's are. The view takes
//! the buffer's text when it changes from anywhere else, and each of its edits
//! goes into the buffer as one replacement of the span that differs. An IME
//! preedit is never handed over until it is committed.

use std::collections::HashMap;

use gpui_kit::{
    AnyElement, AnyView, App, AppContext, Context, Entity, EntityId, FocusHandle, Focusable,
    Global, IntoElement, Render, StyleRefinement, Styled, Subscription, WeakEntity, Window,
};
use live_markdown::{
    doc::minimal_change,
    view::{LiveEvent, LiveStyle, LiveView},
};

use crate::{
    editor::{Document, LiveMarkdown},
    fonts,
};

/// Each document's Live view, kept while the document lives, so its caret,
/// scroll and undo outlast a tab switch as the source editor's do.
#[derive(Default)]
struct Surfaces(HashMap<EntityId, Entity<Surface>>);

impl Global for Surfaces {}

/// The seam the editor draws Live through.
pub fn live_markdown() -> LiveMarkdown {
    LiveMarkdown {
        surface: std::rc::Rc::new(surface),
        limit_bytes: live_markdown::LIMIT_BYTES,
    }
}

/// The Live view of `document` and the handle that takes its keyboard.
fn surface(
    _: &desktop_core::app_state::AppTabRecord,
    document: &Entity<Document>,
    window: &mut Window,
    cx: &mut App,
) -> (AnyElement, FocusHandle) {
    let surface = kept(document, window, cx);
    let focus = surface.read(cx).view.focus_handle(cx);
    (surface.into_any_element(), focus)
}

/// The document's Live surface, made the first time it is shown.
fn kept(document: &Entity<Document>, window: &mut Window, cx: &mut App) -> Entity<Surface> {
    let id = document.entity_id();
    if let Some(surface) = cx
        .try_global::<Surfaces>()
        .and_then(|surfaces| surfaces.0.get(&id))
    {
        return surface.clone();
    }
    let surface = cx.new(|cx| Surface::new(document, window, cx));
    cx.observe_release(document, move |_, cx| {
        cx.default_global::<Surfaces>().0.remove(&id);
    })
    .detach();
    cx.default_global::<Surfaces>()
        .0
        .insert(id, surface.clone());
    surface
}

/// The buffer as the view holds it: line breaks are `\n` whatever the file
/// uses, and autosave writes the file's own back.
fn normalized(buffer: &str) -> String {
    buffer.replace("\r\n", "\n").replace('\r', "\n")
}

fn style(cx: &App) -> LiveStyle {
    let (p, dark) = super::view::palette(cx);
    LiveStyle {
        sans: fonts::SANS.into(),
        mono: fonts::MONO.into(),
        background: p.chrome_bg.into(),
        ink: p.chrome_ink.into(),
        ink_strong: p.chrome_ink_strong.into(),
        dim: p.chrome_dim.into(),
        accent: p.accent.into(),
        selection: p.accent_wash.into(),
        raised: p.chrome_raised.into(),
        hairline: p.chrome_hairline.into(),
        border: p.chrome_border.into(),
        dark,
    }
}

struct Surface {
    /// Weak: the surface is kept until the document is released.
    document: WeakEntity<Document>,
    view: Entity<LiveView>,
    /// The document revision the view shows.
    synced: u64,
    _subscriptions: Vec<Subscription>,
}

impl Surface {
    fn new(document: &Entity<Document>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let buffer = document.read(cx).text().map(|text| text.to_string());
        let text = normalized(buffer.as_deref().unwrap_or_default());
        let style = style(cx);
        let view = cx.new(|cx| LiveView::new(&text, style, window, cx));
        let subscriptions = vec![
            cx.observe(document, |this, _, cx| this.sync(cx)),
            cx.subscribe(&view, |this, _, event: &LiveEvent, cx| match event {
                LiveEvent::Edited => this.push(cx),
            }),
        ];
        Self {
            synced: document.read(cx).revision(),
            document: document.downgrade(),
            view,
            _subscriptions: subscriptions,
        }
    }

    /// The buffer changed: on disk, after a reconnect, or in the source
    /// editor. The view takes the text and keeps its caret.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let Some(document) = self.document.upgrade() else {
            return;
        };
        let document = document.read(cx);
        if document.revision() == self.synced {
            return;
        }
        self.synced = document.revision();
        let Some(buffer) = document.text() else {
            return;
        };
        let text = normalized(&buffer.to_string());
        self.view.update(cx, |view, cx| view.set_text(&text, cx));
    }

    /// The view's edits, into the buffer: the one span where the buffer and
    /// the view differ. A composition typed and cancelled is no edit. A CRLF
    /// buffer differs from the view at every line break, so its first edit
    /// writes the view's text whole and autosave restores the CRLFs.
    fn push(&mut self, cx: &mut Context<Self>) {
        if !self.view.update(cx, |view, _| view.take_edited()) {
            return;
        }
        let Some(document) = self.document.upgrade() else {
            return;
        };
        let Some(buffer) = document.read(cx).text().map(|text| text.to_string()) else {
            return;
        };
        let text = self.view.read(cx).text().to_owned();
        if normalized(&buffer) == text {
            return;
        }
        let (start, old_end, new_end) = minimal_change(&buffer, &text);
        let revision = document.read(cx).revision();
        document.update(cx, |document, cx| {
            document.replace(start..old_end, &text[start..new_end], cx)
        });
        self.synced = document.read(cx).revision();
        // Refused (read-only since the view last drew): the view shows the
        // buffer again rather than text that will never be saved.
        if self.synced == revision {
            let buffer = normalized(&buffer);
            self.view.update(cx, |view, cx| view.set_text(&buffer, cx));
        }
    }
}

impl Render for Surface {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let read_only = !self
            .document
            .upgrade()
            .is_some_and(|document| document.read(cx).can_write(cx));
        let style = style(cx);
        self.view.update(cx, |view, cx| {
            view.set_read_only(read_only, cx);
            view.set_style(style, cx);
        });
        // Cached: the rest of the window redrawing (a terminal's output, a
        // blink) does not lay the document out again.
        AnyView::from(self.view.clone()).cached(StyleRefinement::default().size_full())
    }
}

#[cfg(test)]
#[path = "live_tests.rs"]
mod tests;
