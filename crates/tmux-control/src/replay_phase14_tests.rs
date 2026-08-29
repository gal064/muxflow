use super::*;

fn timed_pane_resource_round(pane_count: usize) -> u128 {
    const CHUNKS_PER_PANE: usize = 8;
    let chunk = [b'x'; 64];
    let started = std::time::Instant::now();
    let mut store =
        PaneResourceStore::with_total_limit(pane_count + 1, 4 * 1024 * 1024, 128 * 1024 * 1024);
    for pane in 0..pane_count {
        let pane_id = format!("%{pane}");
        store.set_visible(&pane_id, false, 1);
        store.seeded(&pane_id, 2);
        for generation in 0..CHUNKS_PER_PANE {
            store.append(&pane_id, &chunk, generation as u64 + 3);
        }
    }
    std::hint::black_box(store.retained_bytes());
    started.elapsed().as_nanos() / (pane_count * (CHUNKS_PER_PANE + 2)) as u128
}

#[test]
#[ignore = "Phase 14 opt-in release timing fixture"]
fn phase14_pane_resource_per_operation_time() {
    for pane_count in [32_usize, 1_024] {
        std::hint::black_box(timed_pane_resource_round(pane_count));
        let mut samples = (0..9)
            .map(|_| timed_pane_resource_round(pane_count))
            .collect::<Vec<_>>();
        samples.sort_unstable();
        println!(
            "PHASE14_METRIC {}",
            serde_json::json!({
                "lane": "paneResourceTime",
                "paneCount": pane_count,
                "samples": samples,
                "medianNanosPerOperation": samples[samples.len() / 2],
            })
        );
    }
}

fn assert_incremental_accounting_matches_model(store: &PaneResourceStore) {
    let resource_bytes = store
        .resources
        .values()
        .map(|resource| resource.raw_tail.len())
        .sum::<usize>();
    let journal_bytes = store
        .output_journals
        .values()
        .flat_map(|journal| journal.iter())
        .map(|output| output.bytes.len())
        .sum::<usize>();
    let retained_panes = store
        .resources
        .iter()
        .filter(|(pane_id, resource)| {
            resource.state == PaneResourceState::HiddenBuffered
                || !resource.raw_tail.is_empty()
                || store
                    .output_journals
                    .get(*pane_id)
                    .is_some_and(|journal| !journal.is_empty())
        })
        .count();
    assert_eq!(store.retained_bytes(), resource_bytes + journal_bytes);
    assert_eq!(store.journal_bytes(), journal_bytes);
    assert_eq!(store.retained_panes(), retained_panes);
    assert!(store.retained_bytes() <= store.max_total_bytes);
    assert!(store.retained_panes() <= store.max_hidden_panes);

    let retained_keys = store
        .resources
        .keys()
        .filter(|pane_id| store.accounted_state(pane_id).retained)
        .cloned()
        .collect();
    assert_lru_matches(&store.retained_lru, &retained_keys);
    let byte_keys = store
        .resources
        .keys()
        .filter(|pane_id| store.accounted_state(pane_id).bytes != 0)
        .cloned()
        .collect();
    assert_lru_matches(&store.byte_lru, &byte_keys);
}

fn assert_lru_matches(lru: &Lru, expected: &std::collections::HashSet<String>) {
    lru.assert_matches(expected);
}

