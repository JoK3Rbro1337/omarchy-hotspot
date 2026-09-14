//! Протокол сокета «окно ↔ служба» (docs/DECISIONS.md §6) и простой клиент к нему.
//! Служба отвечает на каждый запрос, а подписавшимся клиентам шлёт ещё и события.
//! Пароль по этому сокету не передаётся никогда.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::CoreError;
use crate::devices::Device;
use crate::helper_proto::Mac;

/// Предел длины строки запроса и ответа (DECISIONS §6).
pub const MAX_LINE: usize = 64 * 1024;
/// Сколько ждать ответа на запрос, чтобы CLI не завис из-за неисправной службы.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Каталог службы в `$XDG_RUNTIME_DIR` (права 0700 ставит сама служба).
pub fn runtime_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty())?;
    Some(PathBuf::from(base).join("omarchy-hotspot"))
}

pub fn socket_path() -> Option<PathBuf> {
    runtime_dir().map(|d| d.join("daemon.sock"))
}

/// Запрос к службе.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Request {
    Ping,
    GetState,
    GetDevices,
    /// Включить раздачу (служба сама решает про режим одобрения).
    Start,
    Stop,
    /// Разрешить устройству выход в интернет (и запомнить его).
    Approve {
        mac: Mac,
    },
    /// Отказать новому устройству: в чёрный список и отключить от сети.
    Deny {
        mac: Mac,
    },
    Block {
        mac: Mac,
    },
    Unblock {
        mac: Mac,
    },
    Kick {
        mac: Mac,
    },
    ReloadConfig,
    /// После этого клиент получает ещё и события.
    Subscribe,
}

/// Состояние раздачи для клиента (строкой, чтобы протокол не зависел от внутренних типов).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireState {
    Off,
    Starting,
    On,
    Error,
}

/// Ответ службы.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Response {
    Pong,
    State {
        state: WireState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uplink: Option<String>,
        /// Время включения, секунды Unix.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<u64>,
        devices_count: usize,
        /// Режим одобрения действительно работает (раздача включена с белым списком).
        approval: bool,
    },
    Devices {
        list: Vec<Device>,
    },
    Ok,
    Err {
        code: String,
        msg: String,
    },
}

/// Событие службы (приходит только подписавшимся клиентам).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Event {
    StateChanged {
        state: WireState,
    },
    DeviceJoined {
        device: Device,
    },
    DeviceLeft {
        mac: Mac,
    },
    /// Новое устройство ждёт одобрения.
    DevicePending {
        device: Device,
    },
    DeviceApproved {
        mac: Mac,
    },
    DeviceBlocked {
        mac: Mac,
    },
    DeviceUnblocked {
        mac: Mac,
    },
    /// До авто-выключения по простою осталось столько минут.
    IdleWarning {
        minutes_left: u32,
    },
    /// До выключения по таймеру осталось столько минут.
    TimerWarning {
        minutes_left: u32,
    },
    /// Раздачу выключила сама служба (`reason`: `idle` или `timer`).
    Stopped {
        reason: String,
    },
}

/// Всё, что служба шлёт в сокет: ответы и события различаются полем `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ServerMsg {
    Response(Response),
    Event(Event),
}

/// Коды ошибок в `Response::Err`.
pub mod err_code {
    pub const BAD_REQUEST: &str = "bad_request";
    pub const NOT_SUPPORTED: &str = "not_supported";
    pub const FAILED: &str = "failed";
    pub const NO_DEVICE: &str = "no_device";
}

impl Response {
    pub fn err(code: &str, msg: impl Into<String>) -> Response {
        Response::Err {
            code: code.into(),
            msg: msg.into(),
        }
    }
}

