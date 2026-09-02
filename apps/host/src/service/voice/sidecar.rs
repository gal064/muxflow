//! The Python sidecar the host spawns with `uv`
//! (docs/mobile/voice-mode-plan.md §4.2).
//!
//! One JSON header line, optionally followed by exactly `body_bytes` raw
//! bytes, in both directions. A reader task owns stdout and turns the stream
//! into frames, so a request can wait on a channel — which is cancel-safe —
//! while a `cancel` header goes out on stdin.

use std::{
    fs::{self, OpenOptions},
    io,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::mpsc,
    time::{sleep, timeout},
};

pub(crate) const PYTHON_PIN: &str = "3.12";
pub(crate) const SHERPA_ONNX_PIN: &str = "1.13.7";
pub(crate) const EDGE_TTS_PIN: &str = "7.2.8";
/// The largest body the sidecar may answer with: the reply MP3 cap (§2b).
pub(crate) const MAX_REPLY_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_HEADER_BYTES: u64 = 64 * 1024;
/// The stderr log starts over past this on the next spawn.
const MAX_LOG_BYTES: u64 = 1024 * 1024;
/// How often a waiting request looks at its cancellation flag.
const CANCEL_POLL: Duration = Duration::from_millis(100);
/// How long a cancelled request waits for the sidecar to acknowledge.
const CANCEL_GRACE: Duration = Duration::from_secs(5);

const SCRIPT: &str = include_str!("sidecar.py");

/// Writes the embedded script beside the cache as `sidecar-<hash>.py`, 0600,
/// and removes the scripts of other versions. The hash in the name stands in
/// for a compare-on-start: a newer host never runs an older script.
pub(crate) fn script_path(cache_dir: &Path) -> io::Result<PathBuf> {
    let hash = blake3::hash(SCRIPT.as_bytes()).to_hex();
    let name = format!("sidecar-{}.py", &hash[..16]);
    let path = cache_dir.join(&name);
    if let Ok(entries) = fs::read_dir(cache_dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            if file_name.starts_with("sidecar-") && file_name.ends_with(".py") && file_name != name
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    if fs::read_to_string(&path).is_ok_and(|current| current == SCRIPT) {
        return Ok(path);
    }
    let temporary = cache_dir.join(format!(".{name}.{}", std::process::id()));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    io::Write::write_all(&mut file, SCRIPT.as_bytes())?;
    file.sync_all()?;
    fs::rename(&temporary, &path)?;
    Ok(path)
}

pub(crate) fn log_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join("sidecar.log")
}

/// A frame from the sidecar: the header and, when it announced one, the body.
#[derive(Debug)]
pub(crate) struct Reply {
    pub(crate) header: serde_json::Value,
    pub(crate) body: Vec<u8>,
}

#[derive(Debug)]
pub(crate) enum SidecarFailure {
    /// The sidecar answered `ok: false`; it is still healthy.
    Refused { class: String, error: String },
    /// The sidecar was told to cancel and did.
    Cancelled,
    /// The sidecar was told to cancel and did not answer within the grace:
    /// it is stuck in work it cannot interrupt and has to be killed, but that
    /// is the cancel's doing, not a fault of its own.
    CancelUnacknowledged,
    /// No reply within the bound; the child is no longer trustworthy.
    Timeout,
    /// stdout closed, or the child answered something that is not a frame.
    Crashed(String),
}

pub(crate) struct SidecarChild {
    child: Child,
    stdin: ChildStdin,
    frames: mpsc::Receiver<Result<Reply, String>>,
    reader: tokio::task::JoinHandle<()>,
    pub(crate) next_id: u64,
}

