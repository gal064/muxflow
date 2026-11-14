//! The preview entity: one Kit Markdown view following one document's buffer.

use std::{
    cell::Cell,
    collections::HashMap,
    rc::Rc,
    sync::{Arc, Mutex},
};

use base64::Engine as _;
use desktop_core::app_state::{AppTabRecord, MarkdownAppearance};
use gpui_kit::{
    AnyElement, App, AppContext, Bounds, ClickEvent, Context, Entity, FontWeight, HighlightStyle,
    Image, ImageCacheError, ImageFormat, ImageSource, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Pixels, Point, Render, Role, SharedString, SharedUri,
    StatefulInteractiveElement, StyleRefinement, Styled, Subscription, Window,
    base::{TextView, TextViewStyle, text::TextViewState},
    canvas, div,
    prelude::FluentBuilder,
    px, relative, rems,
};
use regex::Regex;
use std::sync::LazyLock;

use super::link::{self, Target};
use crate::{
    commands::dialogs::{Confirmation, open_confirmation},
    editor::{self, Document},
    theme::{
        self, Palette,
        metrics::{NARROW_TERMINAL, narrower_than},
    },
};

/// Document typography belongs to the page, not to the app's UI scale.
const FONT_SIZE: f32 = 16.;
const LINE_HEIGHT: f32 = 1.65;
const CONTENT_WIDTH: f32 = 720.;

/// Embedded raster images only; the whole payload must stay base64. SVG is not
/// among them, as in Tauri's `isEmbeddedMarkdownImage`.
static EMBEDDED_IMAGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^data:image/(png|gif|jpe?g|webp);base64,([a-z\d+/]+={0,2})$")
        .expect("a valid pattern")
});

/// The image a document may show for `src`: an embedded raster, or nothing. A
/// remote or file source is never fetched, so a document cannot beacon or read
/// the host's files.
fn embedded_image(src: &str) -> Option<Arc<Image>> {
    let captures = EMBEDDED_IMAGE.captures(src)?;
    let format = match captures[1].to_ascii_lowercase().as_str() {
        "png" => ImageFormat::Png,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::Webp,
        _ => ImageFormat::Jpeg,
    };
    // A browser decodes a data URL leniently, so a missing pad is not an error.
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    );
    let bytes = engine.decode(&captures[2]).ok()?;
    Some(Arc::new(Image::from_bytes(format, bytes)))
}

/// Decoded once per source, not per frame.
type Images = Arc<Mutex<HashMap<SharedUri, Option<Arc<Image>>>>>;

fn image_source(images: &Images, uri: &SharedUri) -> ImageSource {
    let mut images = images.lock().expect("the image cache is never poisoned");
    // An edited payload is a new key each keystroke; keep the cache small.
    if images.len() > 64 {
        images.clear();
    }
    let image = images
        .entry(uri.clone())
        .or_insert_with(|| embedded_image(uri))
        .clone();
    match image {
        Some(image) => ImageSource::Image(image),
        None => ImageSource::Custom(Arc::new(|_, _| {
            Some(Err(ImageCacheError::Asset("image not allowed".into())))
        })),
    }
}

static IMAGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"!\[([^\]]*)\]\(\s*<?([^)\s>]*)>?[^)]*\)").expect("a valid pattern")
});

/// An image the preview will not show, as its alt text in a code span: a browser draws a
/// blocked image as a box holding the alt text, where Kit's text view leaves blank
/// space. Fenced code and inline code are left as written.
fn show_blocked_images(markdown: &str) -> String {
    // The open fence's marker: a fence closes only on the same character, as long.
    let mut fence: Option<(char, usize)> = None;
    let mut out = String::with_capacity(markdown.len());
    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let marker = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'));
        let run = marker.map_or(0, |m| trimmed.chars().take_while(|c| *c == m).count());
        match (fence, marker) {
            (None, Some(m)) if run >= 3 && !(m == '`' && trimmed[run..].contains('`')) => {
                fence = Some((m, run));
                out.push_str(line);
                continue;
            }
            (Some((open, len)), _) => {
                if marker == Some(open) && run >= len && trimmed[run..].trim().is_empty() {
                    fence = None;
                }
                out.push_str(line);
                continue;
            }
            _ => {}
        }
        // Odd pieces between backticks are inline code.
        for (index, piece) in line.split('`').enumerate() {
            if index > 0 {
                out.push('`');
            }
            if index % 2 == 1 {
                out.push_str(piece);
            } else {
                out.push_str(&IMAGE.replace_all(piece, alt_text));
            }
        }
    }
    out
}

