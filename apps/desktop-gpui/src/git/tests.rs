//! Ported from `api.test.ts`, `repositoryStore.test.ts`, `GitSidebar.test.tsx`,
//! `GitDiffSurface.test.tsx`, `useWorkspaceGit.test.tsx` and the Git half of
//! `useWorkspaceDomainController.test.ts`.

use std::{cell::RefCell, rc::Rc, sync::Arc};

use desktop_core::{
    app_state::{AppTabKind, AppTabRecord, PersistedAppState},
    connection::{git::GitCommand, git_content::GitDiffContentCommand},
    platform::EventSink,
};
use futures::{FutureExt, channel::oneshot, future::BoxFuture};
use gpui_kit::{AppContext as _, Entity, TestAppContext};
use serde_json::{Value, json};

use super::{
    client::{self, GitTransport, Transport},
    diff_view::GitDiffView,
    model::{self, *},
    panel::{GitPanel, PanelContext, PanelEvent, Work},
    store::GitRepositories,
};

// A host that answers what a test tells it to, and remembers what it was asked.

type Answer = Box<dyn FnMut(&GitCommand) -> Option<Result<Value, String>>>;
type Held = Vec<(GitCommand, oneshot::Sender<Result<Value, String>>)>;

struct Fake {
    requests: RefCell<Vec<GitCommand>>,
    held: RefCell<Held>,
    cancelled: RefCell<Vec<String>>,
    answer: RefCell<Answer>,
}

impl Fake {
    fn new(answer: impl FnMut(&GitCommand) -> Option<Result<Value, String>> + 'static) -> Rc<Self> {
        Rc::new(Self {
            requests: RefCell::default(),
            held: RefCell::default(),
            cancelled: RefCell::default(),
            answer: RefCell::new(Box::new(answer)),
        })
    }

    fn operations(&self) -> Vec<String> {
        self.requests
            .borrow()
            .iter()
            .map(|command| command.operation.clone())
            .collect()
    }

    fn count(&self, operation: &str) -> usize {
        self.operations()
            .iter()
            .filter(|op| *op == operation)
            .count()
    }

    /// Answers the oldest held request for `operation`.
    fn release(&self, operation: &str, value: Result<Value, String>) {
        let mut held = self.held.borrow_mut();
        let index = held
            .iter()
            .position(|(command, _)| command.operation == operation)
            .unwrap_or_else(|| panic!("no held {operation}"));
        let (_, sender) = held.remove(index);
        let _ = sender.send(value);
    }

    fn transport(self: &Rc<Self>) -> Transport {
        self.clone()
    }
}

impl GitTransport for Fake {
    fn request(&self, _: String, command: GitCommand) -> BoxFuture<'static, Result<Value, String>> {
        self.requests.borrow_mut().push(command.clone());
        let answer = (self.answer.borrow_mut())(&command);
        match answer {
            Some(answer) => futures::future::ready(answer).boxed(),
            None => {
                let (sender, receiver) = oneshot::channel();
                self.held.borrow_mut().push((command, sender));
                async move { receiver.await.unwrap_or(Err("dropped".into())) }.boxed()
            }
        }
    }

    fn cancel(&self, _: String, operation_id: String) {
        self.cancelled.borrow_mut().push(operation_id);
    }

    fn read_content(
        &self,
        _: GitDiffContentCommand,
        _: Arc<dyn EventSink>,
    ) -> BoxFuture<'static, Result<String, String>> {
        futures::future::ready(Err("no bulk lane".to_owned())).boxed()
    }

    fn cancel_content(&self, _: String, _: String) {}
}

/// "a.txt" and "dir/b.txt" as the host sends them.
const A: &str = "YS50eHQ=";
const B: &str = "ZGlyL2IudHh0";

fn repository(id: &str) -> Value {
    json!({ "repositoryId": id, "worktreeRoot": "/work", "headName": "main" })
}

fn entry(path: &str, display: &str, index: &str, worktree: &str) -> Value {
    json!({
        "path": path, "displayPath": display, "indexKind": index, "worktreeKind": worktree,
        "untracked": worktree == "untracked",
    })
}

fn status_in(repo: &str, generation: u64, entries: Vec<Value>) -> Value {
    json!({
        "repository": repository(repo),
        "generation": generation.to_string(),
        "sourceGeneration": format!("source-{generation}"),
        "entries": entries,
        "authoritative": true,
    })
}

fn status(generation: u64) -> Value {
    status_in(
        "repo",
        generation,
        vec![entry(A, "a.txt", "none", "modified")],
    )
}

fn diff_response(repo: &str, path_bytes: &[u8], generation: u64) -> Value {
    json!({
        "diff": {
            "repository": repository(repo), "target": "unstaged", "path": path_bytes,
            "displayPath": "a.txt", "oldContent": b"base\n", "newContent": b"changed\n",
            "sourceGeneration": format!("source-{generation}"), "hunkCount": 1,
        },
        "status": status_in(repo, generation, vec![entry(A, "a.txt", "none", "modified")]),
    })
}

fn applied(generation: u64) -> Value {
    json!({ "command": { "outcome": "applied", "status": status(generation) } })
}

fn scope(token: &str, epoch: u64) -> GitScope {
    GitScope {
        client_id: "client".into(),
        host_profile_id: "local".into(),
        server_identity: "server".into(),
        terminal_epoch: epoch,
        root_path: "/work".into(),
        root_token: token.into(),
    }
}

fn live(epoch: u64) -> Live {
    Live {
        connected: true,
        client_id: Some("client".into()),
        terminal_epoch: epoch,
        server_identity: Some("server".into()),
        host_profile_id: "local".into(),
    }
}

fn root(token: &str) -> RootInfo {
    RootInfo {
        path: "/work".into(),
        token: token.into(),
        git_worktree: true,
    }
}

