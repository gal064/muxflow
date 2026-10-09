//! Filename searches share the control lane with input: measure actual echoed
//! bytes while enumeration is in flight, rather than substituting an RPC ack.
use super::*;

pub(super) fn run(
    transport: Transport,
    primary_name: &str,
    label: &str,
    reconcile_timeout: Duration,
) -> Result<()> {
    let mut client = PerfClient::connect(&transport, reconcile_timeout)?;
    let initial = client.subscribe()?;
    let primary = session(&initial, primary_name)?.clone();
    client.attach(&initial, &primary.id)?;
    let pane = initial
        .panes
        .iter()
        .find(|pane| pane.session_id == primary.id && pane.active)
        .context("Quick Open fixture needs an active pane")?
        .clone();
    let response = client.request(
        v1::Operation::ResolveActiveRoot,
        v1::Request {
            file: Some(v1::FileServiceRequest {
                pane_id: pane.id.clone(),
                expected_server_identity: client.server_identity.clone(),
                expected_topology_generation: initial.generation,
                ..Default::default()
            }),
            ..Default::default()
        },
    )?;
    ensure!(response.ok, "root resolution: {}", response.display_message);
    let root = response
        .file
        .and_then(|file| file.active_root)
        .context("root omitted")?;
    let request = v1::Request {
        file: Some(v1::FileServiceRequest {
            operation_id: "quick-open".into(),
            root: root.root.clone(),
            root_token: root.root_token,
            pane_id: pane.id.clone(),
            expected_server_identity: client.server_identity.clone(),
            expected_topology_generation: initial.generation,
            expected_session_id: pane.session_id.clone(),
            expected_window_id: pane.window_id.clone(),
            expected_cwd: pane.current_path.clone(),
            search_query: "file".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let baseline = probe_keystroke(&mut client, &pane.id, 45)?;
    let mut echo = Vec::new();
    let mut searches = Vec::new();
    let mut limited = 0;
    let mut max_results = 0;
    let mut max_payload = 0;
    for index in 0..45 {
        let before = client.obs.output_bytes(&pane.id);
        let search_started = Instant::now();
        let search_id = client.send(v1::Operation::SearchFiles, request.clone())?;
        let input_started = Instant::now();
        let input_id = client.send(
            v1::Operation::TerminalInput,
            v1::Request {
                scope: pane.id.clone(),
                data: b"x".to_vec(),
                ..Default::default()
            },
        )?;
        let mut echoed = None;
        ensure!(
            client.pump_until(
                search_started + Duration::from_secs(10),
                |obs, responses| {
                    if echoed.is_none()
                        && obs.output_bytes(&pane.id) > before
                        && responses.contains_key(&input_id)
                    {
                        echoed = Some(input_started.elapsed().as_secs_f64() * 1000.0);
                    }
                    echoed.is_some() && responses.contains_key(&search_id)
                }
            )?,
            "search/input timed out"
        );
        let elapsed = search_started.elapsed().as_secs_f64() * 1000.0;
        let response = client
            .responses
            .remove(&search_id)
            .context("search response missing")?;
        ensure!(
            response.ok,
            "search rejected: {} {}",
            response.error_code,
            response.display_message
        );
        let search = response
            .file
            .and_then(|file| file.search)
            .context("search omitted")?;
        ensure!(
            !search.matches.is_empty(),
            "fixture must contain filenames matching 'file'"
        );
        ensure!(
            search
                .matches
                .iter()
                .all(|item| PathBuf::from(&item.path).starts_with(&pane.current_path)),
            "search escaped cwd"
        );
        let payload =
            serde_json::to_vec(&json!({ "matches": search.matches.iter().map(|item| json!({
            "path": item.path, "relativePath": item.relative_path, "score": item.score,
        })).collect::<Vec<_>>(), "complete": search.complete }))?
            .len();
        ensure!(
            search.matches.len() <= 75 && payload < 32 * 1024,
            "search response exceeded bounds"
        );
        limited += usize::from(!search.complete);
        max_results = max_results.max(search.matches.len());
        max_payload = max_payload.max(payload);
        ensure!(
            client
                .responses
                .remove(&input_id)
                .context("input response missing")?
                .ok,
            "input rejected"
        );
        if index >= 5 {
            echo.push(echoed.unwrap());
            searches.push(elapsed);
        }
    }
    client.input(&pane.id, b"\x15")?;
    // A caller cannot search another cwd by merely changing its captured route.
    let mut stale = request;
    stale
        .file
        .as_mut()
        .unwrap()
        .expected_cwd
        .push_str("/not-the-pane");
    ensure!(
        !client.request(v1::Operation::SearchFiles, stale)?.ok,
        "stale cwd was accepted"
    );
    let baseline = stats(baseline.echo);
    let during = stats(echo);
    let baseline_p95 = baseline["p95Ms"].as_f64().context("baseline p95")?;
    let during_p95 = during["p95Ms"].as_f64().context("search p95")?;
    let tolerance = (baseline_p95 * 0.05).max(2.0);
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "label": label, "baselineEchoMs": baseline, "duringSearchEchoMs": during,
            "searchMs": stats(searches), "inputBudgetPassed": during_p95 <= baseline_p95 + tolerance,
            "limitedSearches": limited, "maxResults": max_results, "maxPayloadBytes": max_payload,
            "staleCwdRejected": true, "resyncs": client.obs.resyncs(),
        }))?
    );
    Ok(())
}