fn alt_text(found: &regex::Captures) -> String {
    if EMBEDDED_IMAGE.is_match(&found[2]) {
        return found[0].to_owned();
    }
    match found[1].trim() {
        "" => String::new(),
        alt => format!("`{alt}`"),
    }
}

/// Whether a refresh must wait: replacing the text under a selection, or one
/// being dragged out, would drop it.
fn holds(selecting: bool, selected: &str) -> bool {
    selecting || !selected.is_empty()
}

pub struct PreviewView {
    document: Entity<Document>,
    text: Entity<TextViewState>,
    /// The buffer the text view shows.
    shown: Option<gpui_kit::component::input::Rope>,
    images: Images,
    /// Where the last left press in the preview landed, to tell a click from
    /// a drag that ends on a link.
    pressed: Arc<Mutex<Option<Point<Pixels>>>>,
    /// The pane's width at the last layout, which the column's side padding
    /// follows.
    pane_width: Rc<Cell<Option<Pixels>>>,
    title: SharedString,
    session_id: String,
    _subscriptions: Vec<Subscription>,
}

impl PreviewView {
    fn new(record: &AppTabRecord, document: Entity<Document>, cx: &mut Context<Self>) -> Self {
        let text = cx.new(|cx| TextViewState::markdown("", cx));
        let subscriptions = vec![
            cx.observe(&document, |this, _, cx| this.refresh(cx)),
            // A selection ending releases a refresh that was waiting on it.
            cx.observe(&text, |this, _, cx| this.refresh(cx)),
        ];
        let mut this = Self {
            document,
            text,
            shown: None,
            images: Images::default(),
            pressed: Arc::default(),
            pane_width: Rc::default(),
            title: record.title.clone().into(),
            session_id: record.session_id.clone(),
            _subscriptions: subscriptions,
        };
        this.refresh(cx);
        this
    }

    /// Shows the buffer as it stands, unless the user is holding a selection
    /// in the preview. Port of `useSanitizedMarkdown`'s selection hold.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(buffer) = self.document.read(cx).text() else {
            return;
        };
        if self.shown.as_ref() == Some(&buffer) {
            return;
        }
        let state = self.text.read(cx);
        if holds(state.is_selecting(), &state.selected_text()) {
            return;
        }
        self.text.update(cx, |state, cx| {
            state.set_text(&show_blocked_images(&buffer.to_string()), cx)
        });
        self.shown = Some(buffer);
        cx.notify();
    }
}

/// The preview's palette: the app's, unless Settings chose one.
pub(super) fn palette(cx: &App) -> (Palette, bool) {
    match crate::settings::get(cx).markdown_appearance {
        MarkdownAppearance::Default => {
            (theme::palette(cx), cx.global::<theme::Appearance>().is_dark)
        }
        MarkdownAppearance::Light => (theme::light(), false),
        MarkdownAppearance::Dark => (theme::dark(), true),
    }
}

/// The `.markdown-preview` rules of `styles.css`.
/// A heading's size in ems of the body text, from `styles.css` for levels 1 to 3 and
/// the browser's own for the rest. Kit's steps are gentler and left the title
/// visibly smaller.
fn heading_scale(level: u8) -> f32 {
    match level {
        1 => 2.,
        2 => 1.55,
        3 => 1.25,
        4 => 1.,
        5 => 0.83,
        _ => 0.67,
    }
}

fn style(p: &Palette, dark: bool) -> TextViewStyle {
    let rounded = StyleRefinement::default().rounded(px(8.));
    // Kit fills a table from the app's theme unless told otherwise.
    let table = rounded.clone().bg(p.chrome_bg);
    let strong = p.chrome_ink_strong;
    TextViewStyle::default()
        .with_foreground(p.chrome_ink.into())
        .with_muted_foreground(p.chrome_dim.into())
        .with_link(p.accent.into())
        .with_selection(p.accent_wash.into())
        .with_code_background(p.chrome_raised.into())
        .with_border(p.chrome_border.into())
        .with_paragraph_gap(rems(1.))
        .with_heading(move |level| {
            StyleRefinement::default()
                .text_size(px(FONT_SIZE * heading_scale(level)))
                .text_color(strong)
                .font_weight(FontWeight::SEMIBOLD)
        })
        .with_code_block(rounded.clone().border_1().border_color(p.chrome_hairline))
        .with_table(table)
        .with_table_head(
            StyleRefinement::default()
                .bg(p.chrome_raised)
                .text_color(strong)
                .font_weight(FontWeight::SEMIBOLD),
        )
        .with_inline_code(HighlightStyle {
            background_color: Some(p.chrome_raised.into()),
            ..Default::default()
        })
        .with_dark(dark)
}

