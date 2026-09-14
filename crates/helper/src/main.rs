//! omarchy-hotspot-helper — единственная часть с правами root (docs/SECURITY.md).
//! Запускается через pkexec. Все аргументы проверяются до любого действия.

mod dns;
mod journal;
mod nft;
mod run;
mod serve;
mod sys;
mod ufw;
mod wifi;

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use omarchy_hotspot_proto::{CountryCode, DnsServer, HelperRequest, Iface, Mac};

#[derive(Parser)]
#[command(
    name = "omarchy-hotspot-helper",
    about = "Root helper for omarchy-hotspot (run via pkexec)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Apply firewall rules for the hotspot (nftables + ufw if active)
    FwApply {
        #[arg(long)]
        ap: Iface,
        #[arg(long)]
        uplink: Iface,
        /// Forward traffic only for MACs from --allow (approval mode)
        #[arg(long)]
        approval: bool,
        /// Do not hide the PC from guests (ufw rules still apply)
        #[arg(long)]
        guests_reach_pc: bool,
        #[arg(long)]
        allow: Vec<Mac>,
        #[arg(long)]
        block: Vec<Mac>,
    },
    /// Remove all firewall rules of the hotspot
    FwClear,
    FwBlock {
        #[arg(long)]
        mac: Mac,
    },
    FwUnblock {
        #[arg(long)]
        mac: Mac,
    },
    FwAllowAdd {
        #[arg(long)]
        mac: Mac,
    },
    FwAllowRemove {
        #[arg(long)]
        mac: Mac,
    },
    /// Disconnect a station from the access point
    StationKick {
        #[arg(long)]
        ap: Iface,
        #[arg(long)]
        mac: Mac,
    },
    /// Set the Wi-Fi regulatory domain (country code)
    RegdomSet {
        country: CountryCode,
    },
    /// Set DNS servers for hotspot clients (1..4)
    DnsSet {
        servers: Vec<DnsServer>,
    },
    DnsClear,
    /// Print the dnsmasq leases file of the AP interface
    LeasesRead {
        #[arg(long)]
        ap: Iface,
    },
    /// Service mode: JSON requests on stdin, JSON responses on stdout
    Serve,
}

impl Cmd {
    fn into_request(self) -> Option<HelperRequest> {
        use HelperRequest as R;
        Some(match self {
            Cmd::FwApply {
                ap,
                uplink,
                approval,
                guests_reach_pc,
                allow,
                block,
            } => R::FwApply {
                ap,
                uplink,
                approval,
                guests_reach_pc,
                allow,
                block,
            },
            Cmd::FwClear => R::FwClear,
            Cmd::FwBlock { mac } => R::FwBlock { mac },
            Cmd::FwUnblock { mac } => R::FwUnblock { mac },
            Cmd::FwAllowAdd { mac } => R::FwAllowAdd { mac },
            Cmd::FwAllowRemove { mac } => R::FwAllowRemove { mac },
            Cmd::StationKick { ap, mac } => R::StationKick { ap, mac },
            Cmd::RegdomSet { country } => R::RegdomSet { country },
            Cmd::DnsSet { servers } => R::DnsSet { servers },
            Cmd::DnsClear => R::DnsClear,
            Cmd::LeasesRead { ap } => R::LeasesRead { ap },
            Cmd::Serve => return None,
        })
    }
}

/// Файл-замок: одновременно работает только один помощник (иначе номера правил ufw сдвигаются).
const LOCK_PATH: &str = "/run/omarchy-hotspot-helper.lock";

fn lock() -> anyhow::Result<nix::fcntl::Flock<std::fs::File>> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(LOCK_PATH)
        .map_err(|e| anyhow::anyhow!("cannot open lock file: {e}"))?;
    nix::fcntl::Flock::lock(f, nix::fcntl::FlockArg::LockExclusive)
        .map_err(|(_, e)| anyhow::anyhow!("cannot lock: {e}"))
}

