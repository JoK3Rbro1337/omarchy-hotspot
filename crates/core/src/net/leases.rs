//! Разбор файла аренд dnsmasq (`/var/lib/NetworkManager/dnsmasq-<ap>.leases`).
//! Сам файл читает только помощник (каталог 0700 root) — сюда приходит уже его вывод.
//!
//! Формат строки: `<когда истечёт> <MAC> <IP> <имя> <client-id>`.
//! Имя присылает само устройство по DHCP, то есть это ЧУЖИЕ данные: их обязательно
//! чистим (`sanitize_hostname`), иначе устройство могло бы «нарисовать» что угодно в окне.

use std::net::Ipv4Addr;

use crate::helper_proto::Mac;

/// Сколько символов имени показываем: длинное имя разъехалось бы по всей строке списка.
pub const HOSTNAME_MAX: usize = 24;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub mac: Mac,
    pub ip: Ipv4Addr,
    pub hostname: Option<String>,
}

/// Разбор всего файла аренд. Плохие строки пропускаются, для одного MAC остаётся последняя.
pub fn parse_leases(text: &str) -> Vec<Lease> {
    let mut out: Vec<Lease> = Vec::new();
    for line in text.lines() {
        let Some(lease) = parse_line(line) else {
            continue;
        };
        match out.iter_mut().find(|l| l.mac == lease.mac) {
            Some(old) => *old = lease,
            None => out.push(lease),
        }
    }
    out
}

fn parse_line(line: &str) -> Option<Lease> {
    let mut parts = line.split_whitespace();
    let _expiry = parts.next()?;
    let mac = Mac::parse(parts.next()?).ok()?;
    let ip: Ipv4Addr = parts.next()?.parse().ok()?;
    // `*` в файле dnsmasq означает «устройство не назвалось».
    let hostname = match parts.next() {
        Some("*") | None => None,
        Some(name) => sanitize_hostname(name),
    };
    Some(Lease { mac, ip, hostname })
}

/// Чистит имя, присланное устройством: убирает управляющие символы (в том числе escape-коды
/// терминала), невидимые и переворачивающие текст символы, обрезает длину. Пусто → `None`.
pub fn sanitize_hostname(raw: &str) -> Option<String> {
    let name: String = raw
        .chars()
        .filter(|&c| !c.is_control())
        .filter(|c| !is_invisible(*c))
        // Любой пробельный символ (в том числе неразрывный) заменяем обычным пробелом.
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .take(HOSTNAME_MAX)
        .collect();
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Символы нулевой ширины и управления направлением письма: не видны, но ломают строку.
fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{00ad}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}'
        | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206f}' | '\u{feff}'
        | '\u{fff9}'..='\u{fffb}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_leases_file() {
        let text = "1757606400 3c:2e:f5:11:22:33 10.42.0.34 Pixel-8 01:3c:2e:f5:11:22:33\n\
                    1757606400 aa:bb:cc:dd:ee:01 10.42.0.51 * 01:aa:bb:cc:dd:ee:01\n";
        let l = parse_leases(text);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].hostname.as_deref(), Some("Pixel-8"));
        assert_eq!(l[0].ip, Ipv4Addr::new(10, 42, 0, 34));
        assert_eq!(l[1].hostname, None);
    }

    #[test]
    fn keeps_last_lease_for_mac() {
        let text = "1 3c:2e:f5:11:22:33 10.42.0.34 old x\n2 3c:2e:f5:11:22:33 10.42.0.40 new x\n";
        let l = parse_leases(text);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].hostname.as_deref(), Some("new"));
        assert_eq!(l[0].ip, Ipv4Addr::new(10, 42, 0, 40));
    }

    #[test]
    fn skips_broken_lines() {
        let text = "\n# comment\n1 not-a-mac 10.42.0.5 name x\n1 3c:2e:f5:11:22:33 999.1.1.1 n x\n";
        assert!(parse_leases(text).is_empty());
    }

    #[test]
    fn hostname_is_sanitized() {
        // Escape-код терминала и перевод строки не должны попасть в окно.
        assert_eq!(
            sanitize_hostname("\u{1b}[31mEVIL\u{7}\nx"),
            Some("[31mEVILx".to_string())
        );
        assert_eq!(sanitize_hostname("a\u{202e}b\u{200b}c"), Some("abc".into()));
        assert_eq!(sanitize_hostname("\u{200b}"), None);
        assert_eq!(sanitize_hostname(""), None);
        let long = sanitize_hostname(&"x".repeat(100)).unwrap();
        assert_eq!(long.chars().count(), HOSTNAME_MAX);
    }
}