fn parse(value: Value) -> StatusSnapshot {
    model::status(serde_json::from_value(value).unwrap()).unwrap()
}

fn snapshot(generation: u64) -> StatusSnapshot {
    parse(status(generation))
}

fn unstaged_a() -> DiffRequest {
    DiffRequest {
        repository_id: "repo".into(),
        path: A.into(),
        original_path: None,
        target: Target::Unstaged,
    }
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    futures::executor::block_on(future)
}

// The wire (`api.test.ts`).

#[test]
fn preserves_opaque_path_bytes_and_decimal_u64_generations() {
    let parsed = parse(status_in(
        "repo",
        u64::MAX,
        vec![entry(B, "dir/b.txt", "added", "none")],
    ));
    assert_eq!(parsed.generation, u64::MAX);
    assert_eq!(parsed.entries[0].path, B);
    assert_eq!(path_bytes(B), b"dir/b.txt");
    assert_eq!(
        parsed.entries[0].absolute_path.as_deref(),
        Some("/work/dir/b.txt")
    );
}

#[test]
fn turns_malformed_event_generations_and_paths_into_scoped_errors() {
    let mut bad = status(1);
    bad["generation"] = json!("01");
    let event = json!({ "watchId": "w", "rootToken": "t", "status": bad });
    let events = watch_events(&event.to_string());
    assert_eq!(events.len(), 1);
    assert!(
        events[0]
            .update
            .as_ref()
            .unwrap_err()
            .contains("decimal u64")
    );

    let not_base64 = status_in(
        "repo",
        1,
        vec![entry("a.txt!", "a.txt", "none", "modified")],
    );
    let event = json!({ "watchId": "w", "rootToken": "t", "status": not_base64 });
    assert!(watch_events(&event.to_string())[0].update.is_err());
}

#[test]
fn routes_only_root_token_bound_status_events() {
    let unbound = json!({ "watchId": "w", "status": status(1) });
    assert!(watch_events(&unbound.to_string()).is_empty());
    let failed = json!({ "watchId": "w", "rootToken": "t", "error": "gone" });
    let events = watch_events(&failed.to_string());
    assert_eq!(events[0].update, Err("gone".into()));
    assert_eq!(events[0].root_token, "t");
}

#[test]
fn rejects_stale_or_mismatched_diff_identity() {
    let fake = Fake::new(|_| Some(Ok(diff_response("repo", b"other.txt", 2))));
    let error = block_on(client::diff(fake.transport(), scope("t", 1), unstaged_a())).unwrap_err();
    assert_eq!(error, "Host returned a stale Git diff identity.");

    // The diff matches, but the status it was read against is another
    // repository's.
    let fake = Fake::new(|_| {
        let mut response = diff_response("repo", b"a.txt", 2);
        response["status"] = status_in("other", 2, vec![]);
        Some(Ok(response))
    });
    let error = block_on(client::diff(fake.transport(), scope("t", 1), unstaged_a())).unwrap_err();
    assert_eq!(
        error,
        "Host returned a Git diff status for a different repository."
    );
}

#[test]
fn carries_the_authoritative_status_a_diff_was_read_against_in_one_round_trip() {
    let fake = Fake::new(|_| Some(Ok(diff_response("repo", b"a.txt", 7))));
    let (diff, status) =
        block_on(client::diff(fake.transport(), scope("t", 1), unstaged_a())).unwrap();
    assert_eq!(fake.operations(), ["diff"]);
    assert_eq!(fake.requests.borrow()[0].path, b"a.txt");
    assert_eq!(status.generation, 7);
    assert_eq!(diff.new_content, b"changed\n");
    assert_eq!(diff.hunk_count, 1);
}

#[test]
fn addresses_mutations_by_opaque_path_and_refuses_discard_without_a_token() {
    let fake = Fake::new(|_| Some(Ok(applied(2))));
    let mutation = Mutation {
        kind: MutationKind::StageFile,
        path: B.into(),
        original_path: None,
        target: Target::Unstaged,
        expected_status_generation: 1,
        expected_source_generation: "source-1".into(),
        hunk_index: None,
        confirmation_token: None,
    };
    let transport = fake.transport();
    block_on(client::mutate(
        transport.clone(),
        scope("t", 1),
        "repo".into(),
        mutation.clone(),
    ))
    .unwrap();
    let sent = fake.requests.borrow()[0].clone();
    assert_eq!(sent.path, b"dir/b.txt");
    assert_eq!(sent.mutation, "stageFile");
    assert_eq!(sent.expected_status_generation, "1");

    let discard = Mutation {
        kind: MutationKind::DiscardFile,
        ..mutation
    };
    let error = block_on(client::mutate(
        transport,
        scope("t", 1),
        "repo".into(),
        discard,
    ));
    assert_eq!(error.unwrap_err(), "Discard requires confirmation.");
    assert_eq!(fake.requests.borrow().len(), 1);
}

#[test]
fn sends_a_commit_and_a_push_exactly_once() {
    let fake = Fake::new(|command| {
        Some(match command.operation.as_str() {
            "commit" => Err("connection lost".into()),
            _ => Ok(json!({ "command": { "outcome": "applied", "pushTarget": "origin/main" } })),
        })
    });
    let transport = fake.transport();
    let committed = block_on(client::commit(
        transport.clone(),
        scope("t", 1),
        "repo".into(),
        3,
        "msg".into(),
    ));
    assert_eq!(committed.unwrap_err(), "connection lost");
    let pushed = block_on(client::push(
        transport.clone(),
        scope("t", 1),
        "repo".into(),
        3,
    ))
    .unwrap();
    assert_eq!(fake.operations(), ["commit", "push"]);
    assert_eq!(model::pushed_to(&pushed), "Pushed to origin/main.");

    let empty = block_on(client::commit(
        transport,
        scope("t", 1),
        "repo".into(),
        3,
        " ".into(),
    ));
    assert_eq!(empty.unwrap_err(), "Enter a commit message.");
    assert_eq!(fake.requests.borrow().len(), 2);
}

