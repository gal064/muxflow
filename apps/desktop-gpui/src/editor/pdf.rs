//! A PDF tab's pages: parsed and rasterized by hayro off the main thread, and
//! held only while they are on screen or next to it.

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

use gpui_kit::{
    AnyElement, AppContext, Context, InteractiveElement, IntoElement, ListAlignment, ListState,
    ParentElement, Render, RenderImage, Role, SharedString, StatefulInteractiveElement, Styled,
    Task, Window, div, img, list, prelude::FluentBuilder, px,
};
use hayro::{
    PixmapSettings, RenderCache, RenderSettings,
    hayro_interpret::InterpreterSettings,
    hayro_syntax::{LoadPdfError, Pdf},
    render,
    vello_cpu::color::palette::css::WHITE,
};

use crate::theme::{
    self,
    metrics::{TYPE_SECONDARY, ui},
};

/// The widest a page is drawn, in logical pixels.
const PAGE_WIDTH: f32 = 900.;
/// The most pixels one page is rasterized into, so a freak page size cannot
/// allocate without bound.
const MAX_PAGE_PIXELS: f32 = 16_000_000.;
/// The longest side a page is rasterized to, within every GPU's texture limit.
const MAX_PAGE_SIDE: f32 = 8192.;
/// The page shapes drawn, width over height; a hostile box is clamped to them.
const PAGE_RATIOS: (f32, f32) = (0.02, 50.);
/// Pages kept rasterized either side of the visible ones.
const KEEP_AROUND: usize = 2;

pub struct PdfView {
    bytes: Arc<[u8]>,
    /// Each page's width over its height, once the document has parsed.
    pages: Option<Result<Vec<f32>, String>>,
    list: ListState,
    rendered: HashMap<usize, Arc<RenderImage>>,
    pending: HashMap<usize, Task<()>>,
    failed: HashSet<usize>,
    _parse: Task<()>,
}

impl PdfView {
    pub fn new(bytes: Arc<[u8]>, cx: &mut Context<Self>) -> Self {
        let list = ListState::new(0, ListAlignment::Top, px(PAGE_WIDTH));
        let view = cx.weak_entity();
        list.set_scroll_handler(move |event, window, cx| {
            view.update(cx, |this, cx| {
                this.keep(event.visible_range.clone(), window, cx)
            })
            .ok();
        });
        cx.on_release(|this, cx| {
            // Deferred so the window being updated, if any, is back in the
            // app's set and gives up its textures too.
            let images: Vec<_> = this.rendered.drain().map(|(_, image)| image).collect();
            cx.defer(move |cx| {
                for image in images {
                    cx.drop_image(image, None);
                }
            });
        })
        .detach();
        let parse = cx.background_spawn({
            let bytes = bytes.clone();
            async move { guarded(|| parse(&bytes)).unwrap_or_else(|| Err(UNREADABLE.into())) }
        });
        let parse = cx.spawn(async move |this, cx| {
            let pages = parse.await;
            this.update(cx, |this, cx| {
                if let Ok(pages) = &pages {
                    this.list.reset(pages.len());
                }
                this.pages = Some(pages);
                cx.notify();
            })
            .ok();
        });
        Self {
            bytes,
            pages: None,
            list,
            rendered: HashMap::new(),
            pending: HashMap::new(),
            failed: HashSet::new(),
            _parse: parse,
        }
    }

    /// Lets go of every page that is neither visible nor next to it.
    fn keep(&mut self, visible: Range<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let kept = visible.start.saturating_sub(KEEP_AROUND)..visible.end + KEEP_AROUND;
        self.pending.retain(|index, _| kept.contains(index));
        let gone: Vec<usize> = self
            .rendered
            .keys()
            .copied()
            .filter(|index| !kept.contains(index))
            .collect();
        for index in gone {
            if let Some(image) = self.rendered.remove(&index) {
                cx.drop_image(image, Some(window));
            }
        }
    }

    /// One page, drawn when its pixels are ready and asked for when not.
    fn page(&mut self, index: usize, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let palette = theme::palette(cx);
        let ratio = match &self.pages {
            Some(Ok(pages)) => pages.get(index).copied().unwrap_or(1.),
            _ => 1.,
        };
        let image = self.rendered.get(&index).cloned();
        let failed = self.failed.contains(&index);
        if image.is_none() && !failed && !self.pending.contains_key(&index) {
            // The drawn width follows the interface size, so the raster does too.
            let width = (f32::from(ui(PAGE_WIDTH).to_pixels(window.rem_size()))
                * window.scale_factor())
            .round() as u32;
            let bytes = self.bytes.clone();
            let raster =
                cx.background_spawn(async move { guarded(|| rasterize(&bytes, index, width)) });
            let task = cx.spawn(async move |this, cx| {
                let image = raster.await.flatten();
                this.update(cx, |this, cx| {
                    this.pending.remove(&index);
                    match image {
                        Some(image) => {
                            this.rendered.insert(index, Arc::new(image));
                        }
                        None => {
                            this.failed.insert(index);
                        }
                    }
                    cx.notify();
                })
                .ok();
            });
            self.pending.insert(index, task);
        }
        let sheet = div()
            .id(("pdf-page", index))
            .role(Role::Image)
            .aria_label(SharedString::from(format!("Page {}", index + 1)))
            .w_full()
            .max_w(ui(PAGE_WIDTH))
            .aspect_ratio(ratio)
            .bg(gpui_kit::white())
            .border_1()
            .border_color(palette.chrome_border)
            .map(|sheet| match image {
                Some(image) => sheet.child(img(image).size_full()),
                None if failed => sheet
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(ui(TYPE_SECONDARY))
                    .text_color(palette.chrome_dim)
                    .child("This page could not be drawn."),
                None => sheet,
            });
        div()
            .w_full()
            .flex()
            .justify_center()
            .px(ui(16.))
            .py(ui(8.))
            .child(sheet)
            .into_any_element()
    }
}

