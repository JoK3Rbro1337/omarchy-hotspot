use crate::CoreError;
use crate::helper_proto::Mac;
use crate::net::cmd;

/// Возможности Wi-Fi-карты из `iw list`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WifiCaps {
    pub ap: bool,
    pub sae: bool,
    pub channels_2ghz: Vec<u8>,
    /// Каналы 5 ГГц, на которых можно поднимать точку (без DFS и без «no IR»).
    pub channels_5ghz: Vec<u8>,
}

pub fn wifi_caps() -> Result<WifiCaps, CoreError> {
    Ok(parse_iw_list(&cmd::run("iw", &["list"])?))
}

/// Возможности карты, к которой относится интерфейс (`iw dev <if> info` → `iw phy phyN info`).
pub fn caps_for_iface(iface: &str) -> Result<WifiCaps, CoreError> {
    let info = cmd::run("iw", &["dev", iface, "info"])?;
    let idx = parse_wiphy_index(&info).ok_or_else(|| CoreError::Parse {
        tool: "iw",
        reason: "no wiphy index".into(),
    })?;
    let phy = format!("phy{idx}");
    Ok(parse_iw_list(&cmd::run("iw", &["phy", &phy, "info"])?))
}

/// `\twiphy 0` → 0.
pub fn parse_wiphy_index(text: &str) -> Option<u32> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("wiphy "))?
        .trim()
        .parse()
        .ok()
}

/// Текущий канал интерфейса (`iw dev <if> info`); `None`, если он не работает.
pub fn current_channel(iface: &str) -> Option<u8> {
    parse_current_channel(&cmd::run("iw", &["dev", iface, "info"]).ok()?)
}

/// `\tchannel 36 (5180 MHz), width: 20 MHz, center1: 5180 MHz` → 36.
pub fn parse_current_channel(text: &str) -> Option<u8> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("channel "))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

pub fn parse_iw_list(text: &str) -> WifiCaps {
    let mut caps = WifiCaps::default();
    let mut in_modes = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("Supported interface modes:") {
            in_modes = true;
            continue;
        }
        if in_modes {
            if let Some(mode) = t.strip_prefix("* ") {
                if mode == "AP" {
                    caps.ap = true;
                }
                continue;
            }
            in_modes = false;
        }
        if t.contains("Device supports SAE") {
            caps.sae = true;
        }
        if let Some((freq, ch)) = parse_freq_line(t) {
            match freq {
                2400..=2500 => caps.channels_2ghz.push(ch),
                5000..=5900 => caps.channels_5ghz.push(ch),
                _ => {}
            }
        }
    }
    caps.channels_2ghz.sort_unstable();
    caps.channels_2ghz.dedup();
    caps.channels_5ghz.sort_unstable();
    caps.channels_5ghz.dedup();
    caps
}

/// `* 5180.0 MHz [36] (22.0 dBm)` → (5180, 36). Каналы с ограничениями пропускаются.
fn parse_freq_line(t: &str) -> Option<(u32, u8)> {
    let rest = t.strip_prefix("* ")?;
    let (freq_s, rest) = rest.split_once(" MHz [")?;
    let (ch_s, flags) = rest.split_once(']')?;
    if ["disabled", "no IR", "radar"]
        .iter()
        .any(|f| flags.contains(f))
    {
        return None;
    }
    let freq = freq_s.split('.').next()?.parse().ok()?;
    let ch = ch_s.parse().ok()?;
    Some((freq, ch))
}

/// Одно подключённое устройство из `iw dev <ap> station dump`.
/// Байты считаются со стороны точки доступа: `rx_bytes` — принято от устройства,
/// `tx_bytes` — отправлено устройству (для гостя это его «скачивание»).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Station {
    pub mac: Mac,
    pub signal_dbm: Option<i32>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub connected_secs: u64,
    pub authorized: bool,
}

impl Station {
    fn new(mac: Mac) -> Station {
        Station {
            mac,
            signal_dbm: None,
            rx_bytes: 0,
            tx_bytes: 0,
            connected_secs: 0,
            // Строки `authorized` может не быть (старые версии iw) — считаем, что всё в порядке.
            authorized: true,
        }
    }
}

/// Подключённые устройства точки доступа. Root не нужен.
pub fn station_dump(iface: &str) -> Result<Vec<Station>, CoreError> {
    Ok(parse_station_dump(&cmd::run(
        "iw",
        &["dev", iface, "station", "dump"],
    )?))
}

/// Разбор `iw dev <ap> station dump`. Непонятные строки просто пропускаются.
pub fn parse_station_dump(text: &str) -> Vec<Station> {
    let mut out: Vec<Station> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Station ") {
            let mac = rest.split_whitespace().next().unwrap_or("");
            match Mac::parse(mac) {
                Ok(mac) => out.push(Station::new(mac)),
                Err(_) => tracing::debug!(mac, "station dump: bad MAC"),
            }
            continue;
        }
        let Some(st) = out.last_mut() else { continue };
        let Some((key, value)) = t.split_once(':') else {
            continue;
        };
        // «signal:  \t-45 [-50, -47] dBm» → первое слово значения.
        let first = value.split_whitespace().next().unwrap_or("");
        match key.trim() {
            "rx bytes" => st.rx_bytes = first.parse().unwrap_or(0),
            "tx bytes" => st.tx_bytes = first.parse().unwrap_or(0),
            // Значение бывает дробным («-45.5 dBm»), берём целую часть.
            "signal" => st.signal_dbm = first.split('.').next().and_then(|v| v.parse().ok()),
            "connected time" => st.connected_secs = first.parse().unwrap_or(0),
            "authorized" => st.authorized = first == "yes",
            _ => {}
        }
    }
    out
}

