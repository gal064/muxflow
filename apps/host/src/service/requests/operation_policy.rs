use tmux_agent_protocol::v1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationValue {
    Known(v1::Operation),
    Unknown(i32),
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
    BulkConnectionRequired,
    ControlConnectionRequired,
}

impl AdmissionError {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::BulkConnectionRequired => "bulk_connection_required",
            Self::ControlConnectionRequired => "control_connection_required",
        }
    }

    pub(crate) fn message(self) -> &'static str {
        match self {
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
    pub(crate) lane: Lane,
    pub(crate) scheduling: Scheduling,
    pub(crate) handler: Handler,
}

impl OperationPolicy {
    pub(crate) fn for_raw(raw: i32) -> Self {
        let Ok(operation) = v1::Operation::try_from(raw) else {
            return Self {
                operation: OperationValue::Unknown(raw),
                lane: Lane::Either,
                scheduling: Scheduling::Inline,
                handler: Handler::Unsupported,
            };
        };

        let (lane, scheduling, handler) = match operation {
            v1::Operation::Unspecified => (Lane::Control, Scheduling::Inline, Handler::Unsupported),
            v1::Operation::FullSnapshot | v1::Operation::Subscribe | v1::Operation::Resync => {
                (Lane::Control, Scheduling::Inline, Handler::Snapshot)
            }
            v1::Operation::ShutdownDaemon => (Lane::Control, Scheduling::Inline, Handler::Daemon),
            v1::Operation::AttachTerminal
            | v1::Operation::TerminalInput
            | v1::Operation::ResizeTerminal
            | v1::Operation::YieldTerminalSizing
            | v1::Operation::SetTerminalVisibility
            | v1::Operation::RequestTerminalSeed
            | v1::Operation::SelectTerminalSession => {
                (Lane::Control, Scheduling::Inline, Handler::Terminal)
            }
            v1::Operation::TmuxAction => (Lane::Control, Scheduling::Inline, Handler::TmuxAction),
            // Detached, unlike every other terminal operation: a scrollback
            // capture is thousands of lines the user is waiting on, and running
            // it inline would hold the reader loop — and every keystroke behind
            // it — for the length of that capture. Read-only, because it
            // photographs and changes nothing.
            v1::Operation::RequestTerminalHistory => {
                (Lane::Control, Scheduling::Detached, Handler::Terminal)
            }
            v1::Operation::ResolveActiveRoot | v1::Operation::ResolveTerminalFile => {
                (Lane::Control, Scheduling::Detached, Handler::ActiveRoot)
            }
            v1::Operation::ListDirectory => {
                (Lane::Control, Scheduling::Detached, Handler::Filesystem)
            }
            // Detached deliberately: the reader loop must stay free to admit
            // this request's own Cancel while its bounded body is streaming.
            v1::Operation::OpenFileStream => {
                (Lane::Bulk, Scheduling::Detached, Handler::Filesystem)
            }
            v1::Operation::WatchDirectory
            | v1::Operation::UnwatchDirectory
            | v1::Operation::FileMutation => {
                (Lane::Control, Scheduling::Detached, Handler::Filesystem)
            }
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
            | v1::Operation::CancelTerminalUpload => {
                (Lane::Bulk, Scheduling::Inline, Handler::Filesystem)
            }
            v1::Operation::ReconcileTerminalUpload => {
                (Lane::Bulk, Scheduling::Inline, Handler::Filesystem)
            }
            v1::Operation::GitStatus | v1::Operation::GitDiff => {
                (Lane::Control, Scheduling::Detached, Handler::Git)
            }
            // Diff bodies are the only Git payload large enough to matter to
            // terminal latency, so they alone take the independent bulk lane.
            // Detached so a body read cannot block
            // the connection's frame loop, including the `Cancel` frame that
            // would stop it.
            v1::Operation::GitDiffContent => (Lane::Bulk, Scheduling::Detached, Handler::Git),
            v1::Operation::WatchGit
            | v1::Operation::UnwatchGit
            | v1::Operation::PrepareGitDiscard
            | v1::Operation::GitMutation
            | v1::Operation::GitCommit
            | v1::Operation::GitPush => (Lane::Control, Scheduling::Detached, Handler::Git),
            v1::Operation::AgentSnapshot | v1::Operation::AgentDiagnostics => {
                (Lane::Control, Scheduling::Inline, Handler::Agent)
            }
            v1::Operation::AgentAction
            | v1::Operation::AgentMarkSeen
            | v1::Operation::AgentHookIngest
            | v1::Operation::AgentHookManagement
            | v1::Operation::AgentHostNaming => (Lane::Control, Scheduling::Inline, Handler::Agent),
            // A readiness probe and a session registration touch in-memory
            // state only, so they answer inline like the agent operations.
            v1::Operation::VoiceStatus | v1::Operation::VoiceSession => {
                (Lane::Control, Scheduling::Inline, Handler::Voice)
            }
            // Detached: each spawns a process and writes the cache directory,
            // and none may hold the reader loop — a transcription is seconds
            // long and its own Cancel arrives on this connection.
            v1::Operation::VoiceProvision
            | v1::Operation::VoiceTranscribe
            | v1::Operation::VoiceSpeak => (Lane::Control, Scheduling::Detached, Handler::Voice),
            v1::Operation::TestDelay
            | v1::Operation::TestInjectGap
            | v1::Operation::TestOverflow => (Lane::Control, Scheduling::Inline, Handler::Test),
        };

        Self {
            operation: OperationValue::Known(operation),
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

    pub(crate) fn admission_error(self, bulk_connection: bool) -> Option<AdmissionError> {
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
    use tmux_agent_protocol::{envelope, read_frame, v1::envelope::Payload, write_frame};
    use tokio::{net::UnixStream, time::timeout};

    fn assert_policy(
        operation: v1::Operation,
        lane: Lane,
        scheduling: Scheduling,
        handler: Handler,
    ) {
        assert_eq!(
            OperationPolicy::for_raw(operation.into()),
            OperationPolicy {
                operation: OperationValue::Known(operation),
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
    /// can read the same list the policy test asserts. Two hand-written
    /// lists of the same operations would be the very drift being guarded.
    fn expected_policies() -> Vec<(v1::Operation, Lane, Scheduling, Handler)> {
        use Handler::{
            ActiveRoot as AR, Agent as AH, Daemon as DH, Filesystem as FH, Git as GH,
            Snapshot as SH, Terminal as TH, Test as XH, TmuxAction as MH, Unsupported as UH,
            Voice as VH,
        };
        use Lane::{Bulk as B, Control as C};
        use Scheduling::{Detached as Dd, Inline as I};
        use v1::Operation::*;

        vec![
            (Unspecified, C, I, UH),
            (FullSnapshot, C, I, SH),
            (Subscribe, C, I, SH),
            (AttachTerminal, C, I, TH),
            (TerminalInput, C, I, TH),
            (ResizeTerminal, C, I, TH),
            (YieldTerminalSizing, C, I, TH),
            (Resync, C, I, SH),
            // Shutdown is intercepted before request registration, but remains a mutation.
            (ShutdownDaemon, C, I, DH),
            (TmuxAction, C, I, MH),
            (SetTerminalVisibility, C, I, TH),
            (RequestTerminalSeed, C, I, TH),
            (RequestTerminalHistory, C, Dd, TH),
            (ResolveActiveRoot, C, Dd, AR),
            (ResolveTerminalFile, C, Dd, AR),
            (ListDirectory, C, Dd, FH),
            (WatchDirectory, C, Dd, FH),
            (UnwatchDirectory, C, Dd, FH),
            (FileMutation, C, Dd, FH),
            (OpenFileStream, B, Dd, FH),
            (StartDownload, B, I, FH),
            (ReadDownloadChunk, B, I, FH),
            (CancelDownload, B, I, FH),
            (BeginFileWrite, B, I, FH),
            (WriteFileChunk, B, I, FH),
            (CommitFileWrite, B, I, FH),
            (CancelFileWrite, B, I, FH),
            (GitStatus, C, Dd, GH),
            (WatchGit, C, Dd, GH),
            (UnwatchGit, C, Dd, GH),
            (GitDiff, C, Dd, GH),
            (GitDiffContent, B, Dd, GH),
            (PrepareGitDiscard, C, Dd, GH),
            (GitMutation, C, Dd, GH),
            (GitCommit, C, Dd, GH),
            (GitPush, C, Dd, GH),
            (AgentSnapshot, C, I, AH),
            (AgentDiagnostics, C, I, AH),
            (AgentAction, C, I, AH),
            (AgentMarkSeen, C, I, AH),
            (AgentHookIngest, C, I, AH),
            (AgentHookManagement, C, I, AH),
            (PrepareTerminalUpload, B, I, FH),
            (WriteTerminalUploadChunk, B, I, FH),
            (CommitTerminalUpload, B, I, FH),
            (CancelTerminalUpload, B, I, FH),
            (ReconcileTerminalUpload, B, I, FH),
            (AgentHostNaming, C, I, AH),
            (SelectTerminalSession, C, I, TH),
            (VoiceStatus, C, I, VH),
            (VoiceProvision, C, Dd, VH),
            (VoiceTranscribe, C, Dd, VH),
            (VoiceSpeak, C, Dd, VH),
            (VoiceSession, C, I, VH),
            (TestDelay, C, I, XH),
            (TestInjectGap, C, I, XH),
            (TestOverflow, C, I, XH),
        ]
    }

    #[test]
    fn every_generated_operation_has_the_expected_policy() {
        for (operation, lane, scheduling, handler) in expected_policies() {
            assert_policy(operation, lane, scheduling, handler);
        }
    }

    /// The list above is hand-written, so this is what makes its name true.
    ///
    /// The production `match` is compiler-exhaustive, so a new operation cannot
    /// be added without *a* policy — but nothing forced it into the
    /// policy assertions, and an operation with a policy and no
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
    fn the_policy_list_covers_every_operation_the_enum_accepts() {
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
            "these operations have a policy but no policy assertion; \
             add them to `every_generated_operation_has_the_expected_policy`: {missing:?}",
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
        assert_eq!(first.admission_error(true), None);
    }

    #[test]
    fn admission_enforces_exact_connection_lane() {
        let naming = OperationPolicy::for_raw(v1::Operation::AgentHostNaming.into());
        assert_eq!(naming.admission_error(false), None);
        assert_eq!(
            naming.admission_error(true),
            Some(AdmissionError::ControlConnectionRequired)
        );
        let download = OperationPolicy::for_raw(v1::Operation::StartDownload.into());
        assert_eq!(download.admission_error(true), None);
        assert_eq!(
            download.admission_error(false),
            Some(AdmissionError::BulkConnectionRequired)
        );
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
        let Some(Payload::ServerHello(_)) = hello.payload else {
            panic!("expected hello")
        };

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