impl SidecarChild {
    /// The production spawn: `uv run` with pinned Python and wheels, the
    /// cache directory as cwd, unbuffered Python output.
    pub(crate) fn uv_command(uv: &Path, script: &Path, cache_dir: &Path) -> Command {
        let mut command = Command::new(uv);
        command
            .arg("run")
            .args(["--python", PYTHON_PIN, "--no-project", "--no-config"])
            .arg("--with")
            .arg(format!("sherpa-onnx=={SHERPA_ONNX_PIN}"))
            .arg("--with")
            .arg(format!("edge-tts=={EDGE_TTS_PIN}"))
            .arg(script)
            .current_dir(cache_dir)
            .env("PYTHONUNBUFFERED", "1");
        command
    }

    /// Spawns `command` as the sidecar, stderr to `log` (started over past
    /// 1 MiB), killed when dropped and — on Linux — when the daemon dies.
    pub(crate) fn spawn_with(mut command: Command, log: &Path) -> io::Result<Self> {
        let log_file = open_log(log)?;
        // `uv run` keeps the interpreter as its own child rather than exec'ing
        // it, so the sidecar is a process group: `kill()` signals the group and
        // reaches Python directly instead of hoping uv forwards the signal.
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log_file))
            .process_group(0)
            .kill_on_drop(true);
        #[cfg(target_os = "linux")]
        // SAFETY: prctl is async-signal-safe and touches only this process.
        unsafe {
            command.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == 0 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            });
        }
        let mut child = command.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("sidecar stdin was not piped"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("sidecar stdout was not piped"))?;
        let (frames_tx, frames) = mpsc::channel(16);
        let reader = tokio::spawn(read_frames(BufReader::new(stdout), frames_tx));
        Ok(Self {
            child,
            stdin,
            frames,
            reader,
            next_id: 0,
        })
    }

    pub(crate) fn id(&self) -> Option<u32> {
        self.child.id()
    }

    /// Sends one request and waits for its final reply within `bound`,
    /// handing every `progress` event for it to `on_progress`. When `cancel`
    /// answers true while waiting, a `cancel` header is written and the
    /// request ends as [`SidecarFailure::Cancelled`] once the sidecar
    /// acknowledges, or [`SidecarFailure::CancelUnacknowledged`] if it does
    /// not within the grace.
    pub(crate) async fn request(
        &mut self,
        mut header: serde_json::Map<String, serde_json::Value>,
        body: &[u8],
        bound: Duration,
        cancel: Option<&(dyn Fn() -> bool + Sync)>,
        on_progress: &mut (dyn FnMut(&serde_json::Value) + Send),
    ) -> Result<Reply, SidecarFailure> {
        self.next_id += 1;
        let id = self.next_id;
        header.insert("id".into(), id.into());
        if !body.is_empty() {
            header.insert("body_bytes".into(), body.len().into());
        }
        let mut line = serde_json::Value::Object(header).to_string();
        line.push('\n');
        let write = async {
            self.stdin.write_all(line.as_bytes()).await?;
            self.stdin.write_all(body).await?;
            self.stdin.flush().await
        };
        write
            .await
            .map_err(|error| SidecarFailure::Crashed(format!("stdin: {error}")))?;

        // Once a cancel has gone out the sidecar gets a short grace of its own
        // to acknowledge; the original bound no longer applies, but *some*
        // bound must, or a stuck sidecar holds the slot forever.
        let mut deadline = tokio::time::Instant::now() + bound;
        let mut cancel_sent = false;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let frame = tokio::select! {
                frame = self.frames.recv() => frame,
                _ = sleep(remaining.min(CANCEL_POLL)) => {
                    if remaining.is_zero() {
                        return Err(if cancel_sent {
                            SidecarFailure::CancelUnacknowledged
                        } else {
                            SidecarFailure::Timeout
                        });
                    }
                    if !cancel_sent && cancel.is_some_and(|cancelled| cancelled()) {
                        cancel_sent = true;
                        deadline = tokio::time::Instant::now() + CANCEL_GRACE;
                        let cancel_line = format!("{{\"op\":\"cancel\",\"id\":{id}}}\n");
                        if self.stdin.write_all(cancel_line.as_bytes()).await.is_err()
                            || self.stdin.flush().await.is_err()
                        {
                            return Err(SidecarFailure::Crashed("stdin closed".into()));
                        }
                    }
                    continue;
                }
            };
            let reply = match frame {
                Some(Ok(reply)) => reply,
                Some(Err(detail)) => return Err(SidecarFailure::Crashed(detail)),
                None => return Err(SidecarFailure::Crashed("stdout closed".into())),
            };
            if reply.header.get("id").and_then(serde_json::Value::as_u64) != Some(id) {
                return Err(SidecarFailure::Crashed(format!(
                    "reply id {:?} does not match request {id}",
                    reply.header.get("id")
                )));
            }
            if reply.header.get("event").is_some() {
                on_progress(&reply.header);
                continue;
            }
            return match reply.header.get("ok").and_then(serde_json::Value::as_bool) {
                Some(true) => Ok(reply),
                Some(false) => {
                    let class = string_field(&reply.header, "class");
                    if class == "cancelled" {
                        return Err(SidecarFailure::Cancelled);
                    }
                    Err(SidecarFailure::Refused {
                        class,
                        error: string_field(&reply.header, "error"),
                    })
                }
                None => Err(SidecarFailure::Crashed(
                    "reply header carries neither ok nor event".into(),
                )),
            };
        }
    }

    pub(crate) async fn kill(mut self) {
        self.reader.abort();
        if let Some(pid) = self.child.id().and_then(|pid| i32::try_from(pid).ok()) {
            // SAFETY: the child was spawned into its own process group whose
            // id is its pid; a group that has already exited is an ESRCH.
            unsafe {
                libc::kill(-pid, libc::SIGTERM);
            }
        }
        let _ = self.child.start_kill();
        let _ = timeout(Duration::from_secs(5), self.child.wait()).await;
    }
}

