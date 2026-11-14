//! How big a tmux client to ask for, and the one request that can resize the
//! user's real windows. Port of `clientSize.ts` and `app/useClientResize.ts`.
//!
//! tmux applies a client's size to every window it sees, including ones other
//! terminals are attached to, so the size comes from the surface's own pixel
//! box and never from a pane's share of the topology, and a size above the sane
//! bound is reported, not sent.

use std::time::{Duration, Instant};

use terminal_engine::GridSize;
use tmux_control::Pane;

use super::cell_metrics::{BoxChrome, PixelBox, cells_for_box};

pub const MIN_CLIENT_CELLS: usize = 2;
pub const MAX_CLIENT_CELLS: usize = 500;

/// Coalesces a drag of the window edge into one request.
pub const CLIENT_RESIZE_DEBOUNCE: Duration = Duration::from_millis(60);
pub const CLIENT_RESIZE_RETRY: Duration = Duration::from_millis(500);
pub const CLIENT_RESIZE_RETRIES: u32 = 4;
/// The least time between two requests that take the size back from another
/// terminal. This is a rate, not a budget: two clients that each re-assert on
/// seeing the other's size would resize the user's real windows for as long as
/// both are attached.
pub const CLIENT_RESIZE_TAKE_INTERVAL: Duration = Duration::from_millis(2_000);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientSizeDecision {
    Size(GridSize),
    None,
    /// A defect in this computation, never clamped into range and sent.
    Refused(String),
}

pub fn client_size_for_surface(
    surface: Option<PixelBox>,
    cell: Option<PixelBox>,
    chrome: BoxChrome,
) -> ClientSizeDecision {
    let (Some(surface), Some(cell)) = (surface, cell) else {
        return ClientSizeDecision::None;
    };
    if !surface.width.is_finite() || !surface.height.is_finite() {
        return ClientSizeDecision::None;
    }
    let Some(GridSize { columns, rows }) = cells_for_box(surface, cell, chrome) else {
        return ClientSizeDecision::None;
    };
    if columns > MAX_CLIENT_CELLS || rows > MAX_CLIENT_CELLS {
        return ClientSizeDecision::Refused(format!(
            "Refusing to resize the tmux client to {columns}x{rows}: above the \
             {MAX_CLIENT_CELLS} cell bound for a {}x{} pixel surface.",
            surface.width.round(),
            surface.height.round()
        ));
    }
    if columns < MIN_CLIENT_CELLS || rows < MIN_CLIENT_CELLS {
        return ClientSizeDecision::None;
    }
    ClientSizeDecision::Size(GridSize { columns, rows })
}

/// The grid tmux settled on for a window, read off the snapshot: what the
/// surface wants, and what it got, come apart whenever another client wins.
pub fn window_cell_size(panes: &[Pane], window_id: &str) -> Option<GridSize> {
    let (mut columns, mut rows) = (0, 0);
    for pane in panes.iter().filter(|pane| pane.window_id == window_id) {
        columns = columns.max(usize::from(pane.left) + usize::from(pane.width));
        rows = rows.max(usize::from(pane.top) + usize::from(pane.height));
    }
    (columns > 0 && rows > 0).then_some(GridSize { columns, rows })
}

/// What the surface needs for a request, read fresh each time.
#[derive(Clone, Debug, PartialEq)]
pub struct ResizeContext {
    pub client_id: Option<String>,
    pub session_id: Option<String>,
    pub window_id: Option<String>,
    pub can_mutate: bool,
    pub surface: Option<PixelBox>,
    pub cell: Option<PixelBox>,
    pub chrome: BoxChrome,
    /// What tmux actually has for this window.
    pub actual: Option<GridSize>,
    /// False under an app tab: nothing on screen to size for.
    pub terminal_visible: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResizeRequest {
    pub client_id: String,
    pub session_id: String,
    pub window_id: String,
    pub size: GridSize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResizeOutcome {
    Send(ResizeRequest),
    /// Once per distinct problem, so a window held past the bound stays quiet.
    Report(String),
    Nothing,
}

/// A request that failed: retry it, tell the user, or let it go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetryDecision {
    RetryAfter(Duration),
    Report(String),
    Nothing,
}

/// The state behind the resize request, separate from the timers that drive it.
#[derive(Debug, Default)]
pub struct ClientResize {
    last_requested: Option<ResizeRequest>,
    last_reported: Option<String>,
    attempt: u32,
    last_sent_at: Option<Instant>,
}

impl ClientResize {
    /// A new bridge is a new tmux client: what the last one was told is moot.
    pub fn reset_for_new_client(&mut self) {
        *self = Self::default();
    }

