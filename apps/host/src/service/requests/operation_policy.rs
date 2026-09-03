use tmux_agent_protocol::v1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationValue {
    Known(v1::Operation),
    Unknown(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    ReadOnly,
    Mutation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lane {
    Control,
    Bulk,
    Either,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scheduling {
    Inline,
    Detached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Handler {
    Unsupported,
    Snapshot,
    Terminal,
    TmuxAction,
    ActiveRoot,
    Filesystem,
    Git,
    Agent,
    Voice,
    Daemon,
    Test,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdmissionError {
    ReadOnlyMutation,
    BulkConnectionRequired,
    ControlConnectionRequired,
}

impl AdmissionError {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::ReadOnlyMutation => "helper_incompatible",
            Self::BulkConnectionRequired => "bulk_connection_required",
            Self::ControlConnectionRequired => "control_connection_required",
        }
    }

    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::ReadOnlyMutation => "host is read-only until the helper is upgraded",
            Self::BulkConnectionRequired => {
                "file bodies are allowed only on an independent bulk connection"
            }
            Self::ControlConnectionRequired => {
                "control operations are not allowed on a bulk connection"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OperationPolicy {
    pub(crate) operation: OperationValue,
    pub(crate) access: Access,
    pub(crate) lane: Lane,
    pub(crate) scheduling: Scheduling,
    pub(crate) handler: Handler,
}

impl OperationPolicy {
    pub(crate) fn for_raw(raw: i32) -> Self {
        let Ok(operation) = v1::Operation::try_from(raw) else {
            return Self {
                operation: OperationValue::Unknown(raw),
                access: Access::ReadOnly,
                lane: Lane::Either,
                scheduling: Scheduling::Inline,
                handler: Handler::Unsupported,
            };
        };

        let (access, lane, scheduling, handler) = match operation {
            v1::Operation::Unspecified => (
                Access::ReadOnly,
                Lane::Control,
                Scheduling::Inline,
                Handler::Unsupported,
            ),
            v1::Operation::FullSnapshot | v1::Operation::Subscribe | v1::Operation::Resync => (
                Access::ReadOnly,
                Lane::Control,
                Scheduling::Inline,
                Handler::Snapshot,
            ),
            v1::Operation::ShutdownDaemon => (
                Access::Mutation,
                Lane::Control,
                Scheduling::Inline,
                Handler::Daemon,
            ),
            v1::Operation::AttachTerminal
            | v1::Operation::TerminalInput
            | v1::Operation::ResizeTerminal
            | v1::Operation::SetTerminalVisibility
            | v1::Operation::RequestTerminalSeed
            | v1::Operation::SelectTerminalSession => (
                Access::Mutation,
                Lane::Control,
                Scheduling::Inline,
                Handler::Terminal,
            ),
            v1::Operation::TmuxAction => (
                Access::Mutation,
                Lane::Control,
                Scheduling::Inline,
                Handler::TmuxAction,
            ),
            // Detached, unlike every other terminal operation: a scrollback
            // capture is thousands of lines the user is waiting on, and running
            // it inline would hold the reader loop — and every keystroke behind
            // it — for the length of that capture. Read-only, because it
            // photographs and changes nothing.
            v1::Operation::RequestTerminalHistory => (
                Access::ReadOnly,
                Lane::Control,
                Scheduling::Detached,
                Handler::Terminal,
            ),
            v1::Operation::ResolveActiveRoot | v1::Operation::ResolveTerminalFile => (
                Access::ReadOnly,
                Lane::Control,
                Scheduling::Detached,
                Handler::ActiveRoot,
            ),
            v1::Operation::ListDirectory => (
                Access::ReadOnly,
                Lane::Control,
                Scheduling::Detached,
                Handler::Filesystem,
            ),
            // Detached deliberately: the reader loop must stay free to admit
            // this request's own Cancel while its bounded body is streaming.
            v1::Operation::OpenFileStream => (
                Access::ReadOnly,
                Lane::Bulk,
                Scheduling::Detached,
                Handler::Filesystem,
            ),
            // Answered with a refusal naming `OpenFileStream`, so an older
            // desktop is told what to use rather than silently served by a
            // second code path that had already drifted from the first. Still
            // classified as the read it asks to be: a read-only connection
            // must get the same answer as any other.
            //
            // `Either`, because the desktop this refusal is *for* issued
            // `ReadFile` on the **bulk** lane. Classifying it `Control` meant
            // that desktop was turned away at admission with
            // `control_connection_required` and never reached the refusal that
            // names its replacement — the entire reason the operation was kept
            // rather than deleted. `Inline`, because the answer is a constant:
            // detaching it spent a task spawn and the dispatcher's deliberate
            // 1 ms handoff on a request that touches nothing.
            v1::Operation::ReadFile => (
                Access::ReadOnly,
                Lane::Either,
                Scheduling::Inline,
                Handler::Filesystem,
            ),
            v1::Operation::WatchDirectory
            | v1::Operation::UnwatchDirectory
            | v1::Operation::FileMutation
            | v1::Operation::WriteFile => (
                Access::Mutation,
                Lane::Control,
                Scheduling::Detached,
                Handler::Filesystem,
            ),
            v1::Operation::StartDownload
            | v1::Operation::ReadDownloadChunk
            | v1::Operation::CancelDownload
            | v1::Operation::BeginFileWrite
            | v1::Operation::WriteFileChunk
            | v1::Operation::CommitFileWrite
            | v1::Operation::CancelFileWrite
            | v1::Operation::PrepareTerminalUpload
            | v1::Operation::WriteTerminalUploadChunk
            | v1::Operation::CommitTerminalUpload
            | v1::Operation::CancelTerminalUpload => (
                Access::Mutation,
                Lane::Bulk,
                Scheduling::Inline,
                Handler::Filesystem,
            ),
            v1::Operation::ReconcileTerminalUpload => (
                Access::ReadOnly,
                Lane::Bulk,
                Scheduling::Inline,
                Handler::Filesystem,
            ),
            v1::Operation::GitStatus | v1::Operation::GitDiff => (
                Access::ReadOnly,
                Lane::Control,
                Scheduling::Detached,
                Handler::Git,
            ),
            // Diff bodies are the only Git payload large enough to matter to
            // terminal latency, so they alone take the independent bulk lane. A
            // connection that has no bulk lane never sees one of these: its
            // diffs are inlined instead. Detached so a body read cannot block
            // the connection's frame loop, including the `Cancel` frame that
            // would stop it.
            v1::Operation::GitDiffContent => (
                Access::ReadOnly,
                Lane::Bulk,
                Scheduling::Detached,
                Handler::Git,
            ),
            v1::Operation::WatchGit
            | v1::Operation::UnwatchGit
            | v1::Operation::PrepareGitDiscard
            | v1::Operation::GitMutation
            | v1::Operation::GitCommit
            | v1::Operation::GitPush => (
                Access::Mutation,
                Lane::Control,
                Scheduling::Detached,
                Handler::Git,
            ),
            v1::Operation::AgentSnapshot => (
                Access::ReadOnly,
                Lane::Control,
                Scheduling::Inline,
                Handler::Agent,
            ),
            v1::Operation::AgentAction
            | v1::Operation::AgentMarkSeen
            | v1::Operation::AgentHookIngest
            | v1::Operation::AgentHookManagement
            | v1::Operation::AgentHostNaming => (
                Access::Mutation,
                Lane::Control,
                Scheduling::Inline,
                Handler::Agent,
            ),
            // A readiness probe and a session registration touch in-memory
            // state only, so they answer inline like the agent operations.
            v1::Operation::VoiceStatus | v1::Operation::VoiceSession => (
                Access::ReadOnly,
                Lane::Control,
                Scheduling::Inline,
                Handler::Voice,
            ),
            // Detached: each spawns a process and writes the cache directory,
            // and none may hold the reader loop — a transcription is seconds
            // long and its own Cancel arrives on this connection. Mutation, so
            // a read-only host never starts a download.
            v1::Operation::VoiceProvision
            | v1::Operation::VoiceTranscribe
            | v1::Operation::VoiceSpeak => (
                Access::Mutation,
                Lane::Control,
                Scheduling::Detached,
                Handler::Voice,
            ),
            v1::Operation::TestDelay
            | v1::Operation::TestInjectGap
            | v1::Operation::TestOverflow => (
                Access::ReadOnly,
                Lane::Control,
                Scheduling::Inline,
                Handler::Test,
            ),
        };

        Self {
            operation: OperationValue::Known(operation),
            access,
            lane,
            scheduling,
            handler,
        }
    }

    pub(crate) fn known_operation(self) -> Option<v1::Operation> {
        match self.operation {
            OperationValue::Known(operation) => Some(operation),
            OperationValue::Unknown(_) => None,
        }
    }

    pub(crate) fn admission_error(
        self,
        read_only: bool,
        bulk_connection: bool,
    ) -> Option<AdmissionError> {
        if read_only && self.access == Access::Mutation {
            return Some(AdmissionError::ReadOnlyMutation);
        }
        match (self.lane, bulk_connection) {
            (Lane::Bulk, false) => Some(AdmissionError::BulkConnectionRequired),
            (Lane::Control, true) => Some(AdmissionError::ControlConnectionRequired),
            (Lane::Control, false)
            | (Lane::Bulk, true)
            | (Lane::Either, false)
            | (Lane::Either, true) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::serve_with_shutdown;
    use tmux_agent_protocol::{
        HOST_CAPABILITIES, envelope, read_frame, v1::envelope::Payload, write_frame,
    };
    use tokio::{net::UnixStream, time::timeout};

    fn assert_policy(
        operation: v1::Operation,
        access: Access,
        lane: Lane,
        scheduling: Scheduling,
        handler: Handler,
    ) {
        assert_eq!(
            OperationPolicy::for_raw(operation.into()),
            OperationPolicy {
                operation: OperationValue::Known(operation),
                access,
                lane,
                scheduling,
                handler,
            },
            "{}",
            operation.as_str_name(),
        );
    }

    /// Every operation's expected policy, in one table.
    ///
    /// A table rather than a run of assertions so that the coverage test below
    /// can read the same list the compatibility test asserts. Two hand-written
    /// lists of the same operations would be the very drift being guarded.
    fn expected_policies() -> Vec<(v1::Operation, Access, Lane, Scheduling, Handler)> {
        use Access::{Mutation as M, ReadOnly as R};
        use Handler::{
            ActiveRoot as AR, Agent as AH, Daemon as DH, Filesystem as FH, Git as GH,
            Snapshot as SH, Terminal as TH, Test as XH, TmuxAction as MH, Unsupported as UH,
            Voice as VH,
        };
        use Lane::{Bulk as B, Control as C, Either as E};
        use Scheduling::{Detached as Dd, Inline as I};
        use v1::Operation::*;

        vec![
            (Unspecified, R, C, I, UH),
            (FullSnapshot, R, C, I, SH),
            (Subscribe, R, C, I, SH),
            (AttachTerminal, M, C, I, TH),
            (TerminalInput, M, C, I, TH),
            (ResizeTerminal, M, C, I, TH),
            (Resync, R, C, I, SH),
            // Shutdown is intercepted before request registration, but remains a mutation.
            (ShutdownDaemon, M, C, I, DH),
            (TmuxAction, M, C, I, MH),
            (SetTerminalVisibility, M, C, I, TH),
            (RequestTerminalSeed, M, C, I, TH),
            (RequestTerminalHistory, R, C, Dd, TH),
            (ResolveActiveRoot, R, C, Dd, AR),
            (ResolveTerminalFile, R, C, Dd, AR),
            (ListDirectory, R, C, Dd, FH),
            (WatchDirectory, M, C, Dd, FH),
            (UnwatchDirectory, M, C, Dd, FH),
            (FileMutation, M, C, Dd, FH),
            // `Either`, and it matters: the desktop this refusal exists for issued
            // `ReadFile` on the *bulk* lane, so a `Control` classification turned
            // it away at admission and it never saw the message naming its
            // replacement.
            (ReadFile, R, E, I, FH),
            (OpenFileStream, R, B, Dd, FH),
            (WriteFile, M, C, Dd, FH),
            (StartDownload, M, B, I, FH),
            (ReadDownloadChunk, M, B, I, FH),
            (CancelDownload, M, B, I, FH),
            (BeginFileWrite, M, B, I, FH),
            (WriteFileChunk, M, B, I, FH),
            (CommitFileWrite, M, B, I, FH),
            (CancelFileWrite, M, B, I, FH),
            (GitStatus, R, C, Dd, GH),
            (WatchGit, M, C, Dd, GH),
            (UnwatchGit, M, C, Dd, GH),
            (GitDiff, R, C, Dd, GH),
            (GitDiffContent, R, B, Dd, GH),
            (PrepareGitDiscard, M, C, Dd, GH),
            (GitMutation, M, C, Dd, GH),
            (GitCommit, M, C, Dd, GH),
            (GitPush, M, C, Dd, GH),
            (AgentSnapshot, R, C, I, AH),
            (AgentAction, M, C, I, AH),
            (AgentMarkSeen, M, C, I, AH),
            (AgentHookIngest, M, C, I, AH),
            (AgentHookManagement, M, C, I, AH),
            (PrepareTerminalUpload, M, B, I, FH),
            (WriteTerminalUploadChunk, M, B, I, FH),
            (CommitTerminalUpload, M, B, I, FH),
            (CancelTerminalUpload, M, B, I, FH),
            (ReconcileTerminalUpload, R, B, I, FH),
            (AgentHostNaming, M, C, I, AH),
            (SelectTerminalSession, M, C, I, TH),
            (VoiceStatus, R, C, I, VH),
            (VoiceProvision, M, C, Dd, VH),
            (VoiceTranscribe, M, C, Dd, VH),
            (VoiceSpeak, M, C, Dd, VH),
            (VoiceSession, R, C, I, VH),
            (TestDelay, R, C, I, XH),
            (TestInjectGap, R, C, I, XH),
            (TestOverflow, R, C, I, XH),
        ]
    }

    #[test]
    fn every_generated_operation_has_the_compatible_policy() {
        for (operation, access, lane, scheduling, handler) in expected_policies() {
            assert_policy(operation, access, lane, scheduling, handler);
        }
    }

    /// The list above is hand-written, so this is what makes its name true.
    ///
    /// The production `match` is compiler-exhaustive, so a new operation cannot
    /// be added without *a* policy — but nothing forced it into the
    /// compatibility assertions, and an operation with a policy and no
    /// assertion is exactly what "compatible" was supposed to mean. Two
    /// operations were added to this schema in one round by two authors who
    /// each picked the same number; a list that silently stops covering the
    /// enum is the next version of that.
    ///
    /// An earlier version of this test asserted a hard-coded count of 49, which
    /// does not check what the name says: bumping the count is exactly as easy
    /// as adding the operation, and the list could still stop covering the
    /// enum. This names the operations that are missing instead.
    #[test]
    fn the_compatibility_list_covers_every_operation_the_enum_accepts() {
        // The generated enum has no iterator, so this asks it directly. The
        // bound is above every assigned number and below anything plausible.
        let generated: Vec<v1::Operation> = (0..=255_i32)
            .filter_map(|value| v1::Operation::try_from(value).ok())
            .collect();
        let asserted: Vec<v1::Operation> =
            expected_policies().into_iter().map(|row| row.0).collect();
        let missing: Vec<&str> = generated
            .iter()
            .filter(|operation| !asserted.contains(*operation))
            .map(|operation| operation.as_str_name())
            .collect();
        assert!(
            missing.is_empty(),
            "these operations have a policy but no compatibility assertion; \
             add them to `every_generated_operation_has_the_compatible_policy`: {missing:?}",
        );
    }

    #[test]
    fn unknown_numeric_operations_remain_distinct_from_unspecified_and_each_other() {
        let first = OperationPolicy::for_raw(90);
        let second = OperationPolicy::for_raw(91);
        let unspecified = OperationPolicy::for_raw(v1::Operation::Unspecified.into());

        assert_eq!(first.operation, OperationValue::Unknown(90));
        assert_eq!(second.operation, OperationValue::Unknown(91));
        assert_ne!(first.operation, second.operation);
        assert_ne!(first.operation, unspecified.operation);
        assert_eq!(first.handler, Handler::Unsupported);
        assert_eq!(first.admission_error(false, true), None);
    }

    #[test]
    fn admission_enforces_access_and_exact_connection_lane() {
        let naming = OperationPolicy::for_raw(v1::Operation::AgentHostNaming.into());
        assert_eq!(
            naming.admission_error(true, false),
            Some(AdmissionError::ReadOnlyMutation)
        );
        assert_eq!(naming.admission_error(false, false), None);
        assert_eq!(
            naming.admission_error(false, true),
            Some(AdmissionError::ControlConnectionRequired)
        );

        let download = OperationPolicy::for_raw(v1::Operation::StartDownload.into());
        assert_eq!(download.admission_error(false, true), None);
        assert_eq!(
            download.admission_error(false, false),
            Some(AdmissionError::BulkConnectionRequired)
        );

        let shutdown = OperationPolicy::for_raw(v1::Operation::ShutdownDaemon.into());
        assert_eq!(
            shutdown.admission_error(true, false),
            Some(AdmissionError::ReadOnlyMutation)
        );

        // A read-only host must not start a ~487 MB download, but may still say
        // whether voice is set up.
        let provision = OperationPolicy::for_raw(v1::Operation::VoiceProvision.into());
        assert_eq!(
            provision.admission_error(true, false),
            Some(AdmissionError::ReadOnlyMutation)
        );
        let status = OperationPolicy::for_raw(v1::Operation::VoiceStatus.into());
        assert_eq!(status.admission_error(true, false), None);
        assert_eq!(
            status.admission_error(false, true),
            Some(AdmissionError::ControlConnectionRequired)
        );
    }

    #[tokio::test]
    async fn read_only_connection_rejects_host_naming_and_daemon_shutdown() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(serve_with_shutdown(server, Some(shutdown_tx)));
        write_frame(
            &mut client,
            &envelope(
                1,
                0,
                Payload::ClientHello(v1::ClientHello {
                    desktop_version: "read-only-policy-test".into(),
                    requested_capabilities: HOST_CAPABILITIES,
                    expected_helper_version: "incompatible-helper-version".into(),
                    ..Default::default()
                }),
            ),
        )
        .await
        .unwrap();
        let hello = timeout(std::time::Duration::from_secs(3), read_frame(&mut client))
            .await
            .expect("server hello timed out")
            .unwrap()
            .unwrap();
        let Some(Payload::ServerHello(hello)) = hello.payload else {
            panic!("expected hello")
        };
        assert!(hello.read_only);

        for (request_id, request) in [
            (
                2,
                v1::Request {
                    operation: v1::Operation::AgentHostNaming.into(),
                    agent: Some(v1::AgentRequest {
                        hook_management: v1::HookManagementAction::Install.into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ),
            (
                3,
                v1::Request {
                    operation: v1::Operation::AgentHostNaming.into(),
                    agent: Some(v1::AgentRequest {
                        hook_management: v1::HookManagementAction::Uninstall.into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ),
            (
                4,
                v1::Request {
                    operation: v1::Operation::ShutdownDaemon.into(),
                    ..Default::default()
                },
            ),
        ] {
            write_frame(
                &mut client,
                &envelope(request_id, 0, Payload::Request(request)),
            )
            .await
            .unwrap();
            let frame = timeout(std::time::Duration::from_secs(3), read_frame(&mut client))
                .await
                .expect("policy rejection timed out")
                .unwrap()
                .unwrap();
            let Some(Payload::Response(response)) = frame.payload else {
                panic!("expected response")
            };
            assert_eq!(frame.request_id, request_id);
            assert!(!response.ok);
            assert_eq!(response.error_code, "helper_incompatible");
        }
        assert!(shutdown_rx.try_recv().is_err());

        drop(client);
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn bulk_connection_rejects_control_operations_but_reports_unknown_numbers() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let task = tokio::spawn(serve_with_shutdown(server, None));
        write_frame(
            &mut client,
            &envelope(
                1,
                0,
                Payload::ClientHello(v1::ClientHello {
                    desktop_version: "bulk-policy-test".into(),
                    requested_capabilities: HOST_CAPABILITIES,
                    bulk_connection: true,
                    expected_server_identity: crate::service::snapshot::server_identity(),
                    ..Default::default()
                }),
            ),
        )
        .await
        .unwrap();
        let hello = timeout(std::time::Duration::from_secs(3), read_frame(&mut client))
            .await
            .expect("server hello timed out")
            .unwrap()
            .unwrap();
        let Some(Payload::ServerHello(hello)) = hello.payload else {
            panic!("expected hello")
        };
        assert!(!hello.read_only);

        for (request_id, operation, expected_code, expected_detail) in [
            (
                2,
                v1::Operation::AgentSnapshot.into(),
                "control_connection_required",
                "control operations",
            ),
            (3, 90, "unsupported_operation", "90"),
        ] {
            write_frame(
                &mut client,
                &envelope(
                    request_id,
                    0,
                    Payload::Request(v1::Request {
                        operation,
                        ..Default::default()
                    }),
                ),
            )
            .await
            .unwrap();
            let frame = timeout(std::time::Duration::from_secs(3), read_frame(&mut client))
                .await
                .expect("policy response timed out")
                .unwrap()
                .unwrap();
            let Some(Payload::Response(response)) = frame.payload else {
                panic!("expected response")
            };
            assert_eq!(frame.request_id, request_id);
            assert_eq!(response.error_code, expected_code);
            assert!(response.display_message.contains(expected_detail));
        }

        drop(client);
        task.abort();
        let _ = task.await;
    }
}