fn string_field(header: &serde_json::Value, name: &str) -> String {
    header
        .get(name)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn open_log(path: &Path) -> io::Result<fs::File> {
    let oversized = fs::metadata(path).is_ok_and(|metadata| metadata.len() > MAX_LOG_BYTES);
    OpenOptions::new()
        .create(true)
        .append(!oversized)
        .write(true)
        .truncate(oversized)
        .mode(0o600)
        .open(path)
}

async fn read_frames(
    mut stdout: BufReader<tokio::process::ChildStdout>,
    frames: mpsc::Sender<Result<Reply, String>>,
) {
    loop {
        let mut line = Vec::new();
        let read = (&mut stdout)
            .take(MAX_HEADER_BYTES)
            .read_until(b'\n', &mut line)
            .await;
        match read {
            Ok(0) => return,
            Ok(_) if line.last() != Some(&b'\n') => {
                let _ = frames
                    .send(Err("header line too long or unterminated".into()))
                    .await;
                return;
            }
            Ok(_) => {}
            Err(error) => {
                let _ = frames.send(Err(format!("stdout: {error}"))).await;
                return;
            }
        }
        let header: serde_json::Value = match serde_json::from_slice(&line) {
            Ok(serde_json::Value::Object(map)) => serde_json::Value::Object(map),
            _ => {
                let _ = frames.send(Err("malformed header line".into())).await;
                return;
            }
        };
        let body_bytes = header
            .get("body_bytes")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        if body_bytes > MAX_REPLY_BODY_BYTES as u64 {
            let _ = frames
                .send(Err(format!(
                    "body of {body_bytes} bytes exceeds the reply cap"
                )))
                .await;
            return;
        }
        let mut body = vec![0; body_bytes as usize];
        if body_bytes > 0
            && let Err(error) = stdout.read_exact(&mut body).await
        {
            let _ = frames.send(Err(format!("body: {error}"))).await;
            return;
        }
        if frames.send(Ok(Reply { header, body })).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// A `#!/bin/sh` stand-in for the Python sidecar: reads header lines and
    /// answers from a canned script, so framing and failure handling are
    /// tested without uv or a model.
    pub(crate) fn fake_sidecar(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    /// Echo-style sidecar: extracts the request id, answers `load` with ok,
    /// `speak` with a 5-byte body, `transcribe` with text, `boom` by exiting
    /// mid-request, `garbage` with a non-JSON line, `slow` after 2 s and
    /// `progress` with two progress events before ok.
    pub(crate) const ECHO_SIDECAR: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  op=$(printf '%s' "$line" | sed -n 's/.*"op":"\([a-z]*\)".*/\1/p')
  bytes=$(printf '%s' "$line" | sed -n 's/.*"body_bytes":\([0-9]*\).*/\1/p')
  if [ -n "$bytes" ]; then head -c "$bytes" >/dev/null; fi
  case "$op" in
    load) printf '{"id":%s,"ok":true,"load_millis":7}\n' "$id" ;;
    ping) printf '{"id":%s,"ok":true}\n' "$id" ;;
    transcribe) printf '{"id":%s,"ok":true,"text":"hello\\nworld","decode_millis":12}\n' "$id" ;;
    speak) printf '{"id":%s,"ok":true,"mime":"audio/mpeg","body_bytes":5}\n' "$id"; printf 'MP3!!' ;;
    refuse) printf '{"id":%s,"ok":false,"class":"network","error":"offline"}\n' "$id" ;;
    boom) exit 3 ;;
    garbage) printf 'not json at all\n' ;;
    wrongid) printf '{"id":999,"ok":true}\n' ;;
    slow) sleep 2; printf '{"id":%s,"ok":true}\n' "$id" ;;
    progress) printf '{"id":%s,"event":"progress","phase":"downloading","transferred":1,"total":2}\n' "$id"; printf '{"id":%s,"event":"progress","phase":"downloading","transferred":2,"total":2}\n' "$id"; printf '{"id":%s,"ok":true}\n' "$id" ;;
    cancelme) while IFS= read -r c; do case "$c" in *cancel*) printf '{"id":%s,"ok":false,"class":"cancelled","error":"provision cancelled"}\n' "$id"; break ;; esac; done ;;
    ignorecancel) sleep 30 ;;
    *) printf '{"id":%s,"ok":false,"class":"input","error":"unknown"}\n' "$id" ;;
  esac