/// What a link is resolved and checked against: the tab's file and workspace.
#[derive(Clone)]
pub(super) struct Origin {
    pub(super) resource: String,
    pub(super) host: String,
    pub(super) server: String,
    pub(super) session_id: String,
}

/// How far the pointer may travel between press and release and still be a click.
const CLICK_SLOP: f32 = 4.;

/// Tauri's click test. Kit reports a link on any button's release over it,
/// wherever the press began, so without this a drag that ends on a link, or a
/// right click, would follow it. Kit itself already holds off while the
/// preview has a selection.
fn is_click(event: &ClickEvent, pressed: Option<Point<Pixels>>) -> bool {
    match event {
        ClickEvent::Mouse(click) => {
            let travelled = pressed.map(|at| at - click.up.position);
            click.up.button == MouseButton::Left
                && travelled.is_some_and(|d| f32::from(d.x).hypot(f32::from(d.y)) <= CLICK_SLOP)
        }
        ClickEvent::Keyboard(_) => true,
        ClickEvent::Touch(touch) => !touch.long_press,
    }
}

pub(super) fn follow(origin: &Origin, href: &str, window: &mut Window, cx: &mut App) {
    match link::target(&origin.resource, href) {
        Target::External(url) => open_confirmation(
            Confirmation {
                title: "Open external link?".into(),
                detail: url.clone().into(),
                confirm_label: "Open link".into(),
                destructive: false,
                on_confirm: Rc::new(move |_, cx| {
                    if let Err(error) =
                        desktop_core::external_links::open_external_link(url.clone(), true)
                    {
                        editor::report(error, cx);
                    }
                }),
            },
            window,
            cx,
        ),
        Target::File(path) => {
            // The tab's own host, server and session, as Tauri required.
            let here =
                crate::workspaces::current(cx).map(|workspaces| workspaces.read(cx).view(cx));
            if here.is_some_and(|view| {
                view.host == origin.host
                    && view.server.as_deref() == Some(origin.server.as_str())
                    && view
                        .active_pane
                        .is_some_and(|pane| pane.session_id == origin.session_id)
            }) {
                editor::open_search_match(&path, cx);
            } else {
                editor::report(
                    "Reconnect and select this file's workspace before opening a link.".into(),
                    cx,
                );
            }
        }
        Target::Status(message) => editor::report(message, cx),
    }
}

/// The side padding that centers the 720px column in a pane `width` wide, and never
/// less than the gutter.
fn column_padding(width: Option<Pixels>, gutter: Pixels) -> Pixels {
    width.map_or(gutter, |width| {
        ((width - px(CONTENT_WIDTH)) / 2.).max(gutter)
    })
}

