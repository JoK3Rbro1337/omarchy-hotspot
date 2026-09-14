//! Протокол помощника (docs/SECURITY.md §2, §5): типы аргументов с проверкой при создании,
//! запросы/ответы для режима `serve`, сборка аргументов командной строки.
//! Этот крейт — часть границы root: меняется только по SECURITY.md.

use std::fmt;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

/// Ошибка проверки аргумента (текст на английском, коротко — идёт в журнал и в ответы помощника).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ArgError(pub String);

fn err<T>(msg: impl Into<String>) -> Result<T, ArgError> {
    Err(ArgError(msg.into()))
}

/// Имя сетевого интерфейса: `^[A-Za-z0-9_.-]{1,15}$`, не начинается с `-` или `.`, не `lo`.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Iface(String);

impl Iface {
    pub fn parse(s: &str) -> Result<Iface, ArgError> {
        if s.is_empty() || s.len() > 15 {
            return err("interface name must be 1..15 characters");
        }
        if !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
        {
            return err("interface name has invalid characters");
        }
        // С `-` имя выглядело бы как опция для внешней программы, с `.` — как скрытый путь.
        if s.starts_with('-') || s.starts_with('.') || s == "lo" {
            return err("interface name not allowed");
        }
        Ok(Iface(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// MAC-адрес в нижнем регистре `xx:xx:xx:xx:xx:xx`, unicast, не нулевой.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Mac(String);

impl Mac {
    pub fn parse(s: &str) -> Result<Mac, ArgError> {
        if s.len() != 17 {
            return err("MAC must look like aa:bb:cc:dd:ee:ff");
        }
        let mut bytes = [0u8; 6];
        for (i, part) in s.split(':').enumerate() {
            if i >= 6 || part.len() != 2 || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
                return err("MAC must look like aa:bb:cc:dd:ee:ff");
            }
            bytes[i] = u8::from_str_radix(part, 16).map_err(|_| ArgError("bad MAC".into()))?;
        }
        if bytes == [0; 6] {
            return err("MAC must not be all zeros");
        }
        if bytes[0] & 1 == 1 {
            return err("MAC must be unicast");
        }
        Ok(Mac(s.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Код страны ISO 3166-1 alpha-2 (верхний регистр) или `00` — мировой домен.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CountryCode(String);

/// Все коды ISO 3166-1 alpha-2 (включая коды из wireless-regdb вроде `00`).
const COUNTRY_CODES: &str = "00 AD AE AF AG AI AL AM AO AQ AR AS AT AU AW AX AZ BA BB BD BE BF BG BH BI BJ \
BL BM BN BO BQ BR BS BT BV BW BY BZ CA CC CD CF CG CH CI CK CL CM CN CO CR CU CV CW CX CY CZ DE DJ DK DM \
DO DZ EC EE EG EH ER ES ET FI FJ FK FM FO FR GA GB GD GE GF GG GH GI GL GM GN GP GQ GR GS GT GU GW GY HK \
HM HN HR HT HU ID IE IL IM IN IO IQ IR IS IT JE JM JO JP KE KG KH KI KM KN KP KR KW KY KZ LA LB LC LI LK \
LR LS LT LU LV LY MA MC MD ME MF MG MH MK ML MM MN MO MP MQ MR MS MT MU MV MW MX MY MZ NA NC NE NF NG NI \
NL NO NP NR NU NZ OM PA PE PF PG PH PK PL PM PN PR PS PT PW PY QA RE RO RS RU RW SA SB SC SD SE SG SH SI \
SJ SK SL SM SN SO SR SS ST SV SX SY SZ TC TD TF TG TH TJ TK TL TM TN TO TR TT TV TW TZ UA UG UM US UY UZ \
VA VC VE VG VI VN VU WF WS YE YT ZA ZM ZW";

impl CountryCode {
    pub fn parse(s: &str) -> Result<CountryCode, ArgError> {
        if s.len() != 2 || !s.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return err("country code must be two letters");
        }
        let up = s.to_ascii_uppercase();
        if !COUNTRY_CODES.split(' ').any(|c| c == up) {
            return err("unknown country code");
        }
        Ok(CountryCode(up))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// DNS-сервер: обычный адрес (не `0.0.0.0`/`::`, не multicast, не loopback, не broadcast, не link-local).
/// IPv4 внутри IPv6 (`::ffff:a.b.c.d`, `::a.b.c.d`) не принимается: так обходится проверка loopback.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DnsServer(IpAddr);

impl DnsServer {
    pub fn parse(s: &str) -> Result<DnsServer, ArgError> {
        let ip: IpAddr = s
            .parse()
            .map_err(|_| ArgError("invalid IP address".into()))?;
        let special = match ip {
            IpAddr::V4(v4) => v4.is_broadcast() || v4.is_link_local(),
            IpAddr::V6(v6) => v6.is_unicast_link_local() || v6.to_ipv4().is_some(),
        };
        if ip.is_unspecified() || ip.is_multicast() || ip.is_loopback() || special {
            return err("DNS address must be a normal unicast address");
        }
        Ok(DnsServer(ip))
    }

    pub fn ip(&self) -> IpAddr {
        self.0
    }
}

pub const MAX_DNS_SERVERS: usize = 4;
pub const MAX_MAC_LIST: usize = 256;

/// Список MAC (`--allow`/`--block`): ≤ 256, без повторов.
pub fn check_mac_list(list: &[Mac]) -> Result<(), ArgError> {
    if list.len() > MAX_MAC_LIST {
        return err("too many MAC addresses");
    }
    let mut sorted: Vec<&Mac> = list.iter().collect();
    sorted.sort();
    if sorted.windows(2).any(|w| w[0] == w[1]) {
        return err("duplicate MAC address");
    }
    Ok(())
}

/// Список DNS: 1..=4, без повторов.
pub fn check_dns_list(list: &[DnsServer]) -> Result<(), ArgError> {
    if list.is_empty() || list.len() > MAX_DNS_SERVERS {
        return err("1 to 4 DNS servers expected");
    }
    for (i, a) in list.iter().enumerate() {
        if list[..i].contains(a) {
            return err("duplicate DNS server");
        }
    }
    Ok(())
}

macro_rules! string_newtype_conv {
    ($t:ty) => {
        impl TryFrom<String> for $t {
            type Error = ArgError;
            fn try_from(s: String) -> Result<Self, ArgError> {
                <$t>::parse(&s)
            }
        }
        impl From<$t> for String {
            fn from(v: $t) -> String {
                v.to_string()
            }
        }
        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
        impl fmt::Debug for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({:?})", stringify!($t), self.as_str())
            }
        }
        impl std::str::FromStr for $t {
            type Err = ArgError;
            fn from_str(s: &str) -> Result<Self, ArgError> {
                <$t>::parse(s)
            }
        }
    };
}
string_newtype_conv!(Iface);
string_newtype_conv!(Mac);
string_newtype_conv!(CountryCode);

impl TryFrom<String> for DnsServer {
    type Error = ArgError;
    fn try_from(s: String) -> Result<Self, ArgError> {
        DnsServer::parse(&s)
    }
}
impl From<DnsServer> for String {
    fn from(v: DnsServer) -> String {
        v.0.to_string()
    }
}
impl fmt::Display for DnsServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl fmt::Debug for DnsServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DnsServer({})", self.0)
    }
}
impl std::str::FromStr for DnsServer {
    type Err = ArgError;
    fn from_str(s: &str) -> Result<Self, ArgError> {
        DnsServer::parse(s)
    }
}

/// Запрос помощнику (SECURITY.md §2.1). Один и тот же тип для CLI-аргументов и режима `serve`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case", deny_unknown_fields)]
pub enum HelperRequest {
    FwApply {
        ap: Iface,
        uplink: Iface,
        #[serde(default)]
        approval: bool,
        #[serde(default)]
        guests_reach_pc: bool,
        #[serde(default)]
        allow: Vec<Mac>,
        #[serde(default)]
        block: Vec<Mac>,
    },
    FwClear,
    FwBlock {
        mac: Mac,
    },
    FwUnblock {
        mac: Mac,
    },
    FwAllowAdd {
        mac: Mac,
    },
    FwAllowRemove {
        mac: Mac,
    },
    StationKick {
        ap: Iface,
        mac: Mac,
    },
    RegdomSet {
        country: CountryCode,
    },
    DnsSet {
        servers: Vec<DnsServer>,
    },
    DnsClear,
    LeasesRead {
        ap: Iface,
    },
}

impl HelperRequest {
    /// Проверки списков (одиночные значения проверены при создании типов).
    pub fn validate(&self) -> Result<(), ArgError> {
        match self {
            HelperRequest::FwApply { allow, block, .. } => {
                check_mac_list(allow)?;
                check_mac_list(block)
            }
            HelperRequest::DnsSet { servers } => check_dns_list(servers),
            _ => Ok(()),
        }
    }

    /// Имя подкоманды (для журнала).
    pub fn name(&self) -> &'static str {
        use HelperRequest as R;
        match self {
            R::FwApply { .. } => "fw-apply",
            R::FwClear => "fw-clear",
            R::FwBlock { .. } => "fw-block",
            R::FwUnblock { .. } => "fw-unblock",
            R::FwAllowAdd { .. } => "fw-allow-add",
            R::FwAllowRemove { .. } => "fw-allow-remove",
            R::StationKick { .. } => "station-kick",
            R::RegdomSet { .. } => "regdom-set",
            R::DnsSet { .. } => "dns-set",
            R::DnsClear => "dns-clear",
            R::LeasesRead { .. } => "leases-read",
        }
    }

    /// Аргументы командной строки помощника (для вызова через pkexec).
    pub fn to_args(&self) -> Vec<String> {
        use HelperRequest as R;
        fn push(a: &mut Vec<String>, flag: &str, v: &dyn fmt::Display) {
            a.push(flag.into());
            a.push(v.to_string());
        }
        let mut a: Vec<String> = vec![self.name().into()];
        match self {
            R::FwApply {
                ap,
                uplink,
                approval,
                guests_reach_pc,
                allow,
                block,
            } => {
                push(&mut a, "--ap", ap);
                push(&mut a, "--uplink", uplink);
                if *approval {
                    a.push("--approval".into());
                }
                if *guests_reach_pc {
                    a.push("--guests-reach-pc".into());
                }
                for m in allow {
                    push(&mut a, "--allow", m);
                }
                for m in block {
                    push(&mut a, "--block", m);
                }
            }
            R::FwClear | R::DnsClear => {}
            R::FwBlock { mac }
            | R::FwUnblock { mac }
            | R::FwAllowAdd { mac }
            | R::FwAllowRemove { mac } => {
                push(&mut a, "--mac", mac);
            }
            R::StationKick { ap, mac } => {
                push(&mut a, "--ap", ap);
                push(&mut a, "--mac", mac);
            }
            R::RegdomSet { country } => a.push(country.to_string()),
            R::DnsSet { servers } => a.extend(servers.iter().map(ToString::to_string)),
            R::LeasesRead { ap } => push(&mut a, "--ap", ap),
        }
        a
    }
}

/// Ответ помощника в режиме `serve` (§5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
}

impl HelperResponse {
    pub fn ok(output: String) -> Self {
        HelperResponse {
            ok: true,
            output,
            error: String::new(),
        }
    }

    pub fn err(error: impl Into<String>) -> Self {
        HelperResponse {
            ok: false,
            output: String::new(),
            error: error.into(),
        }
    }
}

/// Предел длины строки запроса в режиме `serve` (без перевода строки). Самый большой допустимый
/// запрос — `fw-apply` с полными списками разрешённых и заблокированных (около 10,4 килобайта), остальное — запас.
pub const SERVE_MAX_LINE: usize = 32 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    const EVIL: &[&str] = &[
        "",
        " ",
        "wlan0 ",
        " wlan0",
        "wlan0;id",
        "$(id)",
        "`id`",
        "wlan0\n",
        "wlan0\0",
        "../etc",
        "wlан0",
        "wlan0|cat",
        "a b",
        "'",
        "\"",
        "-",
        "-h",
        "--ap",
        ".hidden",
    ];

    #[test]
    fn iface_accepts_normal_names() {
        for ok in [
            "wlp15s0", "enp14s0", "wlan0", "tun0", "br-1a2b", "a", "eth0.100",
        ] {
            assert!(Iface::parse(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn iface_rejects_evil() {
        for bad in EVIL {
            assert!(Iface::parse(bad).is_err(), "{bad:?}");
        }
        assert!(Iface::parse(".").is_err());
        assert!(Iface::parse("..").is_err());
        assert!(Iface::parse("lo").is_err());
        assert!(Iface::parse("abcdefghijklmnop").is_err()); // 16 символов
        assert!(Iface::parse("abcdefghijklmno").is_ok()); // 15 символов
    }

    #[test]
    fn mac_normalizes_and_rejects() {
        assert_eq!(
            Mac::parse("AA:BB:CC:DD:EE:0F").unwrap().as_str(),
            "aa:bb:cc:dd:ee:0f"
        );
        for bad in [
            "aa:bb:cc:dd:ee",
            "aa:bb:cc:dd:ee:ff:00",
            "aa-bb-cc-dd-ee-ff",
            "aabbccddeeff",
            "aa:bb:cc:dd:ee:fg",
            "00:00:00:00:00:00",
            "ff:ff:ff:ff:ff:ff",
            "01:00:5e:00:00:01",
            "aa:bb:cc:dd:ee:ff\n",
            " aa:bb:cc:dd:ee:ff",
            "aa:bb:cc:dd:ee:ff;",
            "$(id):bb:cc:dd:ee:ff",
            "",
        ] {
            assert!(Mac::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn country_code() {
        assert_eq!(CountryCode::parse("ua").unwrap().as_str(), "UA");
        assert_eq!(CountryCode::parse("00").unwrap().as_str(), "00");
        for bad in ["", "U", "UAA", "XX", "ZZ", "U;", "ua\n", "ЮА", "$(", "--"] {
            assert!(CountryCode::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn dns_servers() {
        assert!(DnsServer::parse("1.1.1.1").is_ok());
        assert!(DnsServer::parse("2606:4700:4700::1111").is_ok());
        for bad in [
            "0.0.0.0",
            "::",
            "127.0.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "169.254.1.1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:1.1.1.1",
            "::127.0.0.1",
            "1.1.1",
            "1.1.1.1;",
            "dns.example",
            "",
            "1.1.1.1 ",
        ] {
            assert!(DnsServer::parse(bad).is_err(), "{bad:?}");
        }
        let one = DnsServer::parse("1.1.1.1").unwrap();
        assert!(check_dns_list(&[]).is_err());
        assert!(check_dns_list(&[one, one]).is_err());
        assert!(check_dns_list(&[one; 5]).is_err());
        assert!(check_dns_list(&[one]).is_ok());
    }

    #[test]
    fn mac_lists() {
        let m = Mac::parse("aa:bb:cc:dd:ee:01").unwrap();
        assert!(check_mac_list(&[m.clone(), m.clone()]).is_err());
        assert!(check_mac_list(&vec![m.clone(); MAX_MAC_LIST + 1]).is_err());
        let req = HelperRequest::FwApply {
            ap: Iface::parse("wlan0").unwrap(),
            uplink: Iface::parse("eth0").unwrap(),
            approval: false,
            guests_reach_pc: false,
            allow: vec![m.clone()],
            block: vec![m.clone(), m],
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn json_roundtrip_and_validation_inside_serde() {
        let req = HelperRequest::FwBlock {
            mac: Mac::parse("AA:BB:CC:DD:EE:FF").unwrap(),
        };
        let j = serde_json::to_string(&req).unwrap();
        assert_eq!(j, r#"{"cmd":"fw-block","mac":"aa:bb:cc:dd:ee:ff"}"#);
        assert_eq!(serde_json::from_str::<HelperRequest>(&j).unwrap(), req);
        // Плохой MAC не проходит даже через JSON.
        assert!(serde_json::from_str::<HelperRequest>(r#"{"cmd":"fw-block","mac":"x"}"#).is_err());
        // Неизвестные поля и команды — ошибка.
        assert!(
            serde_json::from_str::<HelperRequest>(
                r#"{"cmd":"fw-block","mac":"aa:bb:cc:dd:ee:ff","x":1}"#
            )
            .is_err()
        );
        assert!(serde_json::from_str::<HelperRequest>(r#"{"cmd":"serve"}"#).is_err());
        assert!(serde_json::from_str::<HelperRequest>(r#"{"cmd":"rm"}"#).is_err());
    }

    #[test]
    fn args_for_pkexec() {
        let req = HelperRequest::FwApply {
            ap: Iface::parse("wlp15s0").unwrap(),
            uplink: Iface::parse("enp14s0").unwrap(),
            approval: true,
            guests_reach_pc: false,
            allow: vec![Mac::parse("aa:bb:cc:dd:ee:01").unwrap()],
            block: vec![],
        };
        assert_eq!(
            req.to_args(),
            [
                "fw-apply",
                "--ap",
                "wlp15s0",
                "--uplink",
                "enp14s0",
                "--approval",
                "--allow",
                "aa:bb:cc:dd:ee:01"
            ]
        );
        let req = HelperRequest::DnsSet {
            servers: vec![DnsServer::parse("1.1.1.1").unwrap()],
        };
        assert_eq!(req.to_args(), ["dns-set", "1.1.1.1"]);
        let req = HelperRequest::RegdomSet {
            country: CountryCode::parse("ua").unwrap(),
        };
        assert_eq!(req.to_args(), ["regdom-set", "UA"]);
    }

    #[test]
    fn response_json() {
        let r = HelperResponse::err("bad");
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"ok":false,"error":"bad"}"#
        );
        let r = HelperResponse::ok(String::new());
        assert_eq!(serde_json::to_string(&r).unwrap(), r#"{"ok":true}"#);
    }

    #[test]
    fn largest_request_fits_serve_line() {
        let mac =
            |i: usize| Mac::parse(&format!("02:00:00:00:{:02x}:{:02x}", i / 256, i % 256)).unwrap();
        let req = HelperRequest::FwApply {
            ap: Iface::parse("wlp15s0abcdefgh").unwrap(),
            uplink: Iface::parse("enp14s0abcdefgh").unwrap(),
            approval: true,
            guests_reach_pc: true,
            allow: (0..MAX_MAC_LIST).map(mac).collect(),
            block: (MAX_MAC_LIST..2 * MAX_MAC_LIST).map(mac).collect(),
        };
        req.validate().unwrap();
        let line = serde_json::to_string(&req).unwrap();
        assert!(
            line.len() <= SERVE_MAX_LINE,
            "{} > {SERVE_MAX_LINE}",
            line.len()
        );
    }
}
