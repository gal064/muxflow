//! Replays a recorded agent TUI session at its original timing.
//!
//! Install the binary as `claude` or `codex` so the host's process detection
//! sees an agent in the pane, as tests/integration/agents/agent-fixture.rs does.
//!
//!   claude <recording> [--offset SECONDS]   replay, looping at the end
//!   claude <recording> --idle               draw the first 5 s, then go silent
//!
//! <recording> is the prefix of a util-linux `script` capture: <prefix>.data
//! (--log-out) and <prefix>.timing (--log-timing, classic "delay bytes" lines
//! or advanced "O delay bytes" lines). Everything before --offset is written at
//! once, so panes started with different offsets do not move in lockstep.
//! Build: rustc --edition 2024 -O agent-replay.rs -o claude

use std::io::{Read, Write};
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let prefix = args.get(1).expect("usage: claude <recording> [--offset S | --idle]");
    let idle = args.iter().any(|arg| arg == "--idle");
    let offset = args
        .iter()
        .position(|arg| arg == "--offset")
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.0);

    let data = std::fs::read(format!("{prefix}.data")).expect("read .data");
    let timing = std::fs::read_to_string(format!("{prefix}.timing")).expect("read .timing");
    let mut chunks: Vec<(f64, usize)> = Vec::new();
    for line in timing.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (delay, size) = match fields.as_slice() {
            [delay, size] => (delay, size),
            ["O", delay, size] => (delay, size),
            _ => continue, // input ("I") and header ("H"/"S") records
        };
        if let (Ok(delay), Ok(size)) = (delay.parse::<f64>(), size.parse::<usize>()) {
            chunks.push((delay, size));
        }
    }
    let total: usize = chunks.iter().map(|chunk| chunk.1).sum();
    // `script` puts a "Script started" header line before the first timed byte.
    let start = if data.starts_with(b"Script started") {
        data.iter().position(|byte| *byte == b'\n').map_or(0, |index| index + 1)
    } else {
        0
    };
    let body = &data[start..(start + total).min(data.len())];

    // Like the real TUI: raw, no echo, and consume what the terminal answers to
    // the recorded queries, so those replies never land on screen.
    let _ = std::process::Command::new("stty").args(["raw", "-echo"]).status();
    std::thread::spawn(|| {
        let mut sink = [0_u8; 256];
        let mut input = std::io::stdin();
        while let Ok(read) = input.read(&mut sink) {
            if read == 0 {
                break;
            }
        }
    });
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if idle {
        play(&mut out, body, &chunks, 0.0, Some(5.0));
        drop(out);
        // Silent from here: hold the pane like an agent waiting on a model.
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
    let mut skip = offset;
    loop {
        play(&mut out, body, &chunks, skip, None);
        skip = 0.0;
        let _ = out.write_all(b"\x1b[2J\x1b[H");
    }
}

/// Writes `body` paced by `chunks`. Everything before `skip` seconds goes out
/// at once; with `stop`, returns after that many seconds of recording.
fn play(out: &mut impl Write, body: &[u8], chunks: &[(f64, usize)], skip: f64, stop: Option<f64>) {
    let began = Instant::now();
    let mut clock = 0.0_f64;
    let mut position = 0_usize;
    for &(delay, size) in chunks {
        clock += delay;
        if let Some(stop) = stop
            && clock > stop
        {
            let _ = out.flush();
            return;
        }
        if clock > skip {
            let due = Duration::from_secs_f64(clock - skip);
            let elapsed = began.elapsed();
            if due > elapsed {
                let _ = out.flush();
                std::thread::sleep(due - elapsed);
            }
        }
        let end = (position + size).min(body.len());
        let _ = out.write_all(&body[position..end]);
        position = end;
        if clock > skip {
            let _ = out.flush();
        }
    }
    let _ = out.flush();
}
