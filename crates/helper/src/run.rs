//! Запуск внешних программ: абсолютный путь, массив аргументов, чистое окружение.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, bail};

pub const NFT: &str = "/usr/bin/nft";
pub const UFW: &str = "/usr/bin/ufw";
pub const IW: &str = "/usr/bin/iw";

pub struct Done {
    pub status: std::process::ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

impl Done {
    pub fn ok(&self) -> bool {
        self.status.success()
    }
}

/// Запускает `prog` (абсолютный путь) с аргументами; `stdin` — текст на вход, если нужен.
pub fn exec(prog: &str, args: &[&str], stdin: Option<&str>) -> anyhow::Result<Done> {
    debug_assert!(prog.starts_with('/'));
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .env_clear()
        .env("PATH", "/usr/bin")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().with_context(|| format!("cannot run {prog}"))?;
    if let Some(text) = stdin {
        let mut pipe = child.stdin.take().context("no stdin pipe")?;
        pipe.write_all(text.as_bytes())
            .with_context(|| format!("cannot write to {prog}"))?;
        drop(pipe);
    }
    let out = child
        .wait_with_output()
        .with_context(|| format!("cannot wait for {prog}"))?;
    Ok(Done {
        status: out.status,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Как `exec`, но ненулевой код выхода — ошибка с коротким текстом.
pub fn run(prog: &str, args: &[&str], stdin: Option<&str>) -> anyhow::Result<String> {
    let done = exec(prog, args, stdin)?;
    if !done.ok() {
        let what = prog.rsplit('/').next().unwrap_or(prog);
        let detail = done.stderr.trim().lines().next().unwrap_or("").to_string();
        bail!("{what} {} failed: {detail}", args.first().unwrap_or(&""));
    }
    Ok(done.stdout)
}