#[test]
fn randomized_resource_transitions_match_the_scanning_model() {
    let mut store = PaneResourceStore::with_total_limit(4, 128, 512);
    let mut random = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = || {
        random ^= random << 7;
        random ^= random >> 9;
        random ^= random << 8;
        random
    };

    for step in 1..=4_000_u64 {
        let pane_id = format!("%{}", next() % 16);
        let bytes = vec![(next() & 0xff) as u8; (next() as usize % 32) + 1];
        match next() % 9 {
            0 => store.ensure(&pane_id, next() & 1 == 0, step),
            1 => store.append(&pane_id, &bytes, step),
            2 => store.seeded(&pane_id, step),
            3 => {
                store.set_visible(&pane_id, true, step);
                let first_generation = step.saturating_mul(4);
                store.record_output(&pane_id, b"before", first_generation);
                store.record_output(&pane_id, b"after", first_generation + 1);
                let hidden = store
                    .hide_with_checkpoint(
                        &pane_id,
                        VisibilityCheckpoint {
                            epoch: step,
                            generation: first_generation,
                        },
                        first_generation + 2,
                    )
                    .unwrap();
                if hidden.state == PaneResourceState::HiddenBuffered {
                    assert_eq!(hidden.raw_tail, b"after");
                }
            }
            4 => {
                let expected = store.get(&pane_id).cloned();
                let revealed = store.reveal(&pane_id, step, None);
                if let (Some(expected), Some(revealed)) = (expected, revealed)
                    && expected.state != PaneResourceState::Visible
                {
                    // A reveal carrying no renderer checkpoint is answered with
                    // a seed, never with the tail the store was holding.
                    assert!(revealed.requires_seed);
                    assert!(revealed.raw_tail.is_empty());
                }
            }
            5 => {
                let expected = store.get(&pane_id).cloned();
                let taken = store.take_recovery(&pane_id);
                if let (Some(expected), Some(taken)) = (expected, taken) {
                    assert_eq!(taken.raw_tail, expected.raw_tail);
                }
            }
            6 => store.remove(&pane_id),
            7 => {
                store.record_output(&pane_id, &bytes, step);
            }
            _ => store.set_visible(&pane_id, next() & 1 == 0, step),
        }
        assert_incremental_accounting_matches_model(&store);
    }
}

#[test]
fn empty_visible_output_matches_journal_presence_accounting() {
    let mut store = PaneResourceStore::with_total_limit(1, 128, 512);
    store.ensure("%1", true, 1);
    assert_eq!(
        store.record_output("%1", b"", 2),
        OutputDisposition::Visible
    );
    assert_eq!(store.retained_bytes(), 0);
    assert_eq!(store.journal_bytes(), 0);
    assert_eq!(store.retained_panes(), 1);
    assert_incremental_accounting_matches_model(&store);

    store.ensure("%2", false, 3);
    assert!(store.get("%1").unwrap().requires_seed);
    assert_eq!(store.retained_panes(), 1);
    assert_incremental_accounting_matches_model(&store);
}

#[test]
fn byte_pressure_preserves_empty_hidden_panes_in_pane_lru_order() {
    let mut store = PaneResourceStore::with_total_limit(2, 128, 5);
    store.ensure("%old", false, 1);
    store.ensure("%bytes", true, 2);
    store.record_output("%bytes", b"123456", 3);
    assert!(store.get("%bytes").unwrap().requires_seed);
    assert_incremental_accounting_matches_model(&store);

    store.ensure("%new1", false, 4);
    store.ensure("%new2", false, 5);
    assert_eq!(
        store.get("%old").unwrap().state,
        PaneResourceState::Released
    );
    assert_eq!(
        store.get("%new1").unwrap().state,
        PaneResourceState::HiddenBuffered
    );
    assert_eq!(
        store.get("%new2").unwrap().state,
        PaneResourceState::HiddenBuffered
    );
    assert_incremental_accounting_matches_model(&store);
}