    pub fn evaluate(&mut self, context: &ResizeContext, now: Instant) -> ResizeOutcome {
        let (Some(client_id), Some(session_id), Some(window_id)) = (
            context.client_id.as_ref(),
            context.session_id.as_ref(),
            context.window_id.as_ref(),
        ) else {
            return ResizeOutcome::Nothing;
        };
        if !context.can_mutate || !context.terminal_visible {
            return ResizeOutcome::Nothing;
        }
        let size = match client_size_for_surface(context.surface, context.cell, context.chrome) {
            ClientSizeDecision::None => return ResizeOutcome::Nothing,
            ClientSizeDecision::Refused(reason) => {
                if self.last_reported.as_deref() == Some(reason.as_str()) {
                    return ResizeOutcome::Nothing;
                }
                self.last_reported = Some(reason.clone());
                return ResizeOutcome::Report(reason);
            }
            ClientSizeDecision::Size(size) => size,
        };
        let request = ResizeRequest {
            client_id: client_id.clone(),
            session_id: session_id.clone(),
            window_id: window_id.clone(),
            size,
        };
        // Every trigger recomputes; only a different answer reaches tmux. Three
        // identical `refresh-client -C` requests cost 15 topology events on a
        // real link.
        if self.last_requested.as_ref() == Some(&request) {
            return ResizeOutcome::Nothing;
        }
        self.last_requested = Some(request.clone());
        self.last_reported = None;
        self.last_sent_at = Some(now);
        ResizeOutcome::Send(request)
    }

    /// Reset on a landed request only: the retry budget is per failure run.
    pub fn landed(&mut self) {
        self.attempt = 0;
    }

    pub fn failed(
        &mut self,
        request: &ResizeRequest,
        context: &ResizeContext,
        error: String,
    ) -> RetryDecision {
        if self.last_requested.as_ref() == Some(request) {
            self.last_requested = None;
        }
        if context.client_id.as_deref() != Some(request.client_id.as_str())
            || context.session_id.as_deref() != Some(request.session_id.as_str())
            || context.window_id.as_deref() != Some(request.window_id.as_str())
        {
            return RetryDecision::Nothing;
        }
        if self.attempt < CLIENT_RESIZE_RETRIES {
            self.attempt += 1;
            return RetryDecision::RetryAfter(CLIENT_RESIZE_RETRY);
        }
        if self.last_reported.as_deref() == Some(error.as_str()) {
            return RetryDecision::Nothing;
        }
        self.last_reported = Some(error.clone());
        RetryDecision::Report(error)
    }

    /// Whether a keystroke or a press here should take the client size back:
    /// tmux's answer is not this surface's and the interval has passed.
    pub fn take_due(&self, context: &ResizeContext, now: Instant) -> bool {
        if context.client_id.is_none() || !context.can_mutate || !context.terminal_visible {
            return false;
        }
        let Some(actual) = context.actual else {
            return false;
        };
        let ClientSizeDecision::Size(size) =
            client_size_for_surface(context.surface, context.cell, context.chrome)
        else {
            return false;
        };
        size != actual
            && self.last_sent_at.is_none_or(|sent| {
                now.saturating_duration_since(sent) >= CLIENT_RESIZE_TAKE_INTERVAL
            })
    }

