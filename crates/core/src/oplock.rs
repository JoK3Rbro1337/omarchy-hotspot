//! Замок на действия, которые меняют раздачу: включить, выключить, применить настройки, сбросить.
//! Их запускают CLI, окно, служба и значок на панели. Одновременно они перепутали бы шаги:
//! например, `off` снял бы правила брандмауэра уже после того, как новый `on` их поставил.
//! Замок держится только на время одного действия и никогда — во время запроса к службе
//! (служба сама берёт его для авто-выключения, иначе они ждали бы друг друга).

use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};

use crate::CoreError;
use crate::ipc;

/// Держит замок, пока жив.
pub type OpGuard = Flock<fs::File>;

/// Сколько ждать чужое действие: включение с окном pkexec может занять время.
const WAIT: Duration = Duration::from_secs(120);
const STEP: Duration = Duration::from_millis(100);

/// Взять замок. `None` — нет `$XDG_RUNTIME_DIR` (запуск вне сеанса): работаем без замка.
pub fn acquire() -> Result<Option<OpGuard>, CoreError> {
    let Some(dir) = ipc::runtime_dir() else {
        tracing::warn!("XDG_RUNTIME_DIR is not set: hotspot actions are not locked");
        return Ok(None);
    };
    acquire_in(&dir, WAIT).map(Some)
}

fn acquire_in(dir: &Path, wait: Duration) -> Result<OpGuard, CoreError> {
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(dir.join("hotspot.lock"))?;
    let deadline = Instant::now() + wait;
    let mut logged = false;
    loop {
        match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(guard) => return Ok(guard),
            Err((f, Errno::EWOULDBLOCK)) if Instant::now() < deadline => {
                if !logged {
                    tracing::info!("another hotspot action is running, waiting");
                    logged = true;
                }
                file = f;
                std::thread::sleep(STEP);
            }
            Err((_, Errno::EWOULDBLOCK)) => return Err(CoreError::Busy),
            Err((_, e)) => return Err(CoreError::Io(e.into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_action_waits_then_gives_up() {
        let dir = std::env::temp_dir().join(format!("oh-oplock-{}", std::process::id()));
        let first = acquire_in(&dir, Duration::ZERO).unwrap();
        let busy = acquire_in(&dir, Duration::from_millis(250));
        assert!(matches!(busy, Err(CoreError::Busy)), "{busy:?}");
        drop(first);
        acquire_in(&dir, Duration::ZERO).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }
}