#[test]
fn cancels_the_exact_live_read_when_its_reader_stops_waiting() {
    let fake = Fake::new(|_| None);
    let mut read = Box::pin(client::status(fake.transport(), scope("t", 1)));
    assert!((&mut read).now_or_never().is_none());
    drop(read);
    let id = fake.requests.borrow()[0].operation_id.clone();
    assert_eq!(*fake.cancelled.borrow(), [id]);

    // A mutation is never cancelled behind the user's back.
    let fake = Fake::new(|_| None);
    let mut commit = Box::pin(client::commit(
        fake.transport(),
        scope("t", 1),
        "repo".into(),
        1,
        "m".into(),
    ));
    assert!((&mut commit).now_or_never().is_none());
    drop(commit);
    assert!(fake.cancelled.borrow().is_empty());
}

// The domain controller's Git half.

#[test]
fn creates_no_request_scope_before_the_bridge_says_connected() {
    let link = Live {
        connected: false,
        ..live(1)
    };
    assert_eq!(git_scope(&link, &root("t")), None);
    assert_eq!(git_scope(&live(1), &root("t")), Some(scope("t", 1)));
}

#[test]
fn requires_every_native_transport_identity() {
    for link in [
        Live {
            client_id: None,
            ..live(1)
        },
        Live {
            server_identity: None,
            ..live(1)
        },
        Live {
            server_identity: Some(String::new()),
            ..live(1)
        },
        live(0),
    ] {
        assert_eq!(git_scope(&link, &root("t")), None, "{link:?}");
    }
    assert_eq!(git_scope(&live(1), &root("")), None);
}

// The panel's model (`GitSidebar.test.tsx`).

#[test]
fn groups_conflicts_staged_unstaged_and_untracked_and_shows_nothing_ignored() {
    let parsed = parse(status_in(
        "repo",
        1,
        vec![
            entry(A, "a.txt", "modified", "modified"),
            entry(B, "dir/b.txt", "none", "untracked"),
            json!({
                "path": "Yy50eHQ=", "displayPath": "c.txt", "conflicted": true,
                "conflictCode": "UU", "indexKind": "unmerged", "worktreeKind": "unmerged",
            }),
            json!({ "path": "ZC50eHQ=", "displayPath": "d.txt", "ignored": true, "worktreeKind": "ignored" }),
        ],
    ));
    let groups = group_entries(&parsed.entries);
    let names = |list: &Vec<&StatusEntry>| -> Vec<String> {
        list.iter()
            .map(|entry| entry.display_path.clone())
            .collect()
    };
    assert_eq!(names(&groups.conflicts), ["c.txt"]);
    assert_eq!(names(&groups.staged), ["a.txt"]);
    assert_eq!(names(&groups.unstaged), ["a.txt"]);
    assert_eq!(names(&groups.untracked), ["dir/b.txt"]);
    assert_eq!(status_code(groups.conflicts[0], Target::Unstaged), "U");
    assert_eq!(status_code(groups.staged[0], Target::Staged), "M");
    assert_eq!(
        status_title(groups.conflicts[0], Target::Unstaged),
        "Conflict UU"
    );
    assert_eq!(split_display_path("dir/b.txt"), ("b.txt", "dir"));
}

#[test]
fn reports_a_completed_command_separately_from_a_failed_status_refresh() {
    let clean = model::response_command(model::response(applied(2)).unwrap()).unwrap();
    assert!(!needs_notice(&clean));
    assert_eq!(command_details(&clean, Verb::Commit), "Commit created.");
    let result = CommandResult {
        refresh_failed: true,
        refresh_error: "timed out".into(),
        ..clean
    };
    assert!(needs_notice(&result));
    assert_eq!(
        result_message(&result, "Commit created."),
        "Commit created. Status refresh failed: timed out"
    );
    assert_eq!(
        command_details(&result, Verb::Commit),
        "Commit completed, but status refresh failed: timed out"
    );
}

// The diff tab's model.

fn place<'a>() -> DiffTabPlace<'a> {
    DiffTabPlace {
        host: "local",
        server: "server",
        session_id: "$1",
        session_name: "work",
        root_path: "/work",
        root_token: "t",
    }
}

#[test]
fn opens_a_preview_that_a_pinned_open_promotes_and_never_demotes() {
    let mut state = PersistedAppState::default();
    let status = snapshot(1);
    let entry = status.entries[0].clone();
    let mut ids = ["first", "second"].into_iter();
    let mut open = |state: &mut PersistedAppState, preview| {
        open_git_diff_tab(
            state,
            &place(),
            &entry,
            Target::Unstaged,
            &status,
            preview,
            || ids.next().unwrap().into(),
        )
    };
    assert_eq!(open(&mut state, true), "first");
    assert_eq!(state.app_tabs[0].preview, Some(true));
    assert_eq!(state.app_tabs[0].kind, AppTabKind::GitDiff);
    assert_eq!(state.app_tabs[0].title, "a.txt (unstaged)");
    assert_eq!(model::tab_display_path(&state.app_tabs[0]), "a.txt");
    assert_eq!(open(&mut state, false), "first");
    assert_eq!(state.app_tabs[0].preview, None);
    assert_eq!(open(&mut state, true), "first");
    assert_eq!(state.app_tabs[0].preview, None);
    assert_eq!(state.app_tabs.len(), 1);
}