#[test]
#[ignore = "Phase 14 opt-in operation-count fixture"]
fn phase14_pane_resource_scaling_and_reveal_parity() {
    for pane_count in [32_usize, 256, 1_024] {
        let chunks_per_pane = 8_usize;
        let chunk = vec![b'x'; 64];
        begin_pane_resource_measurement();
        let started = std::time::Instant::now();
        let mut store =
            PaneResourceStore::with_total_limit(pane_count + 1, 4 * 1024 * 1024, 128 * 1024 * 1024);
        for pane in 0..pane_count {
            let pane_id = format!("%{pane}");
            store.set_visible(&pane_id, false, 1);
            store.seeded(&pane_id, 2);
            for generation in 0..chunks_per_pane {
                store.append(&pane_id, &chunk, generation as u64 + 3);
            }
        }
        let expected_bytes = pane_count * chunks_per_pane * chunk.len();
        let retained_bytes = store.retained_bytes();
        let retained_panes = store.retained_panes_for_measurement();
        assert_eq!(retained_bytes, expected_bytes);
        assert_eq!(retained_panes, pane_count);
        let recovery = store
            .take_recovery(&format!("%{}", pane_count - 1))
            .unwrap();
        let elapsed_nanos = started.elapsed().as_nanos();
        let measured_operations = pane_count * (chunks_per_pane + 2) + 1;
        assert_eq!(recovery.raw_tail, vec![b'x'; chunks_per_pane * chunk.len()]);
        let measurements = pane_resource_measurement_snapshot();
        assert_eq!(measurements.full_accounting_scans, 0);
        assert_eq!(measurements.accounting_entries_visited, 0);
        assert_eq!(measurements.lru_retain_operations, 0);
        assert_eq!(measurements.lru_entries_visited, 0);
        println!(
            "PHASE14_METRIC {}",
            serde_json::json!({
                "lane": "paneResource",
                "paneCount": pane_count,
                "chunksPerPane": chunks_per_pane,
                "retainedBytes": retained_bytes,
                "retainedPanes": retained_panes,
                "journalBytes": store.journal_bytes(),
                "fullAccountingScans": measurements.full_accounting_scans,
                "accountingEntriesVisited": measurements.accounting_entries_visited,
                "lruRetainOperations": measurements.lru_retain_operations,
                "lruEntriesVisited": measurements.lru_entries_visited,
                "lruPops": measurements.lru_pops,
                "appendOperations": measurements.append_operations,
                "appendedBytes": measurements.appended_bytes,
                "elapsedNanos": elapsed_nanos,
                "measuredOperations": measured_operations,
                "nanosPerOperation": elapsed_nanos / measured_operations as u128,
                "evictions": measurements.evictions,
                "revealParity": true,
            })
        );
    }

    begin_pane_resource_measurement();
    let mut eviction_store = PaneResourceStore::with_total_limit(1, 1_024, 4_096);
    eviction_store.seeded("%old", 1);
    eviction_store.seeded("%new", 2);
    assert_eq!(
        eviction_store.get("%old").unwrap().state,
        PaneResourceState::Released
    );
    assert_eq!(
        eviction_store.get("%new").unwrap().state,
        PaneResourceState::HiddenBuffered
    );
    let measurements = pane_resource_measurement_snapshot();
    assert_eq!(measurements.evictions, 1);
    assert_eq!(measurements.lru_pops, 1);
    println!(
        "PHASE14_METRIC {}",
        serde_json::json!({
            "lane": "paneResourceEviction",
            "evictions": measurements.evictions,
            "lruPops": measurements.lru_pops,
            "oldPaneReleased": true,
            "newPaneRetained": true,
        })
    );

    begin_pane_resource_measurement();
    let mut sparse_store = PaneResourceStore::with_total_limit(0, 1_024, 4_096);
    for pane in 0..1_024 {
        sparse_store.ensure(&format!("%visible-{pane}"), true, pane);
    }
    sparse_store.ensure("%hidden", false, 1_025);
    let sparse_measurements = pane_resource_measurement_snapshot();
    assert_eq!(sparse_measurements.lru_pops, 1);
    assert_eq!(sparse_measurements.evictions, 1);
    println!(
        "PHASE14_METRIC {}",
        serde_json::json!({
            "lane": "paneResourceSparseEviction",
            "visibleNonRetainedPanes": 1_024,
            "lruPops": sparse_measurements.lru_pops,
            "evictions": sparse_measurements.evictions,
        })
    );
}