done
"#;

    fn spawn_echo(dir: &Path) -> SidecarChild {
        let script = fake_sidecar(dir, "echo.sh", ECHO_SIDECAR);
        SidecarChild::spawn_with(Command::new(script), &log_path(dir)).unwrap()
    }

    fn op(name: &str) -> serde_json::Map<String, serde_json::Value> {
        let mut header = serde_json::Map::new();
        header.insert("op".into(), name.into());
        header
    }

    fn ignore(_: &serde_json::Value) {}

    #[tokio::test]
    async fn replies_are_matched_by_id_and_bodies_are_read_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = spawn_echo(dir.path());
        let bound = Duration::from_secs(5);
        let load = child
            .request(op("load"), &[], bound, None, &mut ignore)
            .await
            .unwrap();
        assert_eq!(load.header["id"], 1);
        assert_eq!(load.header["load_millis"], 7);
        let spoken = child
            .request(
                op("speak"),
                b"pcm-bytes-are-consumed",
                bound,
                None,
                &mut ignore,
            )
            .await
            .unwrap();
        assert_eq!(spoken.header["id"], 2);
        assert_eq!(spoken.body, b"MP3!!");
        let text = child
            .request(op("transcribe"), &[0; 8], bound, None, &mut ignore)
            .await
            .unwrap();
        assert_eq!(text.header["text"], "hello\nworld");
        let refused = child
            .request(op("refuse"), &[], bound, None, &mut ignore)
            .await
            .unwrap_err();
        assert!(matches!(refused, SidecarFailure::Refused { class, .. } if class == "network"));
        // A refusal leaves the child usable.
        assert!(
            child
                .request(op("load"), &[], bound, None, &mut ignore)
                .await
                .is_ok()
        );
        child.kill().await;
    }

    #[tokio::test]
    async fn exit_malformed_header_and_id_mismatch_are_crashes() {
        let dir = tempfile::tempdir().unwrap();
        let bound = Duration::from_secs(5);
        for (name, expected) in [
            ("boom", "stdout closed"),
            ("garbage", "malformed"),
            ("wrongid", "does not match"),
        ] {
            let mut child = spawn_echo(dir.path());
            let failure = child
                .request(op(name), &[], bound, None, &mut ignore)
                .await
                .unwrap_err();
            let SidecarFailure::Crashed(detail) = failure else {
                panic!("{name}: expected a crash, got {failure:?}");
            };
            assert!(detail.contains(expected), "{name}: {detail}");
            child.kill().await;
        }
    }

    #[tokio::test]
    async fn a_slow_reply_times_out_and_progress_events_are_forwarded() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = spawn_echo(dir.path());
        let failure = child
            .request(
                op("slow"),
                &[],
                Duration::from_millis(300),
                None,
                &mut ignore,
            )
            .await
            .unwrap_err();
        assert!(matches!(failure, SidecarFailure::Timeout));
        child.kill().await;

        let mut child = spawn_echo(dir.path());
        let mut seen = Vec::new();
        let mut record =
            |event: &serde_json::Value| seen.push(event["transferred"].as_u64().unwrap());
        child
            .request(
                op("progress"),
                &[],
                Duration::from_secs(5),
                None,
                &mut record,
            )
            .await
            .unwrap();
        assert_eq!(seen, [1, 2]);
        child.kill().await;
    }

    #[tokio::test]
    async fn a_cancelled_request_sends_cancel_and_ends_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = spawn_echo(dir.path());
        let cancel = AtomicBool::new(false);
        let cancelled = || cancel.load(Ordering::Acquire);
        let mut ignore_events = ignore;
        let request = child.request(
            op("cancelme"),
            &[],
            Duration::from_secs(5),
            Some(&cancelled),
            &mut ignore_events,
        );
        let flag = async {
            sleep(Duration::from_millis(150)).await;
            cancel.store(true, Ordering::Release);
        };
        let (outcome, ()) = tokio::join!(request, flag);
        assert!(matches!(outcome.unwrap_err(), SidecarFailure::Cancelled));
        child.kill().await;
    }

    /// A sidecar that never acknowledges the cancel must not hold the request
    /// past the grace period: that request holds the one sidecar slot.
    #[tokio::test]
    async fn an_unacknowledged_cancel_times_out_after_its_grace() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = spawn_echo(dir.path());
        let cancelled = || true;
        let mut ignore_events = ignore;
        let started = std::time::Instant::now();
        let outcome = child
            .request(
                op("ignorecancel"),
                &[],
                Duration::from_secs(60),
                Some(&cancelled),
                &mut ignore_events,
            )
            .await;
        assert!(matches!(
            outcome.unwrap_err(),
            SidecarFailure::CancelUnacknowledged
        ));
        let elapsed = started.elapsed();
        assert!(
            elapsed >= CANCEL_GRACE && elapsed < CANCEL_GRACE + Duration::from_secs(3),
            "{elapsed:?}"
        );
        child.kill().await;
    }

    #[test]
    fn the_script_is_written_once_and_replaces_older_versions() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("sidecar-0000000000000000.py"), "old").unwrap();
        let path = script_path(dir.path()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), SCRIPT);
        assert!(!dir.path().join("sidecar-0000000000000000.py").exists());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(script_path(dir.path()).unwrap(), path);
    }

    #[test]
    fn the_log_starts_over_only_when_oversized() {
        let dir = tempfile::tempdir().unwrap();
        let log = log_path(dir.path());
        fs::write(&log, b"keep").unwrap();
        drop(open_log(&log).unwrap());
        assert_eq!(fs::read(&log).unwrap(), b"keep");
        fs::write(&log, vec![b'x'; MAX_LOG_BYTES as usize + 1]).unwrap();
        drop(open_log(&log).unwrap());
        assert_eq!(fs::metadata(&log).unwrap().len(), 0);
    }
}
