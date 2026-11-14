//! The little of WebSocket (RFC 6455) a chat needs to talk to an agent that
//! serves it on a local unix socket: the client's opening handshake, text
//! messages both ways, and answers to pings. No extensions, no TLS.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const TEXT: u8 = 0x1;
const CLOSE: u8 = 0x8;
const PING: u8 = 0x9;
const PONG: u8 = 0xA;
/// The most the server's answer to the handshake may be.
const MAX_HANDSHAKE_BYTES: usize = 8 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Frame {
    Text(String),
    /// To be answered with [`pong`] of the same payload.
    Ping(Vec<u8>),
    Close,
}

/// Opens the WebSocket on a stream that has just connected.
pub(super) async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
) -> std::io::Result<()> {
    // The key only proves the answer is to this request; the peer is a local
    // socket only this user can reach, so its answer is not checked and the
    // key need not vary.
    stream
        .write_all(
            b"GET / HTTP/1.1\r\n\
              Host: localhost\r\n\
              Upgrade: websocket\r\n\
              Connection: Upgrade\r\n\
              Sec-WebSocket-Key: bXV4Zmxvdy1jaGF0LWtleQ==\r\n\
              Sec-WebSocket-Version: 13\r\n\r\n",
        )
        .await?;
    // One byte at a time, so nothing of the first frame is read with it.
    let mut answer = Vec::new();
    while !answer.ends_with(b"\r\n\r\n") {
        if answer.len() >= MAX_HANDSHAKE_BYTES {
            return Err(std::io::Error::other("the handshake answer is too long"));
        }
        answer.push(stream.read_u8().await?);
    }
    if answer.starts_with(b"HTTP/1.1 101") {
        Ok(())
    } else {
        Err(std::io::Error::other("the agent refused the WebSocket"))
    }
}

/// One frame from the client. A client's frames are masked; a key of zeros
/// leaves the payload as it is, which is all a local socket needs.
fn frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(payload.len() + 14);
    bytes.push(0x80 | opcode);
    match payload.len() {
        length @ 0..=125 => bytes.push(0x80 | length as u8),
        length @ 126..=0xFFFF => {
            bytes.push(0x80 | 126);
            bytes.extend((length as u16).to_be_bytes());
        }
        length => {
            bytes.push(0x80 | 127);
            bytes.extend((length as u64).to_be_bytes());
        }
    }
    bytes.extend([0; 4]);
    bytes.extend(payload);
    bytes
}

pub(super) fn text(message: &str) -> Vec<u8> {
    frame(TEXT, message.as_bytes())
}

pub(super) fn pong(payload: &[u8]) -> Vec<u8> {
    frame(PONG, payload)
}

/// One frame as it lies in the buffer.
struct RawFrame {
    /// Its first byte: the last-frame bit and the opcode.
    first: u8,
    /// Unmasked.
    payload: Vec<u8>,
    /// How many bytes of the buffer it took.
    consumed: usize,
}

/// Turns the bytes the server sends into frames.
pub(super) struct Decoder {
    buffer: Vec<u8>,
    /// The message so far, while its frames are still arriving.
    message: Vec<u8>,
    max_message_bytes: usize,
}

impl Decoder {
    pub(super) fn new(max_message_bytes: usize) -> Self {
        Self {
            buffer: Vec::new(),
            message: Vec::new(),
            max_message_bytes,
        }
    }