impl Render for PreviewView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (palette, dark) = palette(cx);
        let origin = {
            let document = self.document.read(cx);
            Origin {
                resource: document.resource.clone(),
                host: document.host.clone(),
                server: document.server.clone(),
                session_id: self.session_id.clone(),
            }
        };
        let pressed = self.pressed.clone();
        let press = self.pressed.clone();
        let images = self.images.clone();
        // `clamp(20px, 4vw, 48px)`.
        let gutter = (window.viewport_size().width * 0.04).clamp(px(20.), px(48.));
        // Tauri's column is a centered 720px child of a full-width scroller, so
        // its scrollbar sits at the pane edge. Here the scroller is the text view,
        // so the column is made by padding instead of by a maximum width.
        let side = column_padding(self.pane_width.get(), gutter);
        let measured = self.pane_width.clone();
        div()
            .id("markdown-preview")
            .role(Role::Article)
            .aria_label(SharedString::from(format!("{} preview", self.title)))
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            // A narrow window stacks the split, so the preview sits below.
            .map(|this| {
                if narrower_than(NARROW_TERMINAL, window) {
                    this.border_t_1()
                } else {
                    this.border_l_1()
                }
            })
            .border_color(palette.chrome_hairline)
            .bg(palette.chrome_bg)
            .text_color(palette.chrome_ink)
            .text_size(px(FONT_SIZE))
            .line_height(relative(LINE_HEIGHT))
            .flex()
            .justify_center()
            // Captured: Kit's text view takes the press itself.
            .capture_any_mouse_down(move |event, _, _| {
                if event.button == MouseButton::Left {
                    *press.lock().expect("the press is never poisoned") = Some(event.position);
                }
            })
            .child(
                canvas(
                    move |bounds: Bounds<Pixels>, window, _| {
                        if measured.replace(Some(bounds.size.width)) != Some(bounds.size.width) {
                            window.request_animation_frame();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(
                TextView::new(&self.text)
                    .style(style(&palette, dark))
                    .selectable(true)
                    // Kit's scrolling text view pads outside its list, so text clips at the
                    // padding where `.markdown-preview` scrolls it to the pane's edges. Our own
                    // scroller would fix that, but lays out every block on every frame.
                    .scrollable(true)
                    // Tauri's preview had no syntax colors.
                    .code_block_highlighter(|_| Vec::new())
                    // Kit labels a block with its language unless it has actions of its
                    // own; Tauri's preview has no label.
                    .code_block_actions(|_, _, _| div())
                    .image_source(move |uri| image_source(&images, uri))
                    .on_link_click(move |href: &SharedString, event: &ClickEvent, window, cx| {
                        let pressed = *pressed.lock().expect("the press is never poisoned");
                        if is_click(event, pressed) {
                            follow(&origin, href, window, cx);
                        }
                    })
                    .w_full()
                    .h_full()
                    .pt(px(48.))
                    .pb(px(80.))
                    .px(side),
            )
    }
}

/// The preview of `document`, kept for as long as it is drawn.
pub fn surface(
    record: &AppTabRecord,
    document: &Entity<Document>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let view = window.use_keyed_state(("markdown-preview", document.entity_id()), cx, |_, cx| {
        PreviewView::new(record, document.clone(), cx)
    });
    // The tab record is replaced as the tab is renamed or moved.
    view.update(cx, |view, _| {
        view.title = record.title.clone().into();
        view.session_id = record.session_id.clone();
    });
    view.into_any_element()
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, path::Path};

    use gpui_kit::{TestAppContext, VisualTestContext};

    use super::*;
    use crate::{
        app::{self, Core},
        editor::{
            FileNotice, Root,
            autosave::{LineEnding, SaveRequest},
            document::Live,
            io::{FileIo, FileScope, Opened, Reply, TextFile},
        },
    };

    const PIXEL: &str = "iVBORw0KGgo=";

    #[test]
    fn a_blocked_image_leaves_its_alt_text_and_an_embedded_one_stays() {
        let embedded = "![dot](data:image/png;base64,iVBORw0KGgo=)";
        let text = format!(
            "![remote](https://example.com/a.png) and ![](x.png) then {embedded}\n```\n![in code](u)\n```\n"
        );
        assert_eq!(
            show_blocked_images(&text),
            format!("`remote` and  then {embedded}\n```\n![in code](u)\n```\n")
        );
        // A badge inside a link keeps the link; inline code and a longer or other-marker
        // fence are not images.
        let more = "[![build](b.svg)](ci)\n`![syntax](u)`\n~~~\n```\n![a](u)\n~~~\n![b](u)\n";
        assert_eq!(
            show_blocked_images(more),
            "[`build`](ci)\n`![syntax](u)`\n~~~\n```\n![a](u)\n~~~\n`b`\n"
        );
    }

    #[test]
    fn headings_follow_the_stylesheet_scale() {
        assert_eq!(
            (1..=3).map(heading_scale).collect::<Vec<_>>(),
            [2., 1.55, 1.25]
        );
        assert!(heading_scale(6) < heading_scale(5) && heading_scale(5) < heading_scale(4));
    }

    #[test]
    fn the_column_is_centered_by_padding_and_never_tighter_than_the_gutter() {
        let gutter = px(48.);
        assert_eq!(column_padding(None, gutter), gutter);
        assert_eq!(column_padding(Some(px(1000.)), gutter), px(140.));
        assert_eq!(column_padding(Some(px(800.)), gutter), gutter);
        assert_eq!(column_padding(Some(px(150.)), gutter), gutter);
    }

    #[test]
    fn only_embedded_raster_images_are_drawn() {
        for src in [
            format!("data:image/png;base64,{PIXEL}"),
            "data:image/png;base64,iVBORw0KGgo".to_owned(),
            format!("data:image/JPEG;base64,{PIXEL}"),
            format!("data:image/jpg;base64,{PIXEL}"),
            format!("data:image/gif;base64,{PIXEL}"),
            format!("data:image/webp;base64,{PIXEL}"),
        ] {
            assert!(embedded_image(&src).is_some(), "{src}");
        }
        for src in [
            "https://tracker.invalid/pixel.png",
            "http://tracker.invalid/pixel.png",
            "file:///etc/passwd",
            "/etc/passwd",
            "pixel.png",
            "data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=",
            "data:image/svg+xml,%3Csvg%3E%3C/svg%3E",
            "data:text/html;base64,PGI+",
            // A payload that stops being base64 can smuggle a second URL.
            "data:image/png;base64,AAAA(\"),url(\"https://example.invalid/x\")/*)",
            "data:image/png;base64,AAAA\"),url(\"https://tracker.invalid/pixel.png",
            "data:image/png;base64,AAAA\\evil",
            "data:image/png;base64,AAAA&#34;),url(https://tracker.invalid/pixel.png)",
            "data:image/png;base64,AAAA\n",
            "data:image/png;base64,***",
        ] {
            assert!(embedded_image(src).is_none(), "{src}");
        }
    }

    #[test]
    fn the_image_source_draws_embedded_rasters_and_blocks_everything_else() {
        let images = Images::default();
        let source = |src: &str| image_source(&images, &SharedUri::from(src.to_owned()));
        assert!(matches!(
            source(&format!("data:image/png;base64,{PIXEL}")),
            ImageSource::Image(_)
        ));
        for src in [
            "https://tracker.invalid/a.png",
            "file:///etc/passwd",
            "data:image/svg+xml,%3Csvg%3E",
        ] {
            // A custom source never reaches GPUI's HTTP or file loader.
            assert!(matches!(source(src), ImageSource::Custom(_)), "{src}");
        }
    }

    fn click(button: MouseButton, up: Point<Pixels>) -> ClickEvent {
        let at = |position| gpui_kit::MouseDownEvent {
            button,
            position,
            modifiers: Default::default(),
            click_count: 1,
            first_mouse: false,
        };
        ClickEvent::Mouse(gpui_kit::MouseClickEvent {
            down: at(up),
            up: gpui_kit::MouseUpEvent {
                button,
                position: up,
                modifiers: Default::default(),
                click_count: 1,
            },
        })
    }

    #[test]
    fn only_a_left_press_and_release_in_one_place_follows_a_link() {
        let pressed = Some(gpui_kit::point(px(10.), px(10.)));
        let left = |x| click(MouseButton::Left, gpui_kit::point(px(x), px(10.)));
        assert!(is_click(&left(12.), pressed));
        assert!(!is_click(&left(60.), pressed), "a drag ended on the link");
        assert!(
            !is_click(&left(12.), None),
            "the press began outside the preview"
        );
        for button in [MouseButton::Right, MouseButton::Middle] {
            assert!(!is_click(
                &click(button, gpui_kit::point(px(10.), px(10.))),
                pressed
            ));
        }
    }

    #[test]
    fn a_selection_holds_a_refresh_until_it_is_gone() {
        assert!(holds(true, ""));
        assert!(holds(false, "word"));
        assert!(!holds(false, ""));
    }

    struct Disk(RefCell<(String, usize)>);

    struct Io(Rc<Disk>);

    impl FileIo for Io {
        fn read(&self, _: &FileScope, _: &Root, _: &str) -> Reply<Opened> {
            let (content, generation) = self.0.0.borrow().clone();
            Reply::ready(Ok(Opened::Text(TextFile {
                size_bytes: content.len() as u64,
                content,
                generation: generation.to_string(),
                line_ending: LineEnding::Lf,
            })))
        }

        fn write(&self, _: &FileScope, _: &Root, _: &str, _: &SaveRequest) -> Reply<String> {
            Reply::ready(Ok("9".into()))
        }
    }

    struct Fixture {
        disk: Rc<Disk>,
        document: Entity<Document>,
        view: Entity<PreviewView>,
    }

    fn open<'a>(source: &str, cx: &'a mut TestAppContext) -> (Fixture, &'a mut VisualTestContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            app::init(Core::at(Path::new("/nonexistent/app-state.json")), cx);
        });
        let disk = Rc::new(Disk(RefCell::new((source.to_owned(), 1))));
        let live = Rc::new(Live {
            scope: Box::new(|_| {
                Some(FileScope {
                    client_id: "client".into(),
                    profile_id: "local".into(),
                    server_identity: "server-a".into(),
                    terminal_epoch: 1,
                })
            }),
            can_write: Box::new(|_| true),
        });
        let io: Rc<dyn FileIo> = Rc::new(Io(disk.clone()));
        let document = cx.new(|cx| {
            Document::new(
                "a".into(),
                "local".into(),
                "server-a".into(),
                "/repo/docs/note.md".into(),
                Root {
                    path: "/repo".into(),
                    token: "root".into(),
                },
                io,
                live,
                cx,
            )
        });
        let (view, cx) = cx.add_window_view({
            let document = document.clone();
            move |_, cx| {
                let record = crate::workspaces::model::fixtures::tab("a", "$1", 0);
                PreviewView::new(&record, document, cx)
            }
        });
        cx.run_until_parked();
        (
            Fixture {
                disk,
                document,
                view,
            },
            cx,
        )
    }

    impl Fixture {
        fn rendered(&self, cx: &mut VisualTestContext) -> String {
            cx.update(|_, cx| {
                self.view
                    .read(cx)
                    .text
                    .read(cx)
                    .rendered_text()
                    .as_str()
                    .to_owned()
            })
        }

        /// The file changed on disk, as an agent rewriting it would.
        fn rewrite(&self, source: &str, cx: &mut VisualTestContext) {
            let generation = {
                let mut disk = self.disk.0.borrow_mut();
                *disk = (source.to_owned(), disk.1 + 1);
                disk.1.to_string()
            };
            cx.update(|_, cx| {
                self.document.update(cx, |document, cx| {
                    document.notice(
                        FileNotice::Changed {
                            generation,
                            operation_id: None,
                        },
                        cx,
                    )
                })
            });
            cx.run_until_parked();
        }
    }

    #[gpui_kit::test]
    fn the_preview_shows_the_buffer_and_follows_it(cx: &mut TestAppContext) {
        let (fixture, cx) = open("# Title\n\nfirst", cx);
        assert!(fixture.rendered(cx).contains("Title"));
        assert!(fixture.rendered(cx).contains("first"));
        fixture.rewrite("# Title\n\nsecond", cx);
        assert!(fixture.rendered(cx).contains("second"));
        assert!(!fixture.rendered(cx).contains("first"));
    }

    #[gpui_kit::test]
    fn raw_html_is_not_rendered_as_markup(cx: &mut TestAppContext) {
        let (fixture, cx) = open("<script>alert(1)</script>\n\nplain", cx);
        let rendered = fixture.rendered(cx);
        assert!(rendered.contains("plain"));
        assert!(!rendered.contains("alert"), "{rendered}");
    }

    #[gpui_kit::test]
    fn table_cell_breaks_saved_by_live_are_shown_in_preview(cx: &mut TestAppContext) {
        let (fixture, cx) = open("| a |\n|---|\n| one<br>two |", cx);
        let rendered = fixture.rendered(cx);
        assert!(rendered.contains("one\ntwo"), "{rendered:?}");
        assert!(!rendered.contains("<br>"));
    }

    #[gpui_kit::test]
    fn a_standing_selection_holds_a_change_until_it_is_cleared(cx: &mut TestAppContext) {
        let (fixture, cx) = open("before", cx);
        let text = cx.update(|_, cx| fixture.view.read(cx).text.clone());
        cx.update(|_, cx| text.update(cx, |text, cx| text.select_all(cx)));
        assert!(cx.update(|_, cx| !text.read(cx).selected_text().is_empty()));

        fixture.rewrite("after", cx);
        assert_eq!(
            fixture.rendered(cx).trim(),
            "before",
            "a refresh moved a live selection"
        );

        cx.update(|_, cx| text.update(cx, |text, cx| text.clear_selection(cx)));
        cx.run_until_parked();
        assert_eq!(
            fixture.rendered(cx).trim(),
            "after",
            "the deferred update never arrived"
        );
    }

    #[gpui_kit::test]
    fn going_back_to_what_is_shown_drops_a_held_change(cx: &mut TestAppContext) {
        let (fixture, cx) = open("before", cx);
        let text = cx.update(|_, cx| fixture.view.read(cx).text.clone());
        cx.update(|_, cx| text.update(cx, |text, cx| text.select_all(cx)));
        fixture.rewrite("after", cx);
        fixture.rewrite("before", cx);
        cx.update(|_, cx| text.update(cx, |text, cx| text.clear_selection(cx)));
        cx.run_until_parked();
        assert_eq!(fixture.rendered(cx).trim(), "before");
    }
}