impl Render for PdfView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx);
        let surface = div()
            .id("pdf-surface")
            .size_full()
            .bg(palette.chrome_raised);
        match &self.pages {
            None => surface.into_any_element(),
            Some(Err(message)) => surface
                .flex()
                .items_center()
                .justify_center()
                .text_size(ui(TYPE_SECONDARY))
                .text_color(palette.chrome_dim)
                .child(SharedString::from(message.clone()))
                .into_any_element(),
            Some(Ok(_)) => {
                let view = cx.weak_entity();
                surface
                    .child(
                        list(self.list.clone(), move |index, window, cx| {
                            view.update(cx, |this, cx| this.page(index, window, cx))
                                .unwrap_or_else(|_| div().into_any_element())
                        })
                        .size_full(),
                    )
                    .into_any_element()
            }
        }
    }
}

const UNREADABLE: &str = "This PDF could not be read.";

/// Runs hayro on untrusted bytes without letting one of its panics take the
/// app down with it.
fn guarded<T>(work: impl FnOnce() -> T) -> Option<T> {
    catch_unwind(AssertUnwindSafe(work)).ok()
}

fn open(bytes: &Arc<[u8]>) -> Result<Pdf, LoadPdfError> {
    Pdf::new(Arc::new(bytes.clone()))
}

/// Each page's width over its height.
fn parse(bytes: &Arc<[u8]>) -> Result<Vec<f32>, String> {
    let pdf = open(bytes).map_err(|error| match error {
        LoadPdfError::Decryption(_) => "This PDF is password-protected.".to_owned(),
        LoadPdfError::Invalid => UNREADABLE.to_owned(),
    })?;
    let pages: Vec<f32> = pdf
        .pages()
        .iter()
        .map(|page| {
            let (width, height) = page.render_dimensions();
            let ratio = width / height;
            if ratio.is_finite() && ratio > 0. {
                ratio.clamp(PAGE_RATIOS.0, PAGE_RATIOS.1)
            } else {
                1.
            }
        })
        .collect();
    if pages.is_empty() {
        return Err("This PDF has no pages.".into());
    }
    Ok(pages)
}

/// One page as BGRA pixels, `width` pixels across.
fn rasterize(bytes: &Arc<[u8]>, index: usize, width: u32) -> Option<RenderImage> {
    let pdf = open(bytes).ok()?;
    let page = pdf.pages().get(index)?;
    let (page_width, page_height) = page.render_dimensions();
    let scale = (width as f32 / page_width)
        .min((MAX_PAGE_PIXELS / (page_width * page_height)).sqrt())
        .min(MAX_PAGE_SIDE / page_width)
        .min(MAX_PAGE_SIDE / page_height);
    if !scale.is_finite() || page_width * scale < 1. || page_height * scale < 1. {
        return None;
    }
    let pixmap = render(
        page,
        &RenderCache::new(),
        &InterpreterSettings::default(),
        &RenderSettings::default(),
        &PixmapSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: WHITE,
        },
    );
    let (width, height) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
    let mut pixels = pixmap.data_as_u8_slice().to_vec();
    // GPUI draws BGRA; the white background leaves every pixel opaque, so the
    // premultiplied channels are the plain ones.
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(width, height, pixels)?;
    Some(RenderImage::new([image::Frame::new(buffer)]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-page PDF, built with a valid cross-reference table.
    fn one_page(width: u32, height: u32) -> Arc<[u8]> {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] /Contents 4 0 R >>"
            ),
            "<< /Length 30 >>\nstream\n0 0 1 rg 10 10 50 50 re f\nendstream".to_owned(),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (number, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend(format!("{} 0 obj\n{object}\nendobj\n", number + 1).bytes());
        }
        let xref = pdf.len();
        pdf.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
        for offset in offsets {
            pdf.extend(format!("{offset:010} 00000 n \n").bytes());
        }
        pdf.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .bytes(),
        );
        pdf.into()
    }

    #[test]
    fn a_pdf_parses_into_its_page_shapes() {
        assert_eq!(parse(&one_page(200, 100)), Ok(vec![2.]));
    }

    #[test]
    fn a_page_rasterizes_to_the_width_asked_for_in_bgra() {
        let image = rasterize(&one_page(200, 100), 0, 400).unwrap();
        let size = image.size(0);
        assert_eq!((size.width.0, size.height.0), (400, 200));
        // The filled square is pure blue, which BGRA stores first.
        let bytes = image.as_bytes(0).unwrap();
        let row = 200 - 40;
        let pixel = &bytes[(row * 400 + 40) * 4..][..4];
        assert_eq!(pixel, [255, 0, 0, 255]);
        assert!(rasterize(&one_page(200, 100), 1, 400).is_none());
    }

    #[test]
    fn a_tall_page_stays_within_the_texture_limit_and_a_freak_box_is_clamped() {
        let tall = one_page(100, 5000);
        let image = rasterize(&tall, 0, 1800).unwrap();
        let size = image.size(0);
        assert!(size.height.0 <= MAX_PAGE_SIDE as i32, "{size:?}");
        assert_eq!(parse(&tall), Ok(vec![PAGE_RATIOS.0]));
    }

    #[test]
    fn bytes_that_are_not_a_pdf_are_refused_without_a_panic() {
        let garbage: Arc<[u8]> = Arc::from(&b"%PDF-1.7 not really"[..]);
        assert_eq!(
            guarded(|| parse(&garbage)),
            Some(Err(UNREADABLE.to_owned()))
        );
    }
}
