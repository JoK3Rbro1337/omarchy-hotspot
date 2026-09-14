//! Сокет службы: `$XDG_RUNTIME_DIR/omarchy-hotspot/daemon.sock` (каталог 0700, сокет 0600).
//! На каждого клиента — два задания: одно читает запросы, другое пишет ответы и события.
//! Так ни одно чтение не обрывается на середине (в `select!` это было бы легко испортить).

use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use omarchy_hotspot_core::ipc::{self, Event, MAX_LINE, Request, Response, ServerMsg, err_code};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedReadHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, oneshot};

/// Запрос клиента, переданный в главный цикл службы.
pub struct ClientCmd {
    pub req: Request,
    pub reply: oneshot::Sender<Response>,
}

/// Открытый сокет службы вместе с замком: пока живёт `lock`, второй службы быть не может.
pub struct Bound {
    pub listener: UnixListener,
    pub path: PathBuf,
    pub lock: nix::fcntl::Flock<fs::File>,
}

/// Что уходит клиенту (плюс внутренний знак «этот клиент подписался на события»).
enum Out {
    Msg(Box<ServerMsg>),
    Subscribe,
}

/// Открыть сокет. Единственность службы держит файл-замок: только его владелец имеет право
/// убрать оставшийся от прошлого раза сокет (одной проверки «отвечает ли он» мало —
/// она может не пройти и при живой службе).
pub fn bind() -> anyhow::Result<Bound> {
    let dir = ipc::runtime_dir().context("XDG_RUNTIME_DIR is not set")?;
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    let lock = lock_daemon(&dir)?;
    let path = dir.join("daemon.sock");
    if path.exists() {
        fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    tracing::info!(socket = %path.display(), "daemon socket is open");
    Ok(Bound {
        listener,
        path,
        lock,
    })
}

fn lock_daemon(dir: &std::path::Path) -> anyhow::Result<nix::fcntl::Flock<fs::File>> {
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(dir.join("daemon.lock"))?;
    nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock)
        .map_err(|_| anyhow::anyhow!("another omarchy-hotspot daemon is already running"))
}

pub async fn accept_loop(
    listener: UnixListener,
    cmds: mpsc::Sender<ClientCmd>,
    events: broadcast::Sender<Event>,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let cmds = cmds.clone();
                let events = events.subscribe();
                tokio::spawn(async move {
                    if let Err(e) = client(stream, cmds, events).await {
                        tracing::debug!("client connection: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::warn!("cannot accept client: {e}");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

async fn client(
    stream: UnixStream,
    cmds: mpsc::Sender<ClientCmd>,
    events: broadcast::Receiver<Event>,
) -> anyhow::Result<()> {
    // Каталог сокета и так 0700, но проверим, кто на том конце: сокет — доступ к раздаче.
    let cred = stream.peer_cred()?;
    if cred.uid() != nix::unistd::geteuid().as_raw() {
        tracing::warn!(uid = cred.uid(), "client from another user rejected");
        return Ok(());
    }
    let (rd, wr) = stream.into_split();
    let (out_tx, out_rx) = mpsc::channel::<Out>(64);
    let writer = tokio::spawn(writer_task(wr, out_rx, events));

    let mut reader = BufReader::new(rd);
    let mut buf = Vec::with_capacity(512);
    loop {
        let n = read_line(&mut reader, &mut buf).await?;
        if n == 0 {
            break; // клиент закрыл связь
        }
        if buf.len() > MAX_LINE {
            let resp = Response::err(err_code::BAD_REQUEST, "request too long");
            let _ = out_tx
                .send(Out::Msg(Box::new(ServerMsg::Response(resp))))
                .await;
            break; // остаток такой строки не читаем
        }
        let resp = match serde_json::from_slice::<Request>(&buf) {
            Ok(Request::Subscribe) => {
                if out_tx.send(Out::Subscribe).await.is_err() {
                    break;
                }
                Response::Ok
            }
            Ok(req) => ask(&cmds, req).await,
            Err(e) => {
                tracing::debug!("bad request: {e}");
                Response::err(err_code::BAD_REQUEST, "bad request")
            }
        };
        if out_tx
            .send(Out::Msg(Box::new(ServerMsg::Response(resp))))
            .await
            .is_err()
        {
            break;
        }
    }
    drop(out_tx);
    let _ = writer.await;
    Ok(())
}

/// Передать запрос в главный цикл и дождаться ответа.
async fn ask(cmds: &mpsc::Sender<ClientCmd>, req: Request) -> Response {
    let (reply, answer) = oneshot::channel();
    if cmds.send(ClientCmd { req, reply }).await.is_err() {
        return Response::err(err_code::FAILED, "service is shutting down");
    }
    answer
        .await
        .unwrap_or_else(|_| Response::err(err_code::FAILED, "no answer from the service"))
}

async fn writer_task(
    mut wr: tokio::net::unix::OwnedWriteHalf,
    mut out_rx: mpsc::Receiver<Out>,
    mut events: broadcast::Receiver<Event>,
) {
    let mut subscribed = false;
    loop {
        // Обе ветки безопасно прерываются: `recv` у канала и у рассылки не теряет данные.
        tokio::select! {
            out = out_rx.recv() => match out {
                Some(Out::Subscribe) => subscribed = true,
                Some(Out::Msg(msg)) => {
                    if write_msg(&mut wr, &msg).await.is_err() {
                        return;
                    }
                }
                None => return,
            },
            ev = events.recv(), if subscribed => match ev {
                Ok(ev) => {
                    if write_msg(&mut wr, &ServerMsg::Event(ev)).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!("client is slow: {n} events dropped");
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
        }
    }
}

async fn write_msg(
    wr: &mut tokio::net::unix::OwnedWriteHalf,
    msg: &ServerMsg,
) -> std::io::Result<()> {
    let mut line = serde_json::to_string(msg).map_err(std::io::Error::other)?;
    line.push('\n');
    wr.write_all(line.as_bytes()).await?;
    wr.flush().await
}

/// Строка запроса с ограничением длины: длиннее предела — не читаем совсем.
async fn read_line(
    reader: &mut BufReader<OwnedReadHalf>,
    buf: &mut Vec<u8>,
) -> std::io::Result<usize> {
    buf.clear();
    let mut limited = (&mut *reader).take((MAX_LINE + 1) as u64);
    limited.read_until(b'\n', buf).await
}
