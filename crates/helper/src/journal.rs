//! Запись в системный журнал через `/dev/log` (syslog). Без зависимостей, без паролей.

use std::os::unix::net::UnixDatagram;

const TAG: &str = "omarchy-hotspot-helper";
// facility daemon(3) * 8 + severity: info = 6, err = 3
const PRI_INFO: u8 = 30;
const PRI_ERR: u8 = 27;

fn send(pri: u8, msg: &str) {
    // Журнал — вспомогательный: если /dev/log недоступен, работу не прерываем.
    let Ok(sock) = UnixDatagram::unbound() else {
        return;
    };
    let pid = std::process::id();
    let line = format!("<{pri}>{TAG}[{pid}]: {msg}");
    let _ = sock.send_to(line.as_bytes(), "/dev/log");
}

pub fn info(msg: &str) {
    send(PRI_INFO, msg);
}

pub fn error(msg: &str) {
    use std::io::Write;
    send(PRI_ERR, msg);
    // Закрытый stderr — не повод падать.
    let _ = writeln!(std::io::stderr(), "{TAG}: {msg}");
}
