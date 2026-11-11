//! The Git requests, over `desktop-core` directly. Replaces the Tauri client in
//! `features/git/api.ts`: each request is addressed to one scope, a read is
//! cancelled at the host when the future waiting for it is dropped, and a
//! mutation is sent exactly once, because its outcome is never safe to replay.

use std::{rc::Rc, sync::Arc};

use desktop_core::{
    connection::{
        git::GitCommand,
        git_content::{GitDiffContentCommand, GitDiffContentSideCommand},
    },
    platform::{EventBody, EventSink},
};
use futures::{StreamExt, channel::mpsc, future::BoxFuture};
use serde_json::Value;
use uuid::Uuid;

use crate::connection_ui::{backend::TerminalBackend, bridge_events::ChannelSink};

use super::model::{
    self, CommandResult, DeferredBodies, Diff, DiffRequest, GitScope, Mutation, StatusSnapshot,
    path_bytes, target_name,
};

/// How Git reaches a host's helper.
pub trait GitTransport {
    fn request(
        &self,
        client_id: String,
        command: GitCommand,
    ) -> BoxFuture<'static, Result<Value, String>>;
    fn cancel(&self, client_id: String, operation_id: String);
    /// Starts reading withheld diff bodies into `sink`, answering the read's id.
    fn read_content(
        &self,
        command: GitDiffContentCommand,
        sink: Arc<dyn EventSink>,
    ) -> BoxFuture<'static, Result<String, String>>;
    fn cancel_content(&self, client_id: String, read_id: String);
}

pub struct BackendTransport(pub Arc<dyn TerminalBackend>);

impl GitTransport for BackendTransport {
    fn request(
        &self,
        client_id: String,
        command: GitCommand,
    ) -> BoxFuture<'static, Result<Value, String>> {
        self.0.git_request(client_id, command)
    }

    fn cancel(&self, client_id: String, operation_id: String) {
        let _ = self.0.cancel_git_request(client_id, operation_id);
    }

    fn read_content(
        &self,
        command: GitDiffContentCommand,
        sink: Arc<dyn EventSink>,
    ) -> BoxFuture<'static, Result<String, String>> {
        self.0.read_git_diff_content(command, sink)
    }

    fn cancel_content(&self, client_id: String, read_id: String) {
        let _ = self.0.cancel_git_diff_content(client_id, read_id);
    }
}

pub type Transport = Rc<dyn GitTransport>;

fn command(scope: &GitScope, operation: &str) -> GitCommand {
    GitCommand {
        operation: operation.to_owned(),
        operation_id: Uuid::new_v4().to_string(),
        root: scope.root_path.clone(),
        root_token: scope.root_token.clone(),
        expected_server_identity: scope.server_identity.clone(),
        repository_id: String::new(),
        expected_status_generation: String::new(),
        expected_source_generation: String::new(),
        connection_epoch: scope.terminal_epoch.to_string(),
        path: Vec::new(),
        original_path: Vec::new(),
        diff_target: String::new(),
        mutation: String::new(),
        hunk_index: 0,
        confirmation_token: String::new(),
        commit_message: String::new(),
        watch_id: String::new(),
    }
}

/// Cancels a read at the host if whoever waits for it stops waiting.
struct CancelOnDrop {
    transport: Transport,
    client_id: String,
    id: String,
    content: bool,
    armed: bool,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let (client, id) = (self.client_id.clone(), self.id.clone());
        if self.content {
            self.transport.cancel_content(client, id);
        } else {
            self.transport.cancel(client, id);
        }
    }
}

async fn read(
    transport: &Transport,
    scope: &GitScope,
    command: GitCommand,
) -> Result<Value, String> {
    let mut guard = CancelOnDrop {
        transport: transport.clone(),
        client_id: scope.client_id.clone(),
        id: command.operation_id.clone(),
        content: false,
        armed: true,
    };
    let answer = transport.request(scope.client_id.clone(), command).await;
    guard.armed = false;
    answer
}

pub async fn status(transport: Transport, scope: GitScope) -> Result<StatusSnapshot, String> {
    let answer = read(&transport, &scope, command(&scope, "status")).await?;
    model::response_status(model::response(answer)?)
}

pub async fn watch(
    transport: Transport,
    scope: GitScope,
    watch_id: String,
) -> Result<StatusSnapshot, String> {
    let request = GitCommand {
        watch_id,
        ..command(&scope, "watch")
    };
    let answer = read(&transport, &scope, request).await?;
    model::response_status(model::response(answer)?)
}

/// Fire and forget: a release has nobody left to tell about a failure.
pub fn unwatch(
    transport: &Transport,
    scope: &GitScope,
    watch_id: String,
) -> BoxFuture<'static, ()> {
    let answer = transport.request(
        scope.client_id.clone(),
        GitCommand {
            watch_id,
            ..command(scope, "unwatch")
        },
    );
    Box::pin(async move {
        let _ = answer.await;
    })
}

/// One round trip: the diff and the authoritative status it was read against.
/// Bodies the host withheld from the control lane are then read over the bulk
/// lane, where they cannot delay a keystroke.
pub async fn diff(
    transport: Transport,
    scope: GitScope,
    request: DiffRequest,
) -> Result<(Diff, StatusSnapshot), String> {
    let wire = GitCommand {
        repository_id: request.repository_id.clone(),
        path: path_bytes(&request.path),
        original_path: request
            .original_path
            .as_deref()
            .map(path_bytes)
            .unwrap_or_default(),
        diff_target: target_name(request.target).to_owned(),
        ..command(&scope, "diff")
    };
    let answer = read(&transport, &scope, wire).await?;
    let (mut diff, status) = model::response_diff(model::response(answer)?, &request)?;
    if diff.old_ref.is_some() || diff.new_ref.is_some() {
        read_bodies(&transport, &scope, &mut diff).await?;
    }
    Ok((diff, status))
}