    pub(super) fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend(bytes);
    }

    /// The next whole frame, or `None` until more bytes arrive. An error is a
    /// stream this client cannot follow, and ends the connection.
    pub(super) fn next(&mut self) -> Result<Option<Frame>, &'static str> {
        loop {
            let Some(RawFrame {
                first,
                payload,
                consumed,
            }) = self.take_frame()?
            else {
                return Ok(None);
            };
            self.buffer.drain(..consumed);
            let (last, opcode) = (first & 0x80 != 0, first & 0x0F);
            match opcode {
                // A message, or more of one: text, binary and continuation.
                0x0..=0x2 => {
                    if self.message.len() + payload.len() > self.max_message_bytes {
                        return Err("a message is too large");
                    }
                    self.message.extend(payload);
                    if last {
                        let message = std::mem::take(&mut self.message);
                        return Ok(Some(Frame::Text(
                            String::from_utf8_lossy(&message).into_owned(),
                        )));
                    }
                }
                CLOSE => return Ok(Some(Frame::Close)),
                PING => return Ok(Some(Frame::Ping(payload))),
                PONG => {}
                _ => return Err("an unknown frame"),
            }
        }
    }

    /// The first frame in the buffer, once all of it is there.
    fn take_frame(&self) -> Result<Option<RawFrame>, &'static str> {
        let [first, second, rest @ ..] = self.buffer.as_slice() else {
            return Ok(None);
        };
        if first & 0x70 != 0 {
            return Err("an extension nobody asked for");
        }
        let (length, rest) = match second & 0x7F {
            126 => match rest {
                [a, b, rest @ ..] => (u64::from(u16::from_be_bytes([*a, *b])), rest),
                _ => return Ok(None),
            },
            127 => match rest.split_first_chunk::<8>() {
                Some((length, rest)) => (u64::from_be_bytes(*length), rest),
                None => return Ok(None),
            },
            length => (u64::from(length), rest),
        };
        if length > self.max_message_bytes as u64 {
            return Err("a frame is too large");
        }
        let length = length as usize;
        let (mask, rest) = if second & 0x80 != 0 {
            match rest.split_first_chunk::<4>() {
                Some((mask, rest)) => (Some(*mask), rest),
                None => return Ok(None),
            }
        } else {
            (None, rest)
        };
        let Some(payload) = rest.get(..length) else {
            return Ok(None);
        };
        let mut payload = payload.to_vec();
        if let Some(mask) = mask {
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[index % 4];
            }
        }
        let consumed = self.buffer.len() - rest.len() + length;
        Ok(Some(RawFrame {
            first: *first,
            payload,
            consumed,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_frame(first: u8, payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![first];
        match payload.len() {
            length @ 0..=125 => bytes.push(length as u8),
            length @ 126..=0xFFFF => {
                bytes.push(126);
                bytes.extend((length as u16).to_be_bytes());
            }
            length => {
                bytes.push(127);
                bytes.extend((length as u64).to_be_bytes());
            }
        }
        bytes.extend(payload);
        bytes
    }

    #[test]
    fn frames_are_read_whole_however_the_bytes_arrive() {
        let long = "x".repeat(70_000);
        let mut stream = Vec::new();
        stream.extend(server_frame(0x81, b"{\"id\":1}"));
        stream.extend(server_frame(0x89, b"hi"));
        // One message in three frames, with a pong between them.
        stream.extend(server_frame(0x01, b"ab"));
        stream.extend(server_frame(0x8A, b""));
        stream.extend(server_frame(0x00, b"cd"));
        stream.extend(server_frame(0x80, b"ef"));
        stream.extend(server_frame(0x81, &"y".repeat(300).into_bytes()));
        stream.extend(server_frame(0x81, long.as_bytes()));
        stream.extend(server_frame(0x88, b""));

        let mut decoder = Decoder::new(1024 * 1024);
        let mut frames = Vec::new();
        for chunk in stream.chunks(7) {
            decoder.push(chunk);
            while let Some(frame) = decoder.next().unwrap() {
                frames.push(frame);
            }
        }
        assert_eq!(
            frames,
            [
                Frame::Text("{\"id\":1}".into()),
                Frame::Ping(b"hi".to_vec()),
                Frame::Text("abcdef".into()),
                Frame::Text("y".repeat(300)),
                Frame::Text(long),
                Frame::Close,
            ]
        );
    }

    #[test]
    fn a_client_frame_reads_back_as_what_was_sent() {
        for length in [0, 5, 125, 126, 70_000] {
            let message = "m".repeat(length);
            let mut decoder = Decoder::new(1024 * 1024);
            decoder.push(&text(&message));
            assert_eq!(decoder.next().unwrap(), Some(Frame::Text(message)));
            assert_eq!(decoder.next().unwrap(), None);
        }
    }

    #[test]
    fn a_message_past_the_bound_ends_the_stream() {
        let mut decoder = Decoder::new(16);
        decoder.push(&server_frame(0x81, &[b'a'; 17]));
        assert!(decoder.next().is_err());
        let mut decoder = Decoder::new(16);
        decoder.push(&server_frame(0x01, &[b'a'; 10]));
        decoder.push(&server_frame(0x80, &[b'a'; 10]));
        assert!(decoder.next().is_err());
    }

    #[tokio::test]
    async fn the_handshake_stops_at_the_end_of_the_answer() {
        let (mut client, mut server) = tokio::io::duplex(4096);
        let serving = tokio::spawn(async move {
            let mut request = vec![0; 4096];
            let count = server.read(&mut request).await.unwrap();
            assert!(request[..count].starts_with(b"GET / HTTP/1.1\r\n"));
            let mut answer =
                b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n".to_vec();
            answer.extend(server_frame(0x81, b"first"));
            server.write_all(&answer).await.unwrap();
            server
        });
        handshake(&mut client).await.unwrap();
        let _server = serving.await.unwrap();
        // The first frame is still there to read.
        let mut rest = [0; 7];
        client.read_exact(&mut rest).await.unwrap();
        assert_eq!(rest, *server_frame(0x81, b"first"));
    }
}
