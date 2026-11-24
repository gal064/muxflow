//! `muxflow-host chat send <pane> <message>`: sends a user message to a chat
//! pane through the running daemon, as the desktop's composer does, so that a
//! script or another agent can message an agent in Chat mode.
//!
//! A chat pane's agent reads only what the daemon writes to it: typing into
//! the pane is thrown away, and connecting to the agent's socket takes it
//! from the daemon. A terminal pane is refused; `tmux send-keys` reaches it.

use std::{
    io::{BufReader, Read},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, bail};
use tmux_agent_protocol::{
    envelope, read_frame_sync,
    v1::{self, envelope::Payload},
    write_frame_sync,
};

use crate::paths;

const USAGE: &str = "usage: muxflow-host chat send [--socket PATH] <pane-id> <message | ->";
/// How long the daemon may take to answer one request.
const TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) fn run(arguments: Vec<String>) -> anyhow::Result<()> {
    let (socket, pane_id, text) = parse(arguments)?;
    let text = if text == "-" {
        let mut read = String::new();
        std::io::stdin()
            .read_to_string(&mut read)
            .context("read the message from stdin")?;
        read
    } else {
        text
    };
    if text.trim().is_empty() {
        bail!("the message is empty");
    }
    send(&socket, &pane_id, text)
}

/// The socket, the pane and the message, which is `-` to read it from stdin.
fn parse(arguments: Vec<String>) -> anyhow::Result<(PathBuf, String, String)> {
    let mut rest = arguments.into_iter();
    if rest.next().as_deref() != Some("send") {
        bail!(USAGE);
    }
    let mut socket = paths::default_socket_path();
    let mut positional = Vec::new();
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "--socket" => socket = PathBuf::from(rest.next().context(USAGE)?),
            _ => positional.push(argument),
        }
    }
    let [pane_id, text] =
        <[String; 2]>::try_from(positional).map_err(|_| anyhow::anyhow!(USAGE))?;
    Ok((socket, pane_id, text))
}

fn send(socket: &std::path::Path, pane_id: &str, text: String) -> anyhow::Result<()> {
    let stream = UnixStream::connect(socket).with_context(|| {
        format!(
            "the Muxflow helper is not running at {}; open Muxflow to start it",
            socket.display()
        )
    })?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);

    write_frame_sync(
        &mut writer,
        &envelope(1, 0, Payload::ClientHello(v1::ClientHello::default())),
    )?;
    let hello = read_frame_sync(&mut reader)?.context("the helper closed the connection")?;
    let Some(Payload::ServerHello(_)) = hello.payload else {
        bail!("the helper refused the connection; is it the same version as this command?");
    };

    // A full snapshot tells the daemon which panes this connection has seen,
    // which is how it learns that the pane is a chat and which agent runs it.
    let snapshot = request(
        &mut writer,
        &mut reader,
        2,
        v1::Operation::FullSnapshot,
        None,
    )?
    .snapshot
    .context("the helper sent no snapshot")?;
    let pane = snapshot
        .panes
        .iter()
        .find(|pane| pane.id == pane_id)
        .with_context(|| format!("no pane {pane_id} on this host's tmux server"))?;
    if pane.chat_adapter_id.is_empty() {
        bail!("{pane_id} is a terminal pane, not a chat; send to it with tmux send-keys");
    }

    request(
        &mut writer,
        &mut reader,
        3,
        v1::Operation::Chat,
        Some(v1::ChatRequest {
            action: v1::ChatAction::Send.into(),
            pane_id: pane_id.to_owned(),
            expected_server_identity: snapshot.server_identity,
            text,
            ..Default::default()
        }),
    )?;
    Ok(())
}

/// Sends one request and waits for its answer, skipping anything else.
fn request(
    writer: &mut UnixStream,
    reader: &mut BufReader<UnixStream>,
    request_id: u64,
    operation: v1::Operation,
    chat: Option<v1::ChatRequest>,
) -> anyhow::Result<v1::Response> {
    let request = v1::Request {
        operation: operation.into(),
        chat,
        ..Default::default()
    };
    write_frame_sync(writer, &envelope(request_id, 0, Payload::Request(request)))?;
    loop {
        let frame = read_frame_sync(reader)?.context("the helper closed the connection")?;
        if frame.request_id != request_id {
            continue;
        }
        let Some(Payload::Response(response)) = frame.payload else {
            continue;
        };
        if !response.ok {
            bail!("{} ({})", response.display_message, response.error_code);
        }
        return Ok(response);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|argument| (*argument).to_owned()).collect()
    }

    #[test]
    fn parses_a_pane_and_a_message() {
        let (socket, pane, text) = parse(args(&["send", "%3", "hello there"])).unwrap();
        assert_eq!(socket, paths::default_socket_path());
        assert_eq!((pane.as_str(), text.as_str()), ("%3", "hello there"));
    }

    #[test]
    fn takes_a_socket_anywhere() {
        let (socket, pane, text) =
            parse(args(&["send", "%3", "--socket", "/tmp/x.sock", "-"])).unwrap();
        assert_eq!(socket, PathBuf::from("/tmp/x.sock"));
        assert_eq!((pane.as_str(), text.as_str()), ("%3", "-"));
    }

    #[test]
    fn refuses_anything_else() {
        for list in [
            &[][..],
            &["post", "%3", "hi"],
            &["send", "%3"],
            &["send", "%3", "hi", "extra"],
            &["send", "%3", "hi", "--socket"],
        ] {
            assert!(parse(args(list)).is_err(), "{list:?}");
        }
    }
}