    /// Lets the identical computation through the dedupe once.
    pub fn take(&mut self) {
        self.last_requested = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHROME: BoxChrome = BoxChrome {
        horizontal: 12.,
        vertical: 12.,
        scrollbar: 14.,
    };
    const CELL: PixelBox = PixelBox {
        width: 8.,
        height: 17.,
    };
    const SURFACE: PixelBox = PixelBox {
        width: 1000.,
        height: 800.,
    };

    fn grid(columns: usize, rows: usize) -> GridSize {
        GridSize { columns, rows }
    }

    fn pane(window_id: &str, left: u16, top: u16, width: u16, height: u16) -> Pane {
        Pane {
            id: "%1".into(),
            session_id: "$1".into(),
            window_id: window_id.into(),
            index: 0,
            active: false,
            width,
            height,
            left,
            top,
            current_path: "/".into(),
            current_command: "bash".into(),
            pane_pid: 1,
            start_command: String::new(),
            title: String::new(),
        }
    }

    fn context() -> ResizeContext {
        ResizeContext {
            client_id: Some("c".into()),
            session_id: Some("$1".into()),
            window_id: Some("@1".into()),
            can_mutate: true,
            surface: Some(SURFACE),
            cell: Some(CELL),
            chrome: CHROME,
            actual: None,
            terminal_visible: true,
        }
    }

    #[test]
    fn the_surface_decides_the_size_whatever_a_pane_looks_like() {
        // P12-U006: a pane's share of the topology must not enter this number.
        assert_eq!(
            client_size_for_surface(Some(SURFACE), Some(CELL), CHROME),
            ClientSizeDecision::Size(grid(121, 46))
        );
    }

    #[test]
    fn nothing_to_measure_is_no_answer() {
        assert_eq!(
            client_size_for_surface(None, Some(CELL), CHROME),
            ClientSizeDecision::None
        );
        assert_eq!(
            client_size_for_surface(Some(SURFACE), None, CHROME),
            ClientSizeDecision::None
        );
        let nan = PixelBox {
            width: f32::NAN,
            height: 10.,
        };
        assert_eq!(
            client_size_for_surface(Some(nan), Some(CELL), CHROME),
            ClientSizeDecision::None
        );
    }

    #[test]
    fn a_window_dragged_tiny_says_nothing_and_a_size_past_the_bound_is_refused() {
        let tiny = PixelBox {
            width: 40.,
            height: 800.,
        };
        assert_eq!(
            client_size_for_surface(Some(tiny), Some(CELL), CHROME),
            ClientSizeDecision::None
        );
        let huge = PixelBox {
            width: 8. * 501. + 26.,
            height: 800.,
        };
        assert!(matches!(
            client_size_for_surface(Some(huge), Some(CELL), CHROME),
            ClientSizeDecision::Refused(reason) if reason.contains("501x46")
        ));
    }

    #[test]
    fn a_bound_size_is_sent() {
        let at_bound = PixelBox {
            width: 8. * 500. + 26.,
            height: 800.,
        };
        assert_eq!(
            client_size_for_surface(Some(at_bound), Some(CELL), CHROME),
            ClientSizeDecision::Size(grid(500, 46))
        );
    }

    #[test]
    fn the_windows_cells_are_the_bottom_right_panes_edge() {
        let panes = [
            pane("@1", 0, 0, 50, 24),
            pane("@1", 51, 0, 49, 24),
            pane("@2", 0, 0, 200, 90),
        ];
        assert_eq!(window_cell_size(&panes, "@1"), Some(grid(100, 24)));
        assert_eq!(window_cell_size(&panes, "@9"), None);
    }

    #[test]
    fn sends_once_per_answer_and_again_when_the_answer_changes() {
        let mut resize = ClientResize::default();
        let now = Instant::now();
        let ResizeOutcome::Send(request) = resize.evaluate(&context(), now) else {
            panic!("the first answer goes out");
        };
        assert_eq!(request.size, grid(121, 46));
        assert_eq!(resize.evaluate(&context(), now), ResizeOutcome::Nothing);
        let mut wider = context();
        wider.surface = Some(PixelBox {
            width: 1100.,
            height: 800.,
        });
        assert!(matches!(
            resize.evaluate(&wider, now),
            ResizeOutcome::Send(request) if request.size.columns == 134
        ));
    }

    #[test]
    fn a_missing_identity_or_a_read_only_host_asks_for_nothing() {
        let mut resize = ClientResize::default();
        let now = Instant::now();
        for missing in [
            |c: &mut ResizeContext| c.client_id = None,
            |c: &mut ResizeContext| c.session_id = None,
            |c: &mut ResizeContext| c.window_id = None,
            |c: &mut ResizeContext| c.can_mutate = false,
        ] {
            let mut context = context();
            missing(&mut context);
            assert_eq!(resize.evaluate(&context, now), ResizeOutcome::Nothing);
        }
    }

    #[test]
    fn a_refusal_is_reported_once_per_distinct_problem() {
        let mut resize = ClientResize::default();
        let now = Instant::now();
        let mut huge = context();
        huge.surface = Some(PixelBox {
            width: 8. * 600. + 26.,
            height: 800.,
        });
        assert!(matches!(
            resize.evaluate(&huge, now),
            ResizeOutcome::Report(_)
        ));
        assert_eq!(resize.evaluate(&huge, now), ResizeOutcome::Nothing);
    }

    #[test]
    fn a_failed_request_retries_within_its_budget_then_reports_once() {
        let mut resize = ClientResize::default();
        let now = Instant::now();
        let ResizeOutcome::Send(request) = resize.evaluate(&context(), now) else {
            panic!();
        };
        for _ in 0..CLIENT_RESIZE_RETRIES {
            assert_eq!(
                resize.failed(&request, &context(), "down".into()),
                RetryDecision::RetryAfter(CLIENT_RESIZE_RETRY)
            );
            // A failure forgets the record, so the retry is a fresh send.
            assert!(matches!(
                resize.evaluate(&context(), now),
                ResizeOutcome::Send(_)
            ));
        }
        assert_eq!(
            resize.failed(&request, &context(), "down".into()),
            RetryDecision::Report("down".into())
        );
    }

    #[test]
    fn a_failure_for_a_replaced_client_is_dropped_and_a_landing_resets_the_budget() {
        let mut resize = ClientResize::default();
        let now = Instant::now();
        let ResizeOutcome::Send(request) = resize.evaluate(&context(), now) else {
            panic!();
        };
        let mut moved_on = context();
        moved_on.client_id = Some("d".into());
        assert_eq!(
            resize.failed(&request, &moved_on, "gone".into()),
            RetryDecision::Nothing
        );
        resize.landed();
        assert_eq!(resize.attempt, 0);
    }

    #[test]
    fn a_take_waits_for_the_interval_and_only_when_tmux_disagrees() {
        let mut resize = ClientResize::default();
        let start = Instant::now();
        let mut other_won = context();
        other_won.actual = Some(grid(90, 30));
        assert!(matches!(
            resize.evaluate(&other_won, start),
            ResizeOutcome::Send(_)
        ));
        assert!(!resize.take_due(&other_won, start));
        let later = start + CLIENT_RESIZE_TAKE_INTERVAL;
        assert!(resize.take_due(&other_won, later));
        let mut agreed = context();
        agreed.actual = Some(grid(121, 46));
        assert!(!resize.take_due(&agreed, later));
        resize.take();
        assert!(matches!(
            resize.evaluate(&other_won, later),
            ResizeOutcome::Send(_)
        ));
    }

    #[test]
    fn a_new_client_forgets_what_the_last_one_was_told() {
        let mut resize = ClientResize::default();
        let now = Instant::now();
        resize.evaluate(&context(), now);
        resize.reset_for_new_client();
        assert!(matches!(
            resize.evaluate(&context(), now),
            ResizeOutcome::Send(_)
        ));
    }

    #[test]
    fn nothing_is_sent_or_taken_under_an_app_tab() {
        let mut resize = ClientResize::default();
        let hidden = ResizeContext {
            terminal_visible: false,
            actual: Some(grid(10, 10)),
            ..context()
        };
        let now = Instant::now();
        assert_eq!(resize.evaluate(&hidden, now), ResizeOutcome::Nothing);
        assert!(!resize.take_due(&hidden, now));
    }
}