/// Код страны из `iw reg get` (`country UA: DFS-ETSI` → `UA`); `00` считается «не задан».
pub fn reg_country() -> Result<Option<String>, CoreError> {
    Ok(parse_reg_country(&cmd::run("iw", &["reg", "get"])?))
}

pub fn parse_reg_country(text: &str) -> Option<String> {
    let cc = text
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("country "))?
        .split(':')
        .next()?
        .trim();
    (cc.len() == 2 && cc != "00").then(|| cc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const IW_LIST: &str = "Wiphy phy0
\tSupported interface modes:
\t\t * managed
\t\t * AP
\t\t * monitor
\tBand 1:
\t\tFrequencies:
\t\t\t* 2412.0 MHz [1] (22.0 dBm)
\t\t\t* 2467.0 MHz [12] (disabled)
\tBand 2:
\t\tFrequencies:
\t\t\t* 5180.0 MHz [36] (22.0 dBm)
\t\t\t* 5260.0 MHz [52] (22.0 dBm) (no IR, radar detection)
\t\t\t* 5745.0 MHz [149] (22.0 dBm) (no IR)
\tDevice supports SAE with AUTHENTICATE command
";

    #[test]
    fn parses_current_channel() {
        let info = "Interface wlp15s0\n\ttype AP\n\twiphy 0\n\tssid Test\n\tchannel 36 (5180 MHz), width: 20 MHz, center1: 5180 MHz\n";
        assert_eq!(parse_current_channel(info), Some(36));
        assert_eq!(parse_current_channel("Interface x\n\ttype managed\n"), None);
    }

    #[test]
    fn parses_wiphy_index() {
        let info =
            "Interface wlp15s0\n\tifindex 3\n\ttype managed\n\twiphy 0\n\ttxpower 3.00 dBm\n";
        assert_eq!(parse_wiphy_index(info), Some(0));
        assert_eq!(parse_wiphy_index("Interface x\n"), None);
    }

    #[test]
    fn parses_caps() {
        let c = parse_iw_list(IW_LIST);
        assert!(c.ap && c.sae);
        assert_eq!(c.channels_2ghz, vec![1]);
        assert_eq!(c.channels_5ghz, vec![36]);
    }

    #[test]
    fn no_ap_without_mode() {
        let c = parse_iw_list("Wiphy phy0\n\tSupported interface modes:\n\t\t * managed\n");
        assert!(!c.ap);
    }

    const STATION_DUMP: &str = "Station 3c:2e:f5:11:22:33 (on wlp15s0)
\tinactive time:\t100 ms
\trx bytes:\t204800
\trx packets:\t512
\ttx bytes:\t1048576
\ttx packets:\t900
\tsignal:  \t-42 [-45, -48] dBm
\tsignal avg:\t-43 [-46, -49] dBm
\ttx bitrate:\t433.3 MBit/s VHT-MCS 9
\tauthorized:\tyes
\tauthenticated:\tyes
\tconnected time:\t3661 seconds
Station aa:bb:cc:dd:ee:01 (on wlp15s0)
\trx bytes:\t100
\ttx bytes:\t200
\tsignal:  \t-77 dBm
\tauthorized:\tno
\tconnected time:\t5 seconds
";

    #[test]
    fn parses_station_dump() {
        let st = parse_station_dump(STATION_DUMP);
        assert_eq!(st.len(), 2);
        assert_eq!(st[0].mac.as_str(), "3c:2e:f5:11:22:33");
        assert_eq!(st[0].signal_dbm, Some(-42));
        assert_eq!(st[0].rx_bytes, 204_800);
        assert_eq!(st[0].tx_bytes, 1_048_576);
        assert_eq!(st[0].connected_secs, 3661);
        assert!(st[0].authorized);
        assert_eq!(st[1].signal_dbm, Some(-77));
        assert!(!st[1].authorized);
    }

    #[test]
    fn station_dump_empty_and_broken() {
        assert!(parse_station_dump("").is_empty());
        // Строки без «Station» в начале не должны падать и ничего не создают.
        assert!(parse_station_dump("\trx bytes:\t10\n").is_empty());
        // Плохой MAC пропускается целиком вместе со своими строками.
        assert!(parse_station_dump("Station nonsense (on wlp15s0)\n\trx bytes:\t10\n").is_empty());
    }

    #[test]
    fn parses_country() {
        assert_eq!(
            parse_reg_country("global\ncountry UA: DFS-ETSI\n"),
            Some("UA".into())
        );
        assert_eq!(parse_reg_country("global\ncountry 00: DFS-UNSET\n"), None);
        assert_eq!(parse_reg_country(""), None);
    }
}