/// Клиент службы: одна строка JSON — запрос, одна строка — ответ или событие.
pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Client {
    /// Подключиться к работающей службе. Службы нет — ошибка ввода-вывода.
    pub fn connect() -> Result<Client, CoreError> {
        let path =
            socket_path().ok_or_else(|| CoreError::Config("XDG_RUNTIME_DIR is not set".into()))?;
        let stream = UnixStream::connect(&path)?;
        let writer = stream.try_clone()?;
        Ok(Client {
            reader: BufReader::new(stream),
            writer,
        })
    }

    /// Ограничение на ожидание ответа. `None` — ждать сколько угодно (поток событий).
    pub fn set_timeout(&mut self, t: Option<Duration>) -> Result<(), CoreError> {
        self.reader.get_ref().set_read_timeout(t)?;
        Ok(())
    }

    pub fn send(&mut self, req: &Request) -> Result<(), CoreError> {
        let mut line = serde_json::to_string(req)
            .map_err(|e| CoreError::Config(format!("cannot encode request: {e}")))?;
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        self.writer.flush()?;
        Ok(())
    }

    /// Следующая строка от службы (ответ или событие). Конец связи — `Ok(None)`.
    pub fn recv(&mut self) -> Result<Option<ServerMsg>, CoreError> {
        let mut buf = Vec::with_capacity(256);
        let n = (&mut self.reader)
            .take((MAX_LINE + 1) as u64)
            .read_until(b'\n', &mut buf)?;
        if n == 0 {
            return Ok(None);
        }
        if buf.len() > MAX_LINE {
            return Err(CoreError::Config("daemon sent an oversized line".into()));
        }
        let msg = serde_json::from_slice(&buf)
            .map_err(|e| CoreError::Config(format!("cannot parse daemon message: {e}")))?;
        Ok(Some(msg))
    }

    /// Запрос и ответ на него; события, пришедшие между делом, пропускаются.
    pub fn request(&mut self, req: &Request) -> Result<Response, CoreError> {
        self.set_timeout(Some(REQUEST_TIMEOUT))?;
        self.send(req)?;
        loop {
            match self.recv()? {
                Some(ServerMsg::Response(r)) => return Ok(r),
                Some(ServerMsg::Event(_)) => {}
                None => return Err(CoreError::Config("daemon closed the connection".into())),
            }
        }
    }

    /// Отдельный отправитель для другого потока (чтение и запись идут независимо).
    pub fn try_clone_writer(&self) -> Result<UnixStream, CoreError> {
        Ok(self.writer.try_clone()?)
    }
}

/// Служба отвечает на сокете. Используется перед включением режима одобрения.
pub fn daemon_alive() -> bool {
    let Ok(mut c) = Client::connect() else {
        return false;
    };
    matches!(c.request(&Request::Ping), Ok(Response::Pong))
}

/// Отправить одну команду работающей службе. Службы нет — `Ok(None)`.
pub fn try_request(req: &Request) -> Result<Option<Response>, CoreError> {
    match Client::connect() {
        Ok(mut c) => c.request(req).map(Some),
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let req = Request::Approve {
            mac: Mac::parse("aa:bb:cc:dd:ee:01").unwrap(),
        };
        let line = serde_json::to_string(&req).unwrap();
        assert_eq!(line, r#"{"type":"approve","mac":"aa:bb:cc:dd:ee:01"}"#);
        assert_eq!(serde_json::from_str::<Request>(&line).unwrap(), req);
    }

    #[test]
    fn bad_mac_is_rejected() {
        let line = r#"{"type":"approve","mac":"ff:ff:ff:ff:ff:ff"}"#;
        assert!(serde_json::from_str::<Request>(line).is_err());
        let line = r#"{"type":"approve","mac":"hello"}"#;
        assert!(serde_json::from_str::<Request>(line).is_err());
    }

    #[test]
    fn unknown_request_type_is_rejected() {
        assert!(serde_json::from_str::<Request>(r#"{"type":"reboot"}"#).is_err());
        assert!(serde_json::from_str::<Request>("{}").is_err());
        // Лишние поля у команд с полями не принимаем.
        let extra = r#"{"type":"approve","mac":"aa:bb:cc:dd:ee:01","x":1}"#;
        assert!(serde_json::from_str::<Request>(extra).is_err());
    }

    #[test]
    fn events_and_responses_are_told_apart() {
        let ev = Event::DeviceLeft {
            mac: Mac::parse("aa:bb:cc:dd:ee:02").unwrap(),
        };
        let line = serde_json::to_string(&ev).unwrap();
        assert_eq!(
            serde_json::from_str::<ServerMsg>(&line).unwrap(),
            ServerMsg::Event(ev)
        );
        let resp = Response::State {
            state: WireState::On,
            uplink: Some("enp14s0".into()),
            since: Some(1_700_000_000),
            devices_count: 2,
            approval: true,
        };
        let line = serde_json::to_string(&resp).unwrap();
        assert_eq!(
            serde_json::from_str::<ServerMsg>(&line).unwrap(),
            ServerMsg::Response(resp)
        );
    }
}
