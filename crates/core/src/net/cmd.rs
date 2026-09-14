use std::io;
use std::process::{Command, Output, Stdio};

use crate::CoreError;

fn exec(tool: &'static str, args: &[&str]) -> Result<Output, CoreError> {
    Command::new(tool)
        .args(args)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => CoreError::MissingTool(tool),
            _ => CoreError::Io(e),
        })
}

/// Запускает программу с массивом аргументов (без shell) и возвращает stdout.
pub fn run(tool: &'static str, args: &[&str]) -> Result<String, CoreError> {
    tracing::debug!(tool, ?args, "run");
    let out = exec(tool, args)?;
    if !out.status.success() {
        return Err(CoreError::ToolFailed {
            tool,
            code: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Как `run`, но среди аргументов есть пароль: аргументы не логируются,
/// а текст ошибки программы не сохраняется (вдруг она повторила значение).
pub fn run_secret(tool: &'static str, args: &[&str]) -> Result<String, CoreError> {
    tracing::debug!(tool, "run (arguments hidden)");
    let out = exec(tool, args)?;
    if !out.status.success() {
        return Err(CoreError::ToolFailed {
            tool,
            code: out.status.code().unwrap_or(-1),
            stderr: "(hidden: the command contained a password)".into(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Как `run`, но ненулевой код выхода — это `Ok(false)`, а не ошибка.
pub fn succeeds(tool: &'static str, args: &[&str]) -> Result<bool, CoreError> {
    match run(tool, args) {
        Ok(_) => Ok(true),
        Err(CoreError::ToolFailed { .. }) => Ok(false),
        Err(e) => Err(e),
    }
}
