//! Соответствие MAC → IP из таблицы соседей ядра (`ip -j -4 neigh show dev <ap>`). Root не нужен.
//! Это запасной источник IP: имя устройства и надёжный IP дают аренды dnsmasq (нужен помощник).

use std::net::Ipv4Addr;

use serde::Deserialize;

use crate::CoreError;
use crate::helper_proto::Mac;
use crate::net::cmd;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Neigh {
    pub mac: Mac,
    pub ip: Ipv4Addr,
}

/// Строка вывода `ip -j`. Незнакомые поля игнорируются.
#[derive(Deserialize)]
struct RawNeigh {
    dst: String,
    lladdr: Option<String>,
    #[serde(default)]
    state: Vec<String>,
}

pub fn neigh_table(iface: &str) -> Result<Vec<Neigh>, CoreError> {
    let out = cmd::run("ip", &["-j", "-4", "neigh", "show", "dev", iface])?;
    Ok(parse_neigh_json(&out))
}

/// Разбор JSON от `ip -j -4 neigh`. Записи без MAC и «мусорные» (FAILED, INCOMPLETE) отброшены.
pub fn parse_neigh_json(text: &str) -> Vec<Neigh> {
    let raw: Vec<RawNeigh> = match serde_json::from_str(text.trim()) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("cannot parse ip neigh output: {e}");
            return Vec::new();
        }
    };
    raw.into_iter()
        .filter(|n| {
            !n.state
                .iter()
                .any(|s| s == "FAILED" || s == "INCOMPLETE" || s == "NOARP")
        })
        .filter_map(|n| {
            Some(Neigh {
                mac: Mac::parse(n.lladdr.as_deref()?).ok()?,
                ip: n.dst.parse().ok()?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_neigh_json() {
        let json = r#"[{"dst":"10.42.0.34","lladdr":"3c:2e:f5:11:22:33","state":["REACHABLE"]},
            {"dst":"10.42.0.51","lladdr":"aa:bb:cc:dd:ee:01","state":["STALE"]},
            {"dst":"10.42.0.99","state":["FAILED"]},
            {"dst":"10.42.0.77","lladdr":"aa:bb:cc:dd:ee:02","state":["FAILED"]}]"#;
        let n = parse_neigh_json(json);
        assert_eq!(n.len(), 2);
        assert_eq!(n[0].ip, Ipv4Addr::new(10, 42, 0, 34));
        assert_eq!(n[1].mac.as_str(), "aa:bb:cc:dd:ee:01");
    }

    #[test]
    fn broken_json_is_empty() {
        assert!(parse_neigh_json("").is_empty());
        assert!(parse_neigh_json("not json").is_empty());
        assert!(parse_neigh_json("[]").is_empty());
    }
}