/// Выполнить проверенный запрос. Возвращает текст для stdout (обычно пустой).
///
/// # Errors
/// Аргументы не прошли проверку, не удалось взять замок или выполнить команду.
pub fn handle(req: &HelperRequest) -> anyhow::Result<String> {
    // Отказ тоже оставляет след в журнале (SECURITY.md §2).
    if let Err(e) = req.validate() {
        journal::error(&format!("rejected: {}: {e}", req.name()));
        return Err(e.into());
    }
    let args = req.to_args().join(" ");
    let result = lock().and_then(|_guard| dispatch(req));
    match &result {
        Ok(_) => journal::info(&format!("ok: {args}")),
        Err(e) => journal::error(&format!("failed: {args}: {e}")),
    }
    result
}

fn dispatch(req: &HelperRequest) -> anyhow::Result<String> {
    use HelperRequest as R;
    match req {
        R::FwApply {
            ap,
            uplink,
            approval,
            guests_reach_pc,
            allow,
            block,
        } => {
            sys::check_ap(ap)?;
            sys::check_uplink(uplink, ap)?;
            nft::apply(&nft::Rules {
                ap,
                uplink,
                approval: *approval,
                guests_reach_pc: *guests_reach_pc,
                allow,
                block,
            })?;
            // Шаг ufw не прошёл — снимаем правила ufw (они переживают перезагрузку и могли
            // встать наполовину). Таблицу nft оставляем: она ставится атомарно и это основная
            // защита, а `fw-apply` повторяют и на работающей раздаче (SECURITY.md §2.1).
            let ufw_res = ufw::is_active().and_then(|active| {
                if active {
                    ufw::apply(ap, uplink)
                } else {
                    Ok(())
                }
            });
            if let Err(e) = ufw_res {
                let _ = ufw::clear();
                return Err(e);
            }
        }
        R::FwClear => {
            // Оба шага выполняются независимо; возвращается первая ошибка.
            let nft_res = nft::clear();
            let ufw_res = ufw::clear();
            nft_res.and(ufw_res)?;
        }
        R::FwBlock { mac } => nft::set_add(nft::SET_BLOCKED, mac)?,
        R::FwUnblock { mac } => nft::set_remove(nft::SET_BLOCKED, mac)?,
        R::FwAllowAdd { mac } => nft::set_add(nft::SET_ALLOWED, mac)?,
        R::FwAllowRemove { mac } => nft::set_remove(nft::SET_ALLOWED, mac)?,
        R::StationKick { ap, mac } => {
            sys::check_ap(ap)?;
            wifi::station_kick(ap, mac)?;
        }
        R::RegdomSet { country } => wifi::regdom_set(country)?,
        R::DnsSet { servers } => dns::set(servers)?,
        R::DnsClear => dns::clear()?,
        R::LeasesRead { ap } => {
            // Только Wi-Fi: аренды чужих shared-подключений (по кабелю) не читаем.
            sys::check_wireless(ap)?;
            return wifi::leases_read(ap);
        }
    }
    Ok(String::new())
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // Неверные аргументы — в журнал, но без их текста (его прислал вызывающий).
            // `--help` и `--version` — не отказ. Не root пишет в журнал не от root — не пишем вовсе.
            if e.use_stderr() && nix::unistd::geteuid().is_root() {
                journal::error("rejected arguments");
            }
            e.exit();
        }
    };
    if !nix::unistd::geteuid().is_root() {
        journal::error("must run as root (via pkexec)");
        return ExitCode::from(2);
    }
    match cli.cmd.into_request() {
        None => match serve::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                journal::error(&format!("serve: {e}"));
                ExitCode::from(1)
            }
        },
        Some(req) => match handle(&req) {
            Ok(out) => {
                use std::io::Write;
                if !out.is_empty() && std::io::stdout().write_all(out.as_bytes()).is_err() {
                    return ExitCode::from(1);
                }
                ExitCode::SUCCESS
            }
            Err(_) => ExitCode::from(1),
        },
    }
}