#[test]
fn shows_every_line_of_the_file_and_refuses_what_is_not_text() {
    let patch = whole_file_patch(
        "dir/a.rs",
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n",
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\nEIGHT\n",
    );
    assert!(patch.starts_with("--- a/a.rs\n+++ b/a.rs\n"));
    assert!(patch.contains(" one\n") && patch.contains("-eight\n+EIGHT\n"));
    let parse_patch = gpui_kit::component::diff::DiffFile::parse;
    assert_eq!(parse_patch(&patch).unwrap().len(), 1);
    // A mode change has no text change, and the file is still shown.
    let same = whole_file_patch("a.txt", "x\n", "x\n");
    assert!(same.contains(" x\n"));
    assert_eq!(parse_patch(&same).unwrap().len(), 1);
    // An unusual name never reaches the patch header.
    assert!(whole_file_patch("a b\n+++ c", "", "y\n").starts_with("--- a/file\n"));

    let response = model::response(diff_response("repo", b"a.txt", 1)).unwrap();
    let mut diff = model::response_diff(response, &unstaged_a()).unwrap().0;
    assert!(matches!(shown_diff(&diff), Shown::Text { .. }));
    diff.new_content = vec![0xff];
    assert_eq!(
        shown_diff(&diff),
        Shown::Blocked("This diff contains non-UTF-8 content and is shown safely as binary.")
    );
    diff.binary = true;
    assert_eq!(
        shown_diff(&diff),
        Shown::Blocked("Binary changes cannot be displayed or edited as text.")
    );
}

// The shared observation (`repositoryStore.test.ts`, `useWorkspaceGit.test.tsx`).

fn store(cx: &mut TestAppContext, fake: &Rc<Fake>) -> Entity<GitRepositories> {
    let transport = fake.transport();
    cx.new(|_| GitRepositories::new(transport))
}

fn watch_event(watch_id: &str, token: &str, generation: u64) -> WatchEvent {
    WatchEvent {
        watch_id: watch_id.into(),
        root_token: token.into(),
        update: Ok(snapshot(generation)),
    }
}

fn generation(store: &Entity<GitRepositories>, key: &str, cx: &mut TestAppContext) -> Option<u64> {
    store.read_with(cx, |store, _| {
        store.state(key).status.map(|status| status.generation)
    })
}

fn watching(command: &GitCommand) -> Option<Result<Value, String>> {
    (command.operation == "watch").then(|| Ok(json!({ "status": status(1) })))
}

#[gpui_kit::test]
fn gives_every_consumer_of_one_repository_the_same_watch_and_status(cx: &mut TestAppContext) {
    let fake = Fake::new(watching);
    let store = store(cx, &fake);
    let key = store.update(cx, |store, cx| store.acquire(&scope("t", 1), cx));
    cx.run_until_parked();
    let second = store.update(cx, |store, cx| store.acquire(&scope("t", 1), cx));
    assert_eq!(key, second);
    assert_eq!(fake.count("watch"), 1);
    assert_eq!(generation(&store, &key, cx), Some(1));

    // Only the last release ends the watch.
    store.update(cx, |store, cx| store.release(&key, cx));
    cx.run_until_parked();
    assert_eq!(fake.count("unwatch"), 0);
    store.update(cx, |store, cx| store.release(&key, cx));
    cx.run_until_parked();
    assert_eq!(fake.count("unwatch"), 1);

    // Re-acquired, it paints from the remembered status before the watch
    // answers.
    *fake.answer.borrow_mut() = Box::new(|_| None);
    let key = store.update(cx, |store, cx| store.acquire(&scope("t", 1), cx));
    let state = store.read_with(cx, |store, _| store.state(&key));
    assert_eq!(state.status.map(|status| status.generation), Some(1));
    assert!(state.loading);
}

#[gpui_kit::test]
fn reconciles_events_that_beat_the_watch_and_ignores_other_watches(cx: &mut TestAppContext) {
    let fake = Fake::new(|_| None);
    let store = store(cx, &fake);
    let key = store.update(cx, |store, cx| store.acquire(&scope("t", 1), cx));
    cx.run_until_parked();
    let watch_id = fake.requests.borrow()[0].watch_id.clone();
    store.update(cx, |store, cx| {
        store.handle_event(watch_event(&watch_id, "t", 3), cx);
        store.handle_event(watch_event("someone-else", "t", 9), cx);
        store.handle_event(watch_event(&watch_id, "other-root", 9), cx);
    });
    fake.release("watch", Ok(json!({ "status": status(2) })));
    cx.run_until_parked();
    assert_eq!(generation(&store, &key, cx), Some(3));

    // Never regresses to an older generation, and another watch is not ours.
    store.update(cx, |store, cx| {
        store.handle_event(watch_event(&watch_id, "t", 2), cx);
        store.handle_event(watch_event("someone-else", "t", 8), cx);
    });
    assert_eq!(generation(&store, &key, cx), Some(3));
}

#[gpui_kit::test]
fn treats_an_identical_status_as_no_transition(cx: &mut TestAppContext) {
    let fake = Fake::new(watching);
    let store = store(cx, &fake);
    let key = store.update(cx, |store, cx| store.acquire(&scope("t", 1), cx));
    cx.run_until_parked();
    let before = store.read_with(cx, |store, _| store.state(&key).status.unwrap());
    store.update(cx, |store, cx| store.accept(&key, snapshot(1), cx));
    let after = store.read_with(cx, |store, _| store.state(&key).status.unwrap());
    assert!(Rc::ptr_eq(&before, &after));
}

