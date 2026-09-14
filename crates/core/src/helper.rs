//! Вызов помощника с правами root через `pkexec` (docs/SECURITY.md §4).

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::CoreError;
use crate::helper_proto::{HelperRequest, HelperResponse, SERVE_MAX_LINE};

/// Путь помощника — единственный, который разрешает polkit-политика.
pub const HELPER_PATH: &str = "/usr/lib/omarchy-hotspot/omarchy-hotspot-helper";
const PKEXEC: &str = "/usr/bin/pkexec";
// Коды pkexec: 126 — пользователь отказался, 127 — политика не разрешает.
const EXIT_DISMISSED: i32 = 126;
const EXIT_NOT_AUTHORIZED: i32 = 127;

pub trait HelperRunner: Send + Sync {
    /// Помощник установлен. Без него раздача не включается, а выключение и сброс работают.
    fn installed(&self) -> bool;
    /// Выполнить запрос; `Ok` — stdout помощника.
    fn call(&self, req: &HelperRequest) -> Result<String, CoreError>;
}

#[derive(Debug, Default)]
pub struct PkexecHelper;

impl HelperRunner for PkexecHelper {
    fn installed(&self) -> bool {
        Path::new(HELPER_PATH).is_file()
    }

    fn call(&self, req: &HelperRequest) -> Result<String, CoreError> {
        req.validate()
            .map_err(|e| CoreError::HelperFailed(e.to_string()))?;
        if !self.installed() {
            return Err(CoreError::HelperNotInstalled);
        }
        let args = req.to_args();
        tracing::debug!(?args, "helper");
        let out = Command::new(PKEXEC)
            .arg(HELPER_PATH)
            .args(&args)
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => CoreError::MissingTool(PKEXEC),
                _ => CoreError::Io(e),
            })?;
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
        match out.status.code() {
            Some(EXIT_DISMISSED) | Some(EXIT_NOT_AUTHORIZED) => Err(CoreError::HelperDenied),
            _ => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let line = stderr.trim().lines().last().unwrap_or("").to_string();
                Err(CoreError::HelperFailed(line))
            }
        }
    }
}

/// Предел на ответ помощника в режиме `serve`: файл аренд читается целиком,
/// но и он не может быть больше нескольких десятков килобайт.
const SERVE_MAX_RESPONSE: usize = 1024 * 1024;
/// Сколько ждём завершения помощника после закрытия канала.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// Помощник в режиме `serve` (SECURITY.md §5): один процесс `pkexec … serve` на всё время
/// раздачи, запросы и ответы — строки JSON по каналам процесса. Так фоновая служба не
/// запускает pkexec на каждое действие (каждый запуск — проверка polkit и запись в журнал).
#[derive(Default)]
pub struct ServeHelper {
    conn: Mutex<Option<Serve>>,
}

struct Serve {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl ServeHelper {
    pub fn new() -> ServeHelper {
        ServeHelper::default()
    }

    /// Помощник запущен (процесс жив).
    pub fn is_running(&self) -> bool {
        self.conn().is_some()
    }

    /// Закрыть канал: помощник видит EOF и завершается сам. Вызов блокирующий
    /// (ждёт завершения процесса), поэтому в службе выполняется в отдельном потоке.
    pub fn shutdown(&self) {
        if let Some(serve) = self.conn().take() {
            serve.close();
        }
    }

    /// Паника в другом потоке «отравляет» замок; данные при этом целы, поэтому берём их.
    fn conn(&self) -> MutexGuard<'_, Option<Serve>> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn spawn() -> Result<Serve, CoreError> {
        if !Path::new(HELPER_PATH).is_file() {
            return Err(CoreError::HelperNotInstalled);
        }
        let mut child = Command::new(PKEXEC)
            .arg(HELPER_PATH)
            .arg("serve")
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // stderr оставляем как есть: сообщения pkexec попадут в журнал службы.
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => CoreError::MissingTool(PKEXEC),
                _ => CoreError::Io(e),
            })?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        Ok(Serve {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }
}

