//! Independent end point for key-to-frame on X11: read a screen rectangle from
//! the X server with `GetImage` until it changes. That is what the server
//! actually holds, unlike `Window::present` returning (the WSI may present
//! asynchronously on a queue thread). Raw protocol over the Unix socket so the
//! spike needs no new dependency. Calibration only; X11 without auth (Xvfb).

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

pub struct PixelReader {
    stream: UnixStream,
    root: u32,
}

impl PixelReader {
    pub fn connect() -> Result<Self, String> {
        let display = std::env::var("DISPLAY").map_err(|_| "DISPLAY not set")?;
        let number = display
            .strip_prefix(':')
            .and_then(|d| d.split('.').next())
            .ok_or_else(|| format!("unsupported DISPLAY {display}"))?;
        let path = format!("/tmp/.X11-unix/X{number}");
        let mut stream = UnixStream::connect(&path).map_err(|e| format!("{path}: {e}"))?;
        // Little-endian, protocol 11.0, no authorization.
        let mut setup = vec![b'l', 0];
        setup.extend_from_slice(&11u16.to_le_bytes());
        setup.extend_from_slice(&0u16.to_le_bytes());
        setup.extend_from_slice(&[0u8; 6]);
        stream.write_all(&setup).map_err(|e| e.to_string())?;
        let mut header = [0u8; 8];
        stream.read_exact(&mut header).map_err(|e| e.to_string())?;
        if header[0] != 1 {
            return Err("X server refused the connection (auth required?)".into());
        }
        let extra = u16::from_le_bytes([header[6], header[7]]) as usize * 4;
        let mut body = vec![0u8; extra];
        stream.read_exact(&mut body).map_err(|e| e.to_string())?;
        let vendor_len = u16::from_le_bytes([body[16], body[17]]) as usize;
        let formats = body[21] as usize;
        let screen_start = 32 + vendor_len.next_multiple_of(4) + 8 * formats;
        let root = u32::from_le_bytes(body[screen_start..screen_start + 4].try_into().unwrap());
        Ok(Self { stream, root })
    }

    /// One `GetImage` of a root-window rectangle, ZPixmap, all planes.
    pub fn grab(&mut self, x: i16, y: i16, w: u16, h: u16) -> Result<Vec<u8>, String> {
        let mut request = vec![73u8, 2];
        request.extend_from_slice(&5u16.to_le_bytes());
        request.extend_from_slice(&self.root.to_le_bytes());
        request.extend_from_slice(&x.to_le_bytes());
        request.extend_from_slice(&y.to_le_bytes());
        request.extend_from_slice(&w.to_le_bytes());
        request.extend_from_slice(&h.to_le_bytes());
        request.extend_from_slice(&u32::MAX.to_le_bytes());
        self.stream.write_all(&request).map_err(|e| e.to_string())?;
        let mut reply = [0u8; 32];
        self.stream.read_exact(&mut reply).map_err(|e| e.to_string())?;
        if reply[0] != 1 {
            return Err(format!("GetImage error code {}", reply[1]));
        }
        let len = u32::from_le_bytes(reply[4..8].try_into().unwrap()) as usize * 4;
        let mut data = vec![0u8; len];
        self.stream.read_exact(&mut data).map_err(|e| e.to_string())?;
        Ok(data)
    }

    /// Poll until the rectangle differs from `baseline`. Returns the reader so
    /// the next sample can reuse the connection, and when the change was seen.
    pub fn wait_for_change(
        mut self,
        baseline: Vec<u8>,
        rect: (i16, i16, u16, u16),
        timeout: Duration,
    ) -> (Self, Option<Instant>) {
        let started = Instant::now();
        while started.elapsed() < timeout {
            match self.grab(rect.0, rect.1, rect.2, rect.3) {
                Ok(data) if data != baseline => return (self, Some(Instant::now())),
                Ok(_) => std::thread::sleep(Duration::from_micros(300)),
                Err(_) => break,
            }
        }
        (self, None)
    }
}