/// Both withheld sides over one bulk read; a failure on either side stops the
/// other rather than leaving it streaming to nobody.
async fn read_bodies(
    transport: &Transport,
    scope: &GitScope,
    diff: &mut Diff,
) -> Result<(), String> {
    let mut bodies = DeferredBodies::new(diff.old_ref.as_ref(), diff.new_ref.as_ref())?;
    let side = |content: &Option<model::ContentRef>| {
        content.as_ref().map(|content| GitDiffContentSideCommand {
            content_digest: content.digest.clone(),
            size: content.size.to_string(),
        })
    };
    let request = GitDiffContentCommand {
        client_id: scope.client_id.clone(),
        profile_id: scope.host_profile_id.clone(),
        expected_server_identity: scope.server_identity.clone(),
        connection_epoch: scope.terminal_epoch.to_string(),
        root: scope.root_path.clone(),
        root_token: scope.root_token.clone(),
        repository_id: diff.repository.id.clone(),
        path: path_bytes(&diff.path),
        original_path: diff
            .original_path
            .as_deref()
            .map(path_bytes)
            .unwrap_or_default(),
        diff_target: target_name(diff.target).to_owned(),
        old: side(&diff.old_ref),
        new: side(&diff.new_ref),
    };
    let (sender, mut frames) = mpsc::unbounded();
    let read_id = transport
        .read_content(request, Arc::new(ChannelSink(sender)))
        .await?;
    let mut guard = CancelOnDrop {
        transport: transport.clone(),
        client_id: scope.client_id.clone(),
        id: read_id,
        content: true,
        armed: true,
    };
    while let Some(body) = frames.next().await {
        let EventBody::Raw(frame) = body else {
            continue;
        };
        if bodies.accept(&frame)? {
            guard.armed = false;
            bodies.fill(diff);
            return Ok(());
        }
    }
    Err("Git diff content read failed.".into())
}

fn mutation_command(
    scope: &GitScope,
    operation: &str,
    repository_id: &str,
    mutation: &Mutation,
) -> GitCommand {
    GitCommand {
        repository_id: repository_id.to_owned(),
        path: path_bytes(&mutation.path),
        original_path: mutation
            .original_path
            .as_deref()
            .map(path_bytes)
            .unwrap_or_default(),
        mutation: mutation.kind.wire().to_owned(),
        diff_target: target_name(mutation.target).to_owned(),
        expected_status_generation: mutation.expected_status_generation.to_string(),
        expected_source_generation: mutation.expected_source_generation.clone(),
        hunk_index: mutation.hunk_index.unwrap_or_default(),
        confirmation_token: mutation.confirmation_token.clone().unwrap_or_default(),
        ..command(scope, operation)
    }
}

/// Mints the one-time host token a discard requires.
pub async fn prepare_discard(
    transport: Transport,
    scope: GitScope,
    repository_id: String,
    mutation: Mutation,
) -> Result<String, String> {
    if !mutation.kind.is_discard() {
        return Err("Only discard operations require confirmation tokens.".into());
    }
    let wire = mutation_command(&scope, "prepareDiscard", &repository_id, &mutation);
    let answer = transport.request(scope.client_id.clone(), wire).await?;
    model::response_confirmation(model::response(answer)?)
}

/// Never replayed: once Git has started, uncertainty is a command outcome,
/// not a transport error.
pub async fn mutate(
    transport: Transport,
    scope: GitScope,
    repository_id: String,
    mutation: Mutation,
) -> Result<CommandResult, String> {
    if mutation.kind.is_discard() && mutation.confirmation_token.is_none() {
        return Err("Discard requires confirmation.".into());
    }
    let wire = mutation_command(&scope, "mutate", &repository_id, &mutation);
    let answer = transport.request(scope.client_id.clone(), wire).await?;
    model::response_command(model::response(answer)?)
}

/// Sent exactly once, like a mutation.
pub async fn commit(
    transport: Transport,
    scope: GitScope,
    repository_id: String,
    expected_status_generation: u64,
    message: String,
) -> Result<CommandResult, String> {
    if message.trim().is_empty() {
        return Err("Enter a commit message.".into());
    }
    let wire = GitCommand {
        repository_id,
        expected_status_generation: expected_status_generation.to_string(),
        commit_message: message,
        ..command(&scope, "commit")
    };
    let answer = transport.request(scope.client_id.clone(), wire).await?;
    model::response_command(model::response(answer)?)
}

/// Publishes the current branch to the upstream the host resolves. Sent
/// exactly once: once Git has spoken to a remote, a retry could publish twice.
pub async fn push(
    transport: Transport,
    scope: GitScope,
    repository_id: String,
    expected_status_generation: u64,
) -> Result<CommandResult, String> {
    let wire = GitCommand {
        repository_id,
        expected_status_generation: expected_status_generation.to_string(),
        ..command(&scope, "push")
    };
    let answer = transport.request(scope.client_id.clone(), wire).await?;
    model::response_command(model::response(answer)?)
}
