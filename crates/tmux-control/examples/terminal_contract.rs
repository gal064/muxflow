//! Desktop contract driver: real PaneResourceStore, synthetic capture/transport.
use std::{collections::HashMap, io};

use serde::Deserialize;
use serde_json::{Value, json};
use tmux_control::{
    OutputDisposition, PaneResource, PaneResourceState, PaneResourceStore, REVEAL_TAIL_BOUND,
    VisibilityCheckpoint,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Action {
    kind: String,
    #[serde(default)]
    pane_id: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    holds_snapshot: bool,
    #[serde(default)]
    terminal_epoch: u64,
    #[serde(default)]
    output_generation: u64,
    #[serde(default)]
    state: String,
}

fn resource_event(pane_id: &str, resource: &PaneResource) -> Value {
    json!({
        "kind": "paneResource", "paneId": pane_id,
        "state": match resource.state {
            PaneResourceState::Visible => "visible",
            PaneResourceState::HiddenBuffered => "hiddenBuffered",
            PaneResourceState::Released => "released",
        },
        "rawTail": resource.raw_tail, "generation": resource.generation,
        "snapshotGeneration": resource.snapshot_generation,
        "tailThroughGeneration": resource.tail_through_generation,
        "requiresSeed": resource.requires_seed,
        "resumeFromRenderer": resource.resume_from_renderer,
        "recoveryReason": resource.recovery_reason,
    })
}

fn capture(
    store: &mut PaneResourceStore,
    screens: &HashMap<String, String>,
    pane: &str,
    generation: &mut u64,
    events: &mut Vec<Value>,
) {
    *generation += 1;
    store.ensure(pane, false, *generation);
    store.seeded(pane, *generation);
    if !store.is_hidden(pane) {
        events.push(
            json!({"kind": "seed", "paneId": pane, "generation": generation,
            "data": screens.get(pane).map_or(&[][..], |text| text.as_bytes())}),
        );
    }
}

fn main() {
    let actions: Vec<Action> = serde_json::from_reader(io::stdin()).unwrap();
    let mut store = PaneResourceStore::new(32, REVEAL_TAIL_BOUND);
    let mut screens = HashMap::<String, String>::new();
    let mut generation = 0;
    let mut events = Vec::new();
    let mut error = None;
    for action in actions {
        error = None;
        let pane = action.pane_id.as_str();
        let checkpoint = VisibilityCheckpoint {
            epoch: action.terminal_epoch,
            generation: action.output_generation,
        };
        match action.kind.as_str() {
            "epoch" => {
                events.push(json!({"kind": "generationEpoch", "epoch": action.terminal_epoch}))
            }
            "output" => {
                generation += 1;
                screens
                    .entry(pane.into())
                    .or_default()
                    .push_str(&action.text);
                store.ensure(pane, false, generation);
                if store.record_output(pane, action.text.as_bytes(), generation)
                    == OutputDisposition::Visible
                {
                    events.push(json!({"kind": "output", "paneId": pane, "generation": generation, "data": action.text.as_bytes()}));
                }
            }
            "capture" => capture(&mut store, &screens, pane, &mut generation, &mut events),
            "seed" => {
                store.reveal_for_seed_request(pane, generation);
                capture(&mut store, &screens, pane, &mut generation, &mut events);
            }
            "hide" => {
                generation += 1;
                match store.hide_with_checkpoint(pane, checkpoint, generation) {
                    Ok(resource) => events.push(resource_event(
                        pane,
                        &resource.into_visibility_response(false),
                    )),
                    Err(reason) => error = Some(reason),
                }
            }
            "reveal" => {
                generation += 1;
                store.ensure(pane, false, generation);
                let resource = store
                    .reveal(
                        pane,
                        generation,
                        action.holds_snapshot.then_some(checkpoint),
                    )
                    .unwrap()
                    .into_visibility_response(true);
                events.push(resource_event(pane, &resource));
                if resource.requires_seed {
                    capture(&mut store, &screens, pane, &mut generation, &mut events);
                }
            }
            // Deliberately malformed input for the desktop's recovery paths.
            "unusable" => {
                generation += 1;
                events.push(json!({"kind": "paneResource", "paneId": pane, "state": action.state,
                    "generation": generation, "snapshotGeneration": generation, "tailThroughGeneration": generation,
                    "rawTail": [], "requiresSeed": false, "resumeFromRenderer": false, "recoveryReason": ""}));
            }
            kind => panic!("unknown action: {kind}"),
        }
    }
    for (index, event) in events.iter_mut().enumerate() {
        event["sequence"] = json!(index + 1);
    }
    println!(
        "{}",
        json!({"events": events, "generation": generation, "revealTailBound": REVEAL_TAIL_BOUND, "error": error})
    );
}
