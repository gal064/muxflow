use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, bail};
use tokio::{
    io::{self, AsyncWriteExt},
    net::UnixStream,
    process::Command,
    time::sleep,
};

pub async fn run(socket_path: PathBuf, auto_start: bool) -> anyhow::Result<()> {
    let stream = connect(&socket_path, auto_start).await?;
    let (mut socket_read, mut socket_write) = stream.into_split();
    let mut stdin = io::stdin();
    let mut stdout = io::stdout();

    let upload = async {
        io::copy(&mut stdin, &mut socket_write).await?;
        socket_write.shutdown().await
    };
    let download = async {
        io::copy(&mut socket_read, &mut stdout).await?;
        stdout.flush().await
    };
    tokio::try_join!(upload, download)?;
    Ok(())
}

async fn connect(path: &Path, auto_start: bool) -> anyhow::Result<UnixStream> {
    if let Ok(stream) = UnixStream::connect(path).await {
        return Ok(stream);
    }
    if !auto_start {
        bail!("host daemon is not available at {}", path.display());
    }

    let executable = std::env::current_exe().context("resolve host helper executable")?;
    Command::new(executable)
        .arg("daemon")
        .arg("--socket")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("start host daemon")?;

    for _ in 0..100 {
        if let Ok(stream) = UnixStream::connect(path).await {
            return Ok(stream);
        }
        sleep(Duration::from_millis(20)).await;
    }
    bail!("host daemon did not create {}", path.display())
}