#[gpui_kit::test]
fn opens_no_second_watch_while_one_is_in_flight_and_restarts_a_failed_one(cx: &mut TestAppContext) {
    let fake = Fake::new(|_| None);
    let store = store(cx, &fake);
    let key = store.update(cx, |store, cx| store.acquire(&scope("t", 1), cx));
    // The store holds the refresh; the caller need not wait on it.
    drop(store.update(cx, |store, cx| store.refresh(&key, cx)));
    cx.run_until_parked();
    assert_eq!(fake.operations(), ["watch", "status"]);
    fake.release("status", Ok(json!({ "status": status(1) })));
    cx.run_until_parked();
    assert!(!store.read_with(cx, |store, _| store.state(&key).loading));

    fake.release("watch", Err("helper restarted".into()));
    cx.run_until_parked();
    let state = store.read_with(cx, |store, _| store.state(&key));
    assert_eq!(state.error.as_deref(), Some("helper restarted"));
    // Asking again restarts the watch, and a status coming back clears the
    // error.
    // The store holds the refresh; the caller need not wait on it.
    drop(store.update(cx, |store, cx| store.refresh(&key, cx)));
    cx.run_until_parked();
    assert_eq!(fake.count("watch"), 2);
    fake.release("status", Ok(json!({ "status": status(2) })));
    cx.run_until_parked();
    let state = store.read_with(cx, |store, _| store.state(&key));
    assert_eq!(state.error, None);
    assert_eq!(state.status.map(|status| status.generation), Some(2));
}

// The panel (`GitSidebar.test.tsx`, `useWorkspaceGit.test.tsx`).

struct Panel {
    fake: Rc<Fake>,
    store: Entity<GitRepositories>,
    panel: Entity<GitPanel>,
    messages: Rc<RefCell<Vec<String>>>,
    /// The window the commit form lives in.
    window: gpui_kit::AnyWindowHandle,
}

impl Panel {
    fn new(
        cx: &mut TestAppContext,
        answer: impl FnMut(&GitCommand) -> Option<Result<Value, String>> + 'static,
    ) -> Self {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(crate::app::Core::at(std::path::Path::new(
                "/nonexistent/app-state.json",
            )));
            crate::commands::init(cx);
        });
        let fake = Fake::new(answer);
        let store = store(cx, &fake);
        let owner = store.clone();
        let panel = cx.new(|cx| GitPanel::new(owner, cx));
        let messages = Rc::new(RefCell::new(Vec::new()));
        cx.update(|cx| {
            let messages = messages.clone();
            cx.subscribe(&panel, move |_, event, _| {
                if let PanelEvent::Message(message) = event {
                    messages.borrow_mut().push(message.clone());
                }
            })
            .detach();
        });
        cx.add_empty_window();
        let window = *cx.windows().last().unwrap();
        Self {
            fake,
            store,
            panel,
            messages,
            window,
        }
    }

    fn point_at(&self, token: &str, epoch: u64, cx: &mut TestAppContext) {
        let context = PanelContext::new(Some(root(token)), live(epoch), true);
        self.panel
            .update(cx, |panel, cx| panel.set_context(context, cx));
        cx.run_until_parked();
    }

    fn entry(&self, cx: &mut TestAppContext) -> StatusEntry {
        self.panel
            .read_with(cx, |panel, cx| panel.status(cx).unwrap().entries[0].clone())
    }

    fn pending_discard(&self, cx: &mut TestAppContext) -> Option<super::panel::PendingDiscard> {
        let entry = self.entry(cx);
        self.panel.read_with(cx, |panel, cx| {
            panel.pending_discard(&entry, Target::Unstaged, cx)
        })
    }

    fn run(&self, work: Work, cx: &mut TestAppContext) {
        let panel = self.panel.clone();
        cx.update_window(self.window, |_, window, cx| {
            panel.update(cx, |panel, cx| panel.run(work, window, cx))
        })
        .unwrap();
        cx.run_until_parked();
    }

    fn type_message(&self, text: &str, cx: &mut TestAppContext) {
        let panel = self.panel.clone();
        cx.update_window(self.window, |_, window, cx| {
            panel.update(cx, |panel, cx| panel.set_message(text, window, cx))
        })
        .unwrap();
    }
}

fn answering(command: &GitCommand) -> Option<Result<Value, String>> {
    Some(Ok(match command.operation.as_str() {
        "watch" | "status" => json!({ "status": status(1) }),
        "prepareDiscard" => json!({ "confirmation": { "token": "once" } }),
        _ => applied(2),
    }))
}

#[gpui_kit::test]
fn holds_one_observation_across_a_pane_switch_within_the_same_worktree(cx: &mut TestAppContext) {
    let panel = Panel::new(cx, answering);
    panel.point_at("t", 1, cx);
    panel.point_at("t", 1, cx);
    assert_eq!(panel.fake.operations(), ["watch"]);
    // A different epoch is a different connection: the old watch ends.
    panel.point_at("t", 2, cx);
    assert_eq!(panel.fake.operations(), ["watch", "unwatch", "watch"]);
}

#[gpui_kit::test]
fn stages_a_row_and_says_nothing_when_the_refreshed_row_shows_it(cx: &mut TestAppContext) {
    let panel = Panel::new(cx, answering);
    panel.point_at("t", 1, cx);
    let entry = panel.entry(cx);
    panel.panel.update(cx, |panel, cx| {
        panel.mutate_file(&entry, Target::Unstaged, MutationKind::StageFile, None, cx)
    });
    cx.run_until_parked();
    let sent = panel.fake.requests.borrow().last().unwrap().clone();
    assert_eq!(sent.operation, "mutate");
    assert_eq!(sent.mutation, "stageFile");
    assert_eq!(sent.path, b"a.txt");
    assert!(panel.messages.borrow().is_empty());
    let generation = panel
        .panel
        .read_with(cx, |panel, cx| panel.status(cx).unwrap().generation);
    assert_eq!(generation, 2);
}

