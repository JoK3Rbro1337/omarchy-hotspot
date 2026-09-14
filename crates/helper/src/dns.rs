//! Файл DNS для dnsmasq `NetworkManager` в режиме shared (SECURITY.md §2.1: dns-set / dns-clear).

use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::Context;
use omarchy_hotspot_proto::DnsServer;

const DIR: &str = "/etc/NetworkManager/dnsmasq-shared.d";
const FILE: &str = "/etc/NetworkManager/dnsmasq-shared.d/omarchy-hotspot.conf";

pub fn render(servers: &[DnsServer]) -> String {
    let mut s = String::from("# omarchy-hotspot: managed file, do not edit\nno-resolv\n");
    for ip in servers {
        let _ = writeln!(s, "server={ip}");
    }
    s
}

pub fn set(servers: &[DnsServer]) -> anyhow::Result<()> {
    let dir = Path::new(DIR);
    if !dir.is_dir() {
        fs::create_dir(dir).context("cannot create dnsmasq-shared.d")?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755))?;
    }
    // Пишем во временный файл рядом и переименовываем — файл всегда целый.
    let tmp = format!("{FILE}.tmp");
    // Старый временный файл убираем и создаём заново: за символической ссылкой не идём.
    match fs::remove_file(&tmp) {
        Ok(()) | Err(_) => {}
    }
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&tmp)
            .context("cannot write dns file")?;
        f.write_all(render(servers).as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, FILE).context("cannot replace dns file")?;
    Ok(())
}

pub fn clear() -> anyhow::Result<()> {
    match fs::remove_file(FILE) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).context("cannot remove dns file"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_servers() {
        let a = DnsServer::parse("1.1.1.1").unwrap();
        let b = DnsServer::parse("2606:4700:4700::1111").unwrap();
        assert_eq!(
            render(&[a, b]),
            "# omarchy-hotspot: managed file, do not edit\nno-resolv\nserver=1.1.1.1\nserver=2606:4700:4700::1111\n"
        );
    }
}
