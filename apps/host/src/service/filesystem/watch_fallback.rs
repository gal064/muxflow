use super::*;

/// How long the poller waits between shards while at least one target is on
/// the fallback path. With none, it parks on its signal instead of ticking.
pub(super) const FALLBACK_TICK: Duration = Duration::from_millis(25);
pub(super) const FALLBACK_SCAN_ENTRY_BUDGET: usize = 2_048;
const FALLBACK_SCAN_TIME_BUDGET: Duration = Duration::from_millis(8);

/// Backoff between *completed* unchanged scans of one failed target.
///
/// A directory nothing is writing to costs one scan every [`FALLBACK_BACKOFF_MAX`]
/// rather than a permanent 25 ms loop, and an unchanged scan never publishes a
/// snapshot: the desktop's listing is already that snapshot, and re-sending it
/// made every fallback watch a periodic full-directory payload forever.
const FALLBACK_BACKOFF_BASE: Duration = Duration::from_millis(250);
const FALLBACK_BACKOFF_MAX: Duration = Duration::from_secs(4);

/// Backoff between attempts to put a failed target back on the native watcher.
const NATIVE_RETRY_BASE: Duration = Duration::from_secs(1);
const NATIVE_RETRY_MAX: Duration = Duration::from_secs(30);

pub(super) struct FallbackScan {
    iterator: Option<fs::ReadDir>,
    accumulator: u64,
    last_completed: u64,
}

impl FallbackScan {
    fn new(initial: u64) -> Self {
        Self {
            iterator: None,
            accumulator: 0,
            last_completed: initial,
        }
    }
}

/// One watch target's fallback state.
///
/// `native` is the whole point of the type: a target the native watcher
/// accepted is never scanned, so healthy watches cost nothing at all and only
/// the exact targets that failed pay for polling.
pub(super) struct FallbackTarget {
    native: bool,
    scan: FallbackScan,
    next_scan: Instant,
    scan_backoff: Duration,
    next_native_retry: Instant,
    native_retry_backoff: Duration,
}

impl FallbackTarget {
    pub(super) fn native(initial_fingerprint: u64) -> Self {
        Self {
            native: true,
            scan: FallbackScan::new(initial_fingerprint),
            next_scan: Instant::now(),
            scan_backoff: FALLBACK_BACKOFF_BASE,
            next_native_retry: Instant::now(),
            native_retry_backoff: NATIVE_RETRY_BASE,
        }
    }

    pub(super) fn polling(initial_fingerprint: u64) -> Self {
        let mut target = Self::native(initial_fingerprint);
        target.native = false;
        target.next_native_retry = Instant::now() + NATIVE_RETRY_BASE;
        target
    }

    pub(super) fn is_native(&self) -> bool {
        self.native
    }

    fn restored_to_native(&mut self) {
        self.native = true;
        self.native_retry_backoff = NATIVE_RETRY_BASE;
        self.scan_backoff = FALLBACK_BACKOFF_BASE;
        self.scan.iterator = None;
    }

    fn native_retry_failed(&mut self) {
        self.native = false;
        self.native_retry_backoff = (self.native_retry_backoff * 2).min(NATIVE_RETRY_MAX);
        self.next_native_retry = Instant::now() + self.native_retry_backoff;
    }

    /// Moves a target the native watcher has stopped covering onto polling.
    pub(super) fn degrade_to_polling(&mut self) {
        if !self.native {
            return;
        }
        self.native = false;
        self.native_retry_backoff = NATIVE_RETRY_BASE;
        self.next_native_retry = Instant::now() + NATIVE_RETRY_BASE;
        self.next_scan = Instant::now();
        self.scan_backoff = FALLBACK_BACKOFF_BASE;
    }
}

/// What one poller turn decided to do about one target.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum FallbackTurn {
    /// The native watcher covers it, or its backoff has not elapsed.
    Idle,
    /// A bounded shard ran and the directory scan is still in progress.
    Scanning,
    /// A full scan completed with the same fingerprint. Nothing is published.
    Unchanged,
    /// A full scan completed with a different fingerprint.
    Changed,
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) struct FallbackShard {
    pub(super) processed: usize,
    pub(super) completed: bool,
    pub(super) changed: bool,
}

