//! When a pane's screen first reached the display, for what must not queue its
//! own requests ahead of it. Port of `panePaintGate.ts`: a window switch is one
//! link, and the Explorer's root probe (Phase 5) waits behind the pane so a slow
//! link carries the terminal first.
//!
//! A timeout, never a barrier: a pane that never paints must not also cost the
//! user their file tree.

use std::collections::VecDeque;
use std::time::Duration;

use futures::channel::oneshot;
use futures::future::{self, Either};
use gpui_kit::{App, BackgroundExecutor, Global};

pub const PANE_PAINT_TIMEOUT: Duration = Duration::from_millis(600);

/// Bounded because pane ids are not. Eviction only forgets that a pane painted,
/// which is the safe direction: the next wait on it resolves immediately.
const MAX_TRACKED_PANES: usize = 64;

#[derive(Default)]
struct PanedPaint {
    key: String,
    painted: bool,
    waiters: Vec<oneshot::Sender<()>>,
}

/// Keyed by host scope and pane id together: `%1` exists on every tmux server.
#[derive(Default)]
pub struct PanePaintGate {
    panes: VecDeque<PanedPaint>,
}

impl Global for PanePaintGate {}

impl PanePaintGate {
    fn index(&self, key: &str) -> Option<usize> {
        self.panes.iter().position(|pane| pane.key == key)
    }

    fn entry(&mut self, key: &str) -> &mut PanedPaint {
        let index = match self.index(key) {
            Some(index) => index,
            None => {
                self.panes.push_back(PanedPaint {
                    key: key.to_owned(),
                    ..PanedPaint::default()
                });
                while self.panes.len() > MAX_TRACKED_PANES {
                    // Dropping the entry drops its senders, which releases the
                    // waiters: a promise nobody will settle is the one failure
                    // this module must not have.
                    self.panes.pop_front();
                }
                self.panes.len() - 1
            }
        };
        &mut self.panes[index]
    }

    /// A reveal went out for this pane; its screen is on the way.
    pub fn arm(&mut self, key: &str) {
        self.entry(key).painted = false;
    }

    /// This pane's content reached the display. Releases whatever waited.
    pub fn note_painted(&mut self, key: &str) {
        let pane = self.entry(key);
        pane.painted = true;
        for waiter in std::mem::take(&mut pane.waiters) {
            let _ = waiter.send(());
        }
    }

    fn waiter(&mut self, key: &str) -> Option<oneshot::Receiver<()>> {
        let index = self.index(key)?;
        let pane = &mut self.panes[index];
        if pane.painted {
            return None;
        }
        let (sender, receiver) = oneshot::channel();
        pane.waiters.push(sender);
        Some(receiver)
    }
}

/// Resolves on the first paint after the current arm, or on the timeout. An
/// unknown or already painted pane resolves at once.
pub fn wait_for_pane_paint(
    key: &str,
    timeout: Duration,
    cx: &mut App,
) -> impl Future<Output = ()> + use<> {
    let waiter = cx.default_global::<PanePaintGate>().waiter(key);
    let timer = cx.background_executor().clone();
    wait(waiter, timer, timeout)
}

async fn wait(waiter: Option<oneshot::Receiver<()>>, timer: BackgroundExecutor, timeout: Duration) {
    let Some(waiter) = waiter else {
        return;
    };
    let timeout = timer.timer(timeout);
    match future::select(waiter, timeout).await {
        Either::Left(_) | Either::Right(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui_kit::TestAppContext;

    use super::*;

    fn spawn_wait(cx: &mut TestAppContext, key: &'static str, timeout: Duration) -> Rc<Cell<bool>> {
        let resolved = Rc::new(Cell::new(false));
        let done = resolved.clone();
        cx.update(|cx| {
            let wait = wait_for_pane_paint(key, timeout, cx);
            cx.spawn(async move |_| {
                wait.await;
                done.set(true);
            })
            .detach();
        });
        resolved
    }

    #[gpui_kit::test]
    fn an_unarmed_or_painted_pane_resolves_at_once(cx: &mut TestAppContext) {
        let unarmed = spawn_wait(cx, "host\0%1", PANE_PAINT_TIMEOUT);
        cx.run_until_parked();
        assert!(unarmed.get());
        cx.update(|cx| {
            let gate = cx.default_global::<PanePaintGate>();
            gate.arm("host\0%2");
            gate.note_painted("host\0%2");
        });
        let painted = spawn_wait(cx, "host\0%2", PANE_PAINT_TIMEOUT);
        cx.run_until_parked();
        assert!(painted.get());
    }

    #[gpui_kit::test]
    fn an_armed_pane_waits_for_its_paint_and_only_its_own(cx: &mut TestAppContext) {
        cx.update(|cx| cx.default_global::<PanePaintGate>().arm("a\0%1"));
        cx.update(|cx| cx.default_global::<PanePaintGate>().arm("b\0%1"));
        let waiting = spawn_wait(cx, "a\0%1", PANE_PAINT_TIMEOUT);
        cx.run_until_parked();
        assert!(!waiting.get());
        cx.update(|cx| cx.default_global::<PanePaintGate>().note_painted("b\0%1"));
        cx.run_until_parked();
        assert!(!waiting.get(), "another host's pane");
        cx.update(|cx| cx.default_global::<PanePaintGate>().note_painted("a\0%1"));
        cx.run_until_parked();
        assert!(waiting.get());
    }

    #[gpui_kit::test]
    fn a_pane_that_never_paints_resolves_at_the_timeout(cx: &mut TestAppContext) {
        cx.update(|cx| cx.default_global::<PanePaintGate>().arm("a\0%1"));
        let waiting = spawn_wait(cx, "a\0%1", PANE_PAINT_TIMEOUT);
        cx.run_until_parked();
        assert!(!waiting.get());
        cx.executor().advance_clock(PANE_PAINT_TIMEOUT);
        cx.run_until_parked();
        assert!(waiting.get());
    }

    #[gpui_kit::test]
    fn an_evicted_pane_releases_its_waiters(cx: &mut TestAppContext) {
        cx.update(|cx| cx.default_global::<PanePaintGate>().arm("oldest"));
        let waiting = spawn_wait(cx, "oldest", PANE_PAINT_TIMEOUT);
        cx.update(|cx| {
            for index in 0..MAX_TRACKED_PANES {
                cx.default_global::<PanePaintGate>()
                    .arm(&format!("p{index}"));
            }
        });
        cx.run_until_parked();
        assert!(waiting.get());
    }
}