impl Serve {
    /// Одна строка запроса — одна строка ответа.
    fn exchange(&mut self, line: &str) -> Result<HelperResponse, CoreError> {
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        let mut buf = Vec::with_capacity(256);
        let n = (&mut self.stdout)
            .take((SERVE_MAX_RESPONSE + 1) as u64)
            .read_until(b'\n', &mut buf)?;
        if n == 0 {
            return Err(CoreError::Io(std::io::Error::from(
                std::io::ErrorKind::UnexpectedEof,
            )));
        }
        if buf.len() > SERVE_MAX_RESPONSE {
            return Err(CoreError::HelperFailed("response too long".into()));
        }
        serde_json::from_slice(&buf)
            .map_err(|_| CoreError::HelperFailed("bad response from helper".into()))
    }

    /// Помощник умер: разобраться, отказал ли polkit (коды 126/127).
    fn exit_error(self) -> CoreError {
        let Serve {
            mut child, stdin, ..
        } = self;
        drop(stdin);
        match child.wait().ok().and_then(|s| s.code()) {
            Some(EXIT_DISMISSED) | Some(EXIT_NOT_AUTHORIZED) => CoreError::HelperDenied,
            _ => CoreError::HelperFailed("helper stopped".into()),
        }
    }

    fn close(self) {
        let Serve {
            mut child, stdin, ..
        } = self;
        drop(stdin);
        // Помощник завершается по EOF; ждём его, чтобы не оставлять зомби-процесс,
        // но не вечно: снять его сигналом мы всё равно не можем (он работает от root).
        let deadline = Instant::now() + CLOSE_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => {}
            }
            if Instant::now() >= deadline {
                tracing::warn!("root helper did not stop in time");
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl HelperRunner for ServeHelper {
    fn installed(&self) -> bool {
        Path::new(HELPER_PATH).is_file()
    }

    fn call(&self, req: &HelperRequest) -> Result<String, CoreError> {
        req.validate()
            .map_err(|e| CoreError::HelperFailed(e.to_string()))?;
        // Слишком длинный запрос помощник не примет — не отправляем вовсе (SECURITY.md §5).
        let line = serde_json::to_string(req)
            .map_err(|e| CoreError::HelperFailed(format!("cannot encode request: {e}")))?;
        if line.len() > SERVE_MAX_LINE {
            return Err(CoreError::HelperFailed("request too long".into()));
        }
        tracing::debug!(name = req.name(), "helper serve");
        let mut guard = self.conn();
        // Первый раз — запускаем; если помощник умер, пробуем один раз запустить заново.
        for attempt in 0..2 {
            let serve = match guard.as_mut() {
                Some(s) => s,
                None => guard.insert(ServeHelper::spawn()?),
            };
            match serve.exchange(&line) {
                Ok(resp) if resp.ok => return Ok(resp.output),
                Ok(resp) => return Err(CoreError::HelperFailed(resp.error)),
                Err(CoreError::Io(_)) => {
                    let dead = guard.take().expect("connection was present");
                    let err = dead.exit_error();
                    if attempt == 1 || matches!(err, CoreError::HelperDenied) {
                        return Err(err);
                    }
                }
                Err(e) => {
                    // Сбой формата (длинный или битый ответ): хвост мог остаться в канале, и следующий
                    // ответ пришёл бы не к своему запросу. Закрываем — следующий запрос откроет новый.
                    if let Some(broken) = guard.take() {
                        broken.close();
                    }
                    return Err(e);
                }
            }
        }
        Err(CoreError::HelperFailed("helper stopped".into()))
    }
}

impl Drop for ServeHelper {
    fn drop(&mut self) {
        let conn = self.conn.get_mut().unwrap_or_else(|e| e.into_inner());
        if let Some(serve) = conn.take() {
            serve.close();
        }
    }
}

/// Заглушка для тестов: запоминает запросы, может изображать отсутствие или ошибку.
#[cfg(test)]
pub mod mock {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    pub struct MockHelper {
        pub installed: bool,
        pub fail: bool,
        pub calls: Mutex<Vec<HelperRequest>>,
    }

    impl MockHelper {
        pub fn installed() -> Self {
            MockHelper {
                installed: true,
                ..Default::default()
            }
        }

        pub fn names(&self) -> Vec<&'static str> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.name())
                .collect()
        }
    }

    impl HelperRunner for MockHelper {
        fn installed(&self) -> bool {
            self.installed
        }

        fn call(&self, req: &HelperRequest) -> Result<String, CoreError> {
            if !self.installed {
                return Err(CoreError::HelperNotInstalled);
            }
            self.calls.lock().unwrap().push(req.clone());
            if self.fail {
                return Err(CoreError::HelperFailed("mock failure".into()));
            }
            Ok(String::new())
        }
    }
}