/// Runs at most one bounded shard for one target and folds the result back into
/// its backoff schedule.
pub(super) fn advance_target(
    target: &Arc<Mutex<FallbackTarget>>,
    stable_target: &Path,
    now: Instant,
) -> anyhow::Result<FallbackTurn> {
    {
        let state = target.lock().unwrap();
        if state.native || now < state.next_scan {
            return Ok(FallbackTurn::Idle);
        }
    }
    let shard = scan_fallback_shard(stable_target, target)?;
    let mut state = target.lock().unwrap();
    if !shard.completed {
        state.next_scan = now;
        return Ok(FallbackTurn::Scanning);
    }
    if shard.changed {
        state.scan_backoff = FALLBACK_BACKOFF_BASE;
        state.next_scan = now + state.scan_backoff;
        return Ok(FallbackTurn::Changed);
    }
    state.scan_backoff = (state.scan_backoff * 2).min(FALLBACK_BACKOFF_MAX);
    state.next_scan = now + state.scan_backoff;
    Ok(FallbackTurn::Unchanged)
}

/// Whether this target is due for another bounded scan shard.
pub(super) fn scan_due(target: &Arc<Mutex<FallbackTarget>>, now: Instant) -> bool {
    let state = target.lock().unwrap();
    !state.native && now >= state.next_scan
}

/// Whether this target is due for another attempt at native registration.
pub(super) fn native_retry_due(target: &Arc<Mutex<FallbackTarget>>, now: Instant) -> bool {
    let state = target.lock().unwrap();
    !state.native && now >= state.next_native_retry
}

pub(super) fn record_native_retry(target: &Arc<Mutex<FallbackTarget>>, restored: bool) {
    let mut state = target.lock().unwrap();
    if restored {
        state.restored_to_native();
    } else {
        state.native_retry_failed();
    }
}

fn scan_fallback_shard(
    stable_target: &Path,
    state: &Mutex<FallbackTarget>,
) -> anyhow::Result<FallbackShard> {
    scan_fallback_shard_with_limits(
        stable_target,
        state,
        FALLBACK_SCAN_ENTRY_BUDGET,
        FALLBACK_SCAN_TIME_BUDGET,
    )
}

pub(super) async fn advance_target_async(
    stable_target: PathBuf,
    target: Arc<Mutex<FallbackTarget>>,
    now: Instant,
) -> anyhow::Result<FallbackTurn> {
    tokio::task::spawn_blocking(move || advance_target(&target, &stable_target, now))
        .await
        .context("fallback filesystem scan worker stopped")?
}

pub(super) fn scan_fallback_shard_with_limits(
    stable_target: &Path,
    state: &Mutex<FallbackTarget>,
    entry_budget: usize,
    time_budget: Duration,
) -> anyhow::Result<FallbackShard> {
    let mut state = state.lock().unwrap();
    let scan = &mut state.scan;
    if scan.iterator.is_none() {
        scan.accumulator = metadata_generation(&fs::metadata(stable_target)?);
        scan.iterator = Some(fs::read_dir(stable_target)?);
    }

    let iterator = scan
        .iterator
        .as_mut()
        .expect("fallback iterator initialized");
    let (additions, processed, completed) =
        fold_fingerprint_records(entry_budget, time_budget, || {
            loop {
                let Some(entry) = iterator.next() else {
                    return Ok(None);
                };
                let entry = entry?;
                match fs::symlink_metadata(entry.path()) {
                    // `None` is a hidden entry, not the end of the directory:
                    // keep walking rather than reporting the shard complete.
                    Ok(metadata) => match watch_entry_fingerprint(&entry.file_name(), &metadata) {
                        Some(value) => return Ok(Some(value)),
                        None => continue,
                    },
                    Err(error) if error.kind() == ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                }
            }
        })?;
    scan.accumulator = scan.accumulator.wrapping_add(additions);

    let mut changed = false;
    if completed {
        let fingerprint = scan.accumulator;
        changed = fingerprint != scan.last_completed;
        scan.last_completed = fingerprint;
        scan.iterator = None;
        scan.accumulator = 0;
    }
    Ok(FallbackShard {
        processed,
        completed,
        changed,
    })
}

pub(super) fn fold_fingerprint_records(
    entry_budget: usize,
    time_budget: Duration,
    mut next: impl FnMut() -> anyhow::Result<Option<u64>>,
) -> anyhow::Result<(u64, usize, bool)> {
    let started = Instant::now();
    let mut accumulator = 0_u64;
    let mut processed = 0;
    while processed < entry_budget && (processed == 0 || started.elapsed() < time_budget) {
        let Some(record) = next()? else {
            return Ok((accumulator, processed, true));
        };
        accumulator = accumulator.wrapping_add(record);
        processed += 1;
    }
    Ok((accumulator, processed, false))
}
