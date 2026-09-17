use std::{
    io::BufReader,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use tmux_agent_protocol::{
    envelope, read_frame_sync,
    v1::{self, envelope::Payload},
    write_frame_sync,
};

pub fn local_bridge_command(host_binary: &str) -> Command {
    let mut command = Command::new(host_binary);
    command.args(["bridge", "--stdio"]);
    command
}

pub fn ssh_bridge_command(
    config: &str,
    target: &str,
    remote_command: &str,
    options: &[&str],
) -> Command {
    let mut command = Command::new("ssh");
    command.args(["-F", config, "-T"]).args(options);
    command.arg(target).arg(remote_command);
    command
}

pub struct Hello<'a> {
    pub bulk_connection: bool,
    pub expected_server_identity: &'a str,
    pub connection_epoch: u64,
}

impl<'a> Hello<'a> {
    pub fn control() -> Self {
        Self {
            bulk_connection: false,
            expected_server_identity: "",
            connection_epoch: 0,
        }
    }
}

pub struct Bridge {
    child: Child,
    pub stdin: ChildStdin,
    pub reader: BufReader<ChildStdout>,
    next_request: u64,
}

impl Bridge {
    pub fn connect(
        command: &mut Command,
        hello: Hello<'_>,
        first_request: u64,
    ) -> Result<(Self, v1::ServerHello), String> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("could not spawn bridge: {error}"))?;
        let mut stdin = child.stdin.take().ok_or("bridge stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("bridge stdout unavailable")?;
        let mut reader = BufReader::new(stdout);
        let server_hello = handshake(&mut stdin, &mut reader, &hello)?;
        Ok((
            Self {
                child,
                stdin,
                reader,
                next_request: first_request,
            },
            server_hello,
        ))
    }

    pub fn next_request_id(&mut self) -> Result<u64, String> {
        let request_id = self.next_request;
        self.next_request = self
            .next_request
            .checked_add(1)
            .ok_or("request ID overflow")?;
        Ok(request_id)
    }

    pub fn upcoming_request_id(&self) -> u64 {
        self.next_request
    }

    pub fn request_with_events(
        &mut self,
        request: v1::Request,
    ) -> Result<(v1::Response, Vec<v1::Envelope>), String> {
        let request_id = self.next_request_id()?;
        request_with_events(&mut self.stdin, &mut self.reader, request_id, request)
    }

    pub fn response(&mut self, request: v1::Request) -> Result<v1::Response, String> {
        let request_id = self.next_request_id()?;
        write_frame_sync(
            &mut self.stdin,
            &envelope(request_id, 0, Payload::Request(request)),
        )
        .map_err(|error| error.to_string())?;
        loop {
            let frame = read_frame_sync(&mut self.reader)
                .map_err(|error| error.to_string())?
                .ok_or("bridge disconnected")?;
            if frame.request_id == request_id
                && let Some(Payload::Response(response)) = frame.payload
            {
                return Ok(response);
            }
        }
    }

    pub fn request(&mut self, request: v1::Request) -> Result<v1::Response, String> {
        let response = self.response(request)?;
        if response.ok {
            Ok(response)
        } else {
            Err(format!(
                "{}: {}",
                response.error_code, response.display_message
            ))
        }
    }

    pub fn request_error(&mut self, request: v1::Request) -> Result<v1::Response, String> {
        let response = self.response(request)?;
        if response.ok {
            Err("request unexpectedly succeeded".into())
        } else {
            Ok(response)
        }
    }

    pub fn send(&mut self, request: v1::Request) -> Result<u64, String> {
        let request_id = self.next_request_id()?;
        write_frame_sync(
            &mut self.stdin,
            &envelope(request_id, 0, Payload::Request(request)),
        )
        .map_err(|error| error.to_string())?;
        Ok(request_id)
    }

    pub fn subscribe(&mut self) -> Result<v1::Snapshot, String> {
        self.request(v1::Request {
            operation: v1::Operation::Subscribe.into(),
            scope: "full".into(),
            ..Default::default()
        })?
        .snapshot
        .ok_or("subscribe omitted snapshot".into())
    }

    pub fn active_root(&mut self, operation_id: String) -> Result<v1::ActiveRoot, String> {
        let snapshot = self.subscribe()?;
        let pane = snapshot
            .panes
            .iter()
            .find(|pane| pane.active)
            .or_else(|| snapshot.panes.first())
            .ok_or("fixture has no tmux pane")?;
        self.resolve_active_root(&snapshot, &pane.id.clone(), operation_id)
    }

    pub fn resolve_active_root(
        &mut self,
        snapshot: &v1::Snapshot,
        pane_id: &str,
        operation_id: String,
    ) -> Result<v1::ActiveRoot, String> {
        self.request(v1::Request {
            operation: v1::Operation::ResolveActiveRoot.into(),
            file: Some(v1::FileServiceRequest {
                operation_id,
                pane_id: pane_id.to_owned(),
                expected_server_identity: snapshot.server_identity.clone(),
                expected_topology_generation: snapshot.generation,
                ..Default::default()
            }),
            ..Default::default()
        })?
        .file
        .and_then(|file| file.active_root)
        .ok_or("active-root response omitted payload".into())
    }

    pub fn close(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn request(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    request_id: u64,
    request: v1::Request,
) -> Result<v1::Response, String> {
    let (response, _) = request_with_events(stdin, reader, request_id, request)?;
    if response.ok {
        Ok(response)
    } else {
        Err(format!(
            "{}: {}",
            response.error_code, response.display_message
        ))
    }
}

pub fn request_error(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    request_id: u64,
    request: v1::Request,
) -> Result<v1::Response, String> {
    let (response, _) = request_with_events(stdin, reader, request_id, request)?;
    if response.ok {
        Err("request unexpectedly succeeded".into())
    } else {
        Ok(response)
    }
}

pub fn request_with_events(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    request_id: u64,
    request: v1::Request,
) -> Result<(v1::Response, Vec<v1::Envelope>), String> {
    write_frame_sync(stdin, &envelope(request_id, 0, Payload::Request(request)))
        .map_err(|error| error.to_string())?;
    let mut events = Vec::new();
    loop {
        let frame = read_frame_sync(reader)
            .map_err(|error| error.to_string())?
            .ok_or("bridge disconnected")?;
        if frame.request_id == request_id
            && let Some(Payload::Response(response)) = frame.payload.clone()
        {
            return Ok((response, events));
        }
        events.push(frame);
    }
}

fn handshake(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    config: &Hello<'_>,
) -> Result<v1::ServerHello, String> {
    write_frame_sync(
        stdin,
        &envelope(
            1,
            0,
            Payload::ClientHello(v1::ClientHello {
                bulk_connection: config.bulk_connection,
                expected_server_identity: config.expected_server_identity.into(),
                connection_epoch: config.connection_epoch,
            }),
        ),
    )
    .map_err(|error| error.to_string())?;
    let frame = read_frame_sync(reader)
        .map_err(|error| error.to_string())?
        .ok_or("bridge closed during handshake")?;
    let Some(Payload::ServerHello(hello)) = frame.payload else {
        return Err("missing ServerHello".into());
    };
    tmux_agent_protocol::validate_host_contract(frame.protocol_major)
        .map_err(|error| error.to_string())?;
    if config.connection_epoch != 0 && hello.connection_epoch != config.connection_epoch {
        return Err("bridge did not echo connection epoch".into());
    }
    if !config.expected_server_identity.is_empty()
        && hello.server_identity != config.expected_server_identity
    {
        return Err("bridge did not echo server identity".into());
    }
    Ok(hello)
}