#[gpui_kit::test]
fn mints_a_discard_token_only_after_confirmation(cx: &mut TestAppContext) {
    let panel = Panel::new(cx, answering);
    panel.point_at("t", 1, cx);
    let pending = panel.pending_discard(cx).unwrap();
    assert_eq!(panel.fake.operations(), ["watch"]);
    panel
        .panel
        .update(cx, |panel, cx| panel.confirm_discard(pending, cx));
    cx.run_until_parked();
    assert_eq!(
        panel.fake.operations(),
        ["watch", "prepareDiscard", "mutate"]
    );
    assert_eq!(panel.fake.requests.borrow()[2].confirmation_token, "once");
}

#[gpui_kit::test]
fn cancels_a_confirmed_discard_once_the_connection_changed(cx: &mut TestAppContext) {
    let panel = Panel::new(cx, answering);
    panel.point_at("t", 1, cx);
    let pending = panel.pending_discard(cx).unwrap();
    panel.point_at("t", 2, cx);
    panel
        .panel
        .update(cx, |panel, cx| panel.confirm_discard(pending, cx));
    cx.run_until_parked();
    assert_eq!(panel.fake.count("prepareDiscard"), 0);
    assert_eq!(
        *panel.messages.borrow(),
        ["Discard was cancelled because the repository connection changed."]
    );
}

#[gpui_kit::test]
fn offers_no_mutation_while_the_status_is_resynchronizing(cx: &mut TestAppContext) {
    let panel = Panel::new(cx, |command| {
        let mut status = status(1);
        status["authoritative"] = json!(false);
        (command.operation == "watch").then(|| Ok(json!({ "status": status })))
    });
    panel.point_at("t", 1, cx);
    assert!(panel.pending_discard(cx).is_none());
    let entry = panel.entry(cx);
    panel.panel.update(cx, |panel, cx| {
        panel.mutate_file(&entry, Target::Unstaged, MutationKind::StageFile, None, cx)
    });
    cx.run_until_parked();
    assert_eq!(panel.fake.operations(), ["watch"]);
}

#[gpui_kit::test]
fn rejects_an_empty_commit_message_before_invoking_git(cx: &mut TestAppContext) {
    let panel = Panel::new(cx, answering);
    panel.point_at("t", 1, cx);
    panel.run(Work::Commit, cx);
    assert_eq!(panel.fake.operations(), ["watch"]);
    let (error, _, _) = panel.panel.read_with(cx, |panel, _| panel.form_state());
    assert_eq!(error.as_deref(), Some("Enter a commit message."));
}

#[gpui_kit::test]
fn leaves_the_commit_form_behind_with_its_repository(cx: &mut TestAppContext) {
    let panel = Panel::new(cx, answering);
    panel.point_at("t", 1, cx);
    panel.run(Work::Commit, cx);
    panel.type_message("meant for t", cx);
    panel.point_at("u", 1, cx);
    assert_eq!(
        panel.panel.read_with(cx, |panel, _| panel.form_state()),
        (None, None, None)
    );
    assert_eq!(
        panel
            .panel
            .read_with(cx, |panel, cx| panel.message_text(cx)),
        ""
    );
}

#[gpui_kit::test]
fn never_reports_a_push_the_host_could_not_confirm_as_one_that_worked(cx: &mut TestAppContext) {
    let panel = Panel::new(cx, |command| {
        Some(Ok(match command.operation.as_str() {
            "watch" => json!({ "status": status(1) }),
            _ => json!({ "command": { "outcome": "partialOrUnknown", "status": status(1) } }),
        }))
    });
    panel.point_at("t", 1, cx);
    panel.run(Work::Push, cx);
    assert_eq!(panel.fake.operations(), ["watch", "push"]);
    // Nothing claims the push is still running, either.
    assert_eq!(
        panel.panel.read_with(cx, |panel, _| panel.form_state()),
        (
            Some("Push outcome is unknown; check the remote before retrying.".to_owned()),
            None,
            None
        )
    );
}

// The diff tab (`GitDiffSurface.test.tsx`).

fn tab(repository: &str) -> AppTabRecord {
    let mut state = PersistedAppState::default();
    let mut status = snapshot(1);
    status.repository.id = repository.into();
    let entry = status.entries[0].clone();
    open_git_diff_tab(
        &mut state,
        &place(),
        &entry,
        Target::Unstaged,
        &status,
        false,
        || "tab".into(),
    );
    state.app_tabs.remove(0)
}

fn diff_view(panel: &Panel, repository: &str, cx: &mut TestAppContext) -> Entity<GitDiffView> {
    panel.point_at("t", 1, cx);
    let owner = panel.panel.clone();
    let tab = tab(repository);
    let view = cx.new(|cx| GitDiffView::new(owner, tab, cx));
    cx.run_until_parked();
    view
}

fn with_diffs(command: &GitCommand) -> Option<Result<Value, String>> {
    Some(Ok(match command.operation.as_str() {
        "diff" => diff_response("repo", b"a.txt", 1),
        "mutate" => applied(2),
        _ => json!({ "status": status(1) }),
    }))
}

#[gpui_kit::test]
fn opens_a_diff_in_one_request_beside_the_panel_watch(cx: &mut TestAppContext) {
    let panel = Panel::new(cx, with_diffs);
    let view = diff_view(&panel, "repo", cx);
    assert_eq!(panel.fake.operations(), ["watch", "diff"]);
    assert_eq!(view.read_with(cx, |view, _| view.shown()), (true, None));

    // The same status published again owes no second read; a newer one does.
    let watch_id = panel.fake.requests.borrow()[0].watch_id.clone();
    for generation in [1, 2] {
        panel.store.update(cx, |store, cx| {
            store.handle_event(watch_event(&watch_id, "t", generation), cx)
        });
        cx.run_until_parked();
    }
    assert_eq!(panel.fake.count("diff"), 2);

    // Closing the tab keeps the panel's watch.
    drop(view);
    cx.run_until_parked();
    assert_eq!(panel.fake.count("unwatch"), 0);
}

