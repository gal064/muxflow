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
            v1::Operation::ResolveActiveRoot => (
                Access::ReadOnly,
                Lane::Control,
                Scheduling::Detached,
                Handler::ActiveRoot,
            ),
            v1::Operation::ListDirectory | v1::Operation::ReadFile => (
                Access::ReadOnly,
                Lane::Control,
                Scheduling::Detached,
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
            // terminal latency, so they normally take the independent bulk
            // lane. `Either` because a read-only host refuses a bulk connection
            // outright, and a large diff must still be viewable there.
            // Detached so a body read cannot block the connection's frame loop,
            // including the `Cancel` frame that would stop it.
            v1::Operation::GitDiffContent => (
                Access::ReadOnly,
                Lane::Either,
                Scheduling::Detached,
                Handler::Git,
            ),
            v1::Operation::WatchGit
            | v1::Operation::UnwatchGit
            | v1::Operation::PrepareGitDiscard
            | v1::Operation::GitMutation
            | v1::Operation::GitCommit => (
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

    #[test]
    fn every_generated_operation_has_the_compatible_policy() {
        use Access::{Mutation as M, ReadOnly as R};
        use Handler::{
            ActiveRoot as AR, Agent as AH, Daemon as DH, Filesystem as FH, Git as GH,
            Snapshot as SH, Terminal as TH, Test as XH, TmuxAction as MH, Unsupported as UH,
        };
        use Lane::{Bulk as B, Control as C, Either as E};
        use Scheduling::{Detached as Dd, Inline as I};
        use v1::Operation::*;

        assert_policy(Unspecified, R, C, I, UH);
        assert_policy(FullSnapshot, R, C, I, SH);
        assert_policy(Subscribe, R, C, I, SH);
        assert_policy(AttachTerminal, M, C, I, TH);
        assert_policy(TerminalInput, M, C, I, TH);
        assert_policy(ResizeTerminal, M, C, I, TH);
        assert_policy(Resync, R, C, I, SH);
        // Shutdown is intercepted before request registration, but remains a mutation.
        assert_policy(ShutdownDaemon, M, C, I, DH);
        assert_policy(TmuxAction, M, C, I, MH);
        assert_policy(SetTerminalVisibility, M, C, I, TH);
        assert_policy(RequestTerminalSeed, M, C, I, TH);
        assert_policy(ResolveActiveRoot, R, C, Dd, AR);
        assert_policy(ListDirectory, R, C, Dd, FH);
        assert_policy(WatchDirectory, M, C, Dd, FH);
        assert_policy(UnwatchDirectory, M, C, Dd, FH);
        assert_policy(FileMutation, M, C, Dd, FH);
        assert_policy(ReadFile, R, C, Dd, FH);
        assert_policy(WriteFile, M, C, Dd, FH);
        assert_policy(StartDownload, M, B, I, FH);
        assert_policy(ReadDownloadChunk, M, B, I, FH);
        assert_policy(CancelDownload, M, B, I, FH);
        assert_policy(BeginFileWrite, M, B, I, FH);
        assert_policy(WriteFileChunk, M, B, I, FH);
        assert_policy(CommitFileWrite, M, B, I, FH);
        assert_policy(CancelFileWrite, M, B, I, FH);
        assert_policy(GitStatus, R, C, Dd, GH);
        assert_policy(WatchGit, M, C, Dd, GH);
        assert_policy(UnwatchGit, M, C, Dd, GH);
        assert_policy(GitDiff, R, C, Dd, GH);
        assert_policy(GitDiffContent, R, E, Dd, GH);
        assert_policy(PrepareGitDiscard, M, C, Dd, GH);
        assert_policy(GitMutation, M, C, Dd, GH);
        assert_policy(GitCommit, M, C, Dd, GH);
        assert_policy(AgentSnapshot, R, C, I, AH);
        assert_policy(AgentAction, M, C, I, AH);
        assert_policy(AgentMarkSeen, M, C, I, AH);
        assert_policy(AgentHookIngest, M, C, I, AH);
        assert_policy(AgentHookManagement, M, C, I, AH);
        assert_policy(PrepareTerminalUpload, M, B, I, FH);
        assert_policy(WriteTerminalUploadChunk, M, B, I, FH);
        assert_policy(CommitTerminalUpload, M, B, I, FH);
        assert_policy(CancelTerminalUpload, M, B, I, FH);
        assert_policy(ReconcileTerminalUpload, R, B, I, FH);
        assert_policy(AgentHostNaming, M, C, I, AH);
        assert_policy(SelectTerminalSession, M, C, I, TH);
        assert_policy(TestDelay, R, C, I, XH);
        assert_policy(TestInjectGap, R, C, I, XH);
        assert_policy(TestOverflow, R, C, I, XH);
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
