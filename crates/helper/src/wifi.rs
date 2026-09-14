//! Команды `iw`: отключить устройство, задать регуляторный домен, прочитать аренды dnsmasq.

use std::fs;

use anyhow::Context;
use omarchy_hotspot_proto::{CountryCode, Iface, Mac};

use crate::run::{self, IW};

pub fn station_kick(ap: &Iface, mac: &Mac) -> anyhow::Result<()> {
    run::run(
        IW,
        &["dev", ap.as_str(), "station", "del", mac.as_str()],
        None,
    )?;
    Ok(())
}

pub fn regdom_set(cc: &CountryCode) -> anyhow::Result<()> {
    run::run(IW, &["reg", "set", cc.as_str()], None)?;
    Ok(())
}

/// Содержимое файла аренд `NetworkManager` для интерфейса; нет файла — пустая строка.
pub fn leases_read(ap: &Iface) -> anyhow::Result<String> {
    let path = format!("/var/lib/NetworkManager/dnsmasq-{}.leases", ap.as_str());
    match fs::read_to_string(&path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).context("cannot read leases"),
    }
}