#[gpui_kit::test]
fn fails_closed_without_a_request_for_a_tab_of_another_repository(cx: &mut TestAppContext) {
    let panel = Panel::new(cx, answering);
    let view = diff_view(&panel, "elsewhere", cx);
    assert_eq!(panel.fake.operations(), ["watch"]);
    let (drawn, error) = view.read_with(cx, |view, _| view.shown());
    assert!(!drawn);
    assert_eq!(
        error.as_deref(),
        Some(
            "This diff belongs to a different repository. Return to its workspace or close the tab."
        )
    );
}

#[gpui_kit::test]
fn asks_nothing_for_a_file_the_status_says_no_longer_changed(cx: &mut TestAppContext) {
    let panel = Panel::new(cx, |command| {
        let clean = status_in("repo", 1, vec![]);
        (command.operation == "watch").then(|| Ok(json!({ "status": clean })))
    });
    let view = diff_view(&panel, "repo", cx);
    assert_eq!(panel.fake.operations(), ["watch"]);
    assert_eq!(view.read_with(cx, |view, _| view.shown()), (false, None));
}

#[gpui_kit::test]
fn only_a_slow_diff_says_it_is_loading(cx: &mut TestAppContext) {
    let panel = Panel::new(cx, |command| {
        (command.operation != "diff").then(|| Ok(json!({ "status": status(1) })))
    });
    let view = diff_view(&panel, "repo", cx);
    let shown = |cx: &mut TestAppContext| {
        cx.run_until_parked();
        view.read_with(cx, |view, _| (view.loading(), view.loading_shown()))
    };
    // DelayedLoading's 150 ms: a fast diff never flashes the line.
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(140));
    assert_eq!(shown(cx), (true, false));
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(10));
    assert_eq!(shown(cx), (true, true));
    panel
        .fake
        .release("diff", Ok(diff_response("repo", b"a.txt", 1)));
    assert_eq!(shown(cx), (false, false));
}

#[gpui_kit::test]
fn surfaces_a_failed_shared_bootstrap_instead_of_loading_forever(cx: &mut TestAppContext) {
    let panel = Panel::new(cx, |_| Some(Err("helper gone".into())));
    let view = diff_view(&panel, "repo", cx);
    let (loading, shown) = view.read_with(cx, |view, _| (view.loading(), view.shown()));
    assert!(!loading);
    assert_eq!(shown, (false, Some("helper gone".to_owned())));
}

#[gpui_kit::test]
fn reloads_the_remaining_diff_once_after_a_hunk_is_staged(cx: &mut TestAppContext) {
    let panel = Panel::new(cx, with_diffs);
    let view = diff_view(&panel, "repo", cx);
    view.update(cx, |view, cx| view.stage_hunk(0, cx));
    cx.run_until_parked();
    assert_eq!(panel.fake.operations(), ["watch", "diff", "mutate", "diff"]);
    let mutate = panel.fake.requests.borrow()[2].clone();
    assert_eq!(mutate.mutation, "stageHunk");
    assert_eq!(mutate.hunk_index, 0);
    assert_eq!(mutate.expected_source_generation, "source-1");
}

#[gpui_kit::test]
fn a_released_entry_s_late_refresh_does_not_land_in_its_successor(cx: &mut TestAppContext) {
    let fake = Fake::new(|_| None);
    let store = store(cx, &fake);
    let key = store.update(cx, |store, cx| store.acquire(&scope("t", 1), cx));
    drop(store.update(cx, |store, cx| store.refresh(&key, cx)));
    cx.run_until_parked();
    store.update(cx, |store, cx| store.release(&key, cx));
    let key = store.update(cx, |store, cx| store.acquire(&scope("t", 1), cx));
    cx.run_until_parked();
    fake.release("status", Err("from the old connection".into()));
    cx.run_until_parked();
    let state = store.read_with(cx, |store, _| store.state(&key));
    assert_eq!(state.error, None);
    assert!(state.loading);
}

#[gpui_kit::test]
fn a_diff_newer_than_the_observation_moves_the_observation(cx: &mut TestAppContext) {
    let panel = Panel::new(cx, |command| {
        Some(Ok(match command.operation.as_str() {
            "diff" => diff_response("repo", b"a.txt", 4),
            _ => json!({ "status": status(1) }),
        }))
    });
    let _view = diff_view(&panel, "repo", cx);
    let generation = panel
        .panel
        .read_with(cx, |panel, cx| panel.status(cx).unwrap().generation);
    assert_eq!(generation, 4);
    assert_eq!(panel.fake.count("diff"), 1);
}

#[test]
fn assembles_withheld_bodies_in_order_and_refuses_gaps() {
    let size = |size| ContentRef {
        size,
        digest: "d".into(),
    };
    let chunk = |side: u8, offset: u64, data: &[u8]| {
        let mut frame = vec![1, side];
        frame.extend_from_slice(&offset.to_be_bytes());
        frame.extend_from_slice(data);
        frame
    };
    let mut bodies = DeferredBodies::new(Some(&size(5)), Some(&size(3))).unwrap();
    assert_eq!(bodies.accept(&chunk(1, 0, b"ba")), Ok(false));
    assert_eq!(bodies.accept(&chunk(1, 2, b"se\n")), Ok(false));
    assert!(
        bodies.accept(&[2]).is_err(),
        "the new side is still missing"
    );
    assert_eq!(bodies.accept(&chunk(2, 0, b"ne\n")), Ok(false));
    assert_eq!(bodies.accept(&[2]), Ok(true));
    let response = model::response(diff_response("repo", b"a.txt", 1)).unwrap();
    let mut diff = model::response_diff(response, &unstaged_a()).unwrap().0;
    bodies.fill(&mut diff);
    assert_eq!(
        (diff.old_content, diff.new_content),
        (b"base\n".to_vec(), b"ne\n".to_vec())
    );

    let mut bodies = DeferredBodies::new(Some(&size(4)), None).unwrap();
    assert!(bodies.accept(&chunk(1, 1, b"x")).is_err());
    assert!(bodies.accept(&chunk(2, 0, b"x")).is_err());
    assert!(DeferredBodies::new(Some(&size(0)), None).is_err());
}

#[test]
fn draws_a_file_with_a_lone_carriage_return() {
    let patch = whole_file_patch("log.txt", "a\rb\nc\n", "a\rB\nc\n");
    let files = gpui_kit::component::diff::DiffFile::parse(&patch);
    assert!(files.is_ok(), "{patch:?}");
}

fn commit_answers(
    commit: Value,
) -> impl FnMut(&GitCommand) -> Option<Result<Value, String>> + 'static {
    move |command| {
        Some(Ok(match command.operation.as_str() {
            "watch" | "status" => json!({ "status": status(1) }),
            "commit" => commit.clone(),
            _ => {
                json!({ "command": { "outcome": "applied", "pushTarget": "origin/main", "status": status(3) } })
            }
        }))
    }
}

#[gpui_kit::test]
fn commits_and_then_pushes_against_the_status_the_commit_left(cx: &mut TestAppContext) {
    let panel = Panel::new(cx, commit_answers(applied(2)));
    panel.point_at("t", 1, cx);
    panel.type_message("message", cx);
    panel.run(Work::CommitAndPush, cx);
    assert_eq!(panel.fake.operations(), ["watch", "commit", "push"]);
    assert_eq!(
        panel.fake.requests.borrow()[2].expected_status_generation,
        "2"
    );
    let (error, note, progress) = panel.panel.read_with(cx, |panel, _| panel.form_state());
    assert_eq!(error, None);
    assert_eq!(note.as_deref(), Some("Committed. Pushed to origin/main."));
    assert_eq!(progress, None);
}

#[gpui_kit::test]
fn does_not_talk_to_the_remote_when_the_chained_commit_did_not_happen(cx: &mut TestAppContext) {
    let panel = Panel::new(
        cx,
        commit_answers(
            json!({ "command": { "outcome": "notApplied", "exitCode": 1, "status": status(1) } }),
        ),
    );
    panel.point_at("t", 1, cx);
    panel.type_message("message", cx);
    panel.run(Work::CommitAndPush, cx);
    assert_eq!(panel.fake.operations(), ["watch", "commit"]);
    let (error, _, progress) = panel.panel.read_with(cx, |panel, _| panel.form_state());
    assert_eq!(error.as_deref(), Some("Git did not create a commit."));
    assert_eq!(progress, None);
}

#[gpui_kit::test]
fn does_not_push_once_the_commit_left_the_status_resynchronizing(cx: &mut TestAppContext) {
    let mut resynchronizing = status(2);
    resynchronizing["authoritative"] = json!(false);
    let panel = Panel::new(
        cx,
        commit_answers(json!({ "command": { "outcome": "applied", "status": resynchronizing } })),
    );
    panel.point_at("t", 1, cx);
    panel.type_message("message", cx);
    panel.run(Work::CommitAndPush, cx);
    assert_eq!(panel.fake.operations(), ["watch", "commit"]);
    let (error, _, progress) = panel.panel.read_with(cx, |panel, _| panel.form_state());
    assert_eq!(error.as_deref(), Some("Git is not available right now."));
    assert_eq!(progress, None);
}

#[gpui_kit::test]
fn follows_the_tab_to_another_file_on_the_same_observation(cx: &mut TestAppContext) {
    let panel = Panel::new(cx, |command| {
        Some(Ok(match command.operation.as_str() {
            "diff" if command.path == b"a.txt" => diff_response("repo", b"a.txt", 1),
            "diff" => {
                let mut response = diff_response("repo", b"dir/b.txt", 1);
                response["diff"]["displayPath"] = json!("dir/b.txt");
                response["status"] = status_in(
                    "repo",
                    1,
                    vec![
                        entry(A, "a.txt", "none", "modified"),
                        entry(B, "dir/b.txt", "none", "modified"),
                    ],
                );
                response
            }
            _ => json!({ "status": status_in("repo", 1, vec![
                entry(A, "a.txt", "none", "modified"),
                entry(B, "dir/b.txt", "none", "modified"),
            ]) }),
        }))
    });
    let view = diff_view(&panel, "repo", cx);
    let mut repointed = tab("repo");
    repointed.git_path = Some(B.into());
    repointed.resource = "unstaged:dir/b.txt".into();
    view.update(cx, |view, cx| view.set_tab(&repointed, cx));
    cx.run_until_parked();
    assert_eq!(panel.fake.operations(), ["watch", "diff", "diff"]);
    assert_eq!(panel.fake.requests.borrow()[2].path, b"dir/b.txt");
    assert_eq!(view.read_with(cx, |view, _| view.shown()), (true, None));
}

#[gpui_kit::test]
fn asks_nothing_of_a_host_that_is_not_the_tab_s_own(cx: &mut TestAppContext) {
    let panel = Panel::new(cx, with_diffs);
    panel.point_at("t", 1, cx);
    let mut elsewhere = tab("repo");
    elsewhere.host_profile_id = "other-host".into();
    let owner = panel.panel.clone();
    let view = cx.new(|cx| GitDiffView::new(owner, elsewhere, cx));
    cx.run_until_parked();
    // Only the panel's own watch: the tab waits for its host.
    assert_eq!(panel.fake.operations(), ["watch"]);
    assert_eq!(view.read_with(cx, |view, _| view.shown()), (false, None));
}
