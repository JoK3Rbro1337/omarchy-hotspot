mod cli;
mod daemon;
mod hotspot_ctx;
mod tui;

use std::io::IsTerminal;

use clap::{Parser, Subcommand, ValueEnum};
use omarchy_hotspot_core::backend::{Band, Security};
use omarchy_hotspot_core::{CoreError, Lang, Msg, t};

use cli::Ctx;
use cli::hotspot::SetWhat;

/// Раздача Wi-Fi с ПК для Omarchy.
#[derive(Parser)]
#[command(name = "omarchy-hotspot", version)]
struct Cli {
    /// Язык сообщений: ru, uk, en (по умолчанию из $LANG)
    #[arg(long, global = true)]
    lang: Option<String>,
    /// Показывать технические подробности ошибок
    #[arg(short, long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Включить раздачу
    On,
    /// Выключить раздачу
    Off,
    /// Включить, если выключена, и выключить, если включена
    Toggle,
    /// Состояние раздачи (код выхода 0 — включена)
    Status {
        /// Вывод в JSON (для панели и скриптов)
        #[arg(long, conflicts_with_all = ["quiet", "waybar"])]
        json: bool,
        /// Вывод для значка на панели Omarchy (JSON в стиле Waybar)
        #[arg(long, conflicts_with = "quiet")]
        waybar: bool,
        /// Ничего не печатать, только код выхода
        #[arg(short, long)]
        quiet: bool,
    },
    /// Показать пароль сети
    Password,
    /// Показать QR-код для подключения телефона
    Qr,
    /// Изменить настройку
    Set {
        #[command(subcommand)]
        what: SetCmd,
    },
    /// Выключить раздачу и удалить всё, что она создала
    Reset {
        /// Не спрашивать подтверждение
        #[arg(short, long)]
        yes: bool,
    },
    /// Проверить, готова ли система к раздаче
    Doctor,
    /// Открыть окно (то же самое, что запуск без подкоманды)
    Tui,
    /// Открыть окно в Omarchy сразу нужного размера (для значка на панели и меню)
    Open,
    /// Список подключённых устройств
    Devices,
    /// Доступ устройств: одобрение новых и чёрный список
    Access {
        #[command(subcommand)]
        what: AccessCmd,
    },
    /// Фоновая служба (её запускает systemd; вручную — для проверки)
    Daemon,
}

#[derive(Subcommand)]
enum AccessCmd {
    /// Показать одобренные и заблокированные устройства
    List,
    /// Разрешить устройству выход в интернет
    Allow { mac: String },
    /// Заблокировать устройство и отключить его от сети
    Block { mac: String },
    /// Убрать устройство из чёрного списка
    Unblock { mac: String },
    /// Спрашивать ли про новые устройства: on или off
    Approval { value: OnOff },
}

#[derive(Clone, Copy, ValueEnum)]
enum OnOff {
    On,
    Off,
}

#[derive(Subcommand)]
enum SetCmd {
    /// Имя сети (1–32 байта)
    Ssid { name: String },
    /// Новый пароль: спросить (скрытый ввод), --generate или --stdin
    Password {
        /// Создать надёжный случайный пароль
        #[arg(long, conflicts_with = "stdin")]
        generate: bool,
        /// Прочитать пароль из стандартного ввода (одна строка)
        #[arg(long)]
        stdin: bool,
    },
    /// Диапазон: auto, 2.4 или 5
    Band { band: BandArg },
    /// Защита: wpa3 или wpa2 (смешанный WPA2/WPA3 для старых устройств)
    Security { security: SecurityArg },
    /// Выключать раздачу, если никто не подключён N минут (0 — не выключать)
    #[command(name = "idle-off")]
    IdleOff { minutes: u32 },
}

#[derive(Clone, Copy, ValueEnum)]
enum BandArg {
    Auto,
    #[value(name = "2.4")]
    Ghz2_4,
    #[value(name = "5")]
    Ghz5,
}

#[derive(Clone, Copy, ValueEnum)]
enum SecurityArg {
    Wpa3,
    Wpa2,
}

fn main() {
    let cli = Cli::parse();
    let level = if cli.verbose { "debug" } else { "warn" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("OMARCHY_HOTSPOT_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level)),
        )
        .with_writer(std::io::stderr)
        .without_time()
        .init();

    let lang = cli
        .lang
        .as_deref()
        .and_then(Lang::parse)
        .unwrap_or_else(Lang::from_env);
    let ctx = Ctx {
        lang,
        verbose: cli.verbose,
    };

    // «Вкл/выкл» с панели или из меню: терминала нет, ошибку покажем уведомлением.
    let notify_errors = matches!(cli.cmd, Some(Cmd::Toggle)) && !std::io::stderr().is_terminal();
    let result = match cli.cmd {
        Some(Cmd::On) => cli::hotspot::on(&ctx).map(|_| 0),
        Some(Cmd::Off) => cli::hotspot::off(&ctx).map(|_| 0),
        Some(Cmd::Toggle) => cli::hotspot::toggle(&ctx).map(|_| 0),
        Some(Cmd::Status { waybar: true, .. }) => cli::hotspot::status_waybar(&ctx).map(|_| 0),
        Some(Cmd::Status { json, quiet, .. }) => cli::hotspot::status(&ctx, json, quiet),
        Some(Cmd::Password) => cli::hotspot::password(&ctx).map(|_| 0),
        Some(Cmd::Qr) => cli::hotspot::qr(&ctx).map(|_| 0),
        Some(Cmd::Set { what }) => cli::hotspot::set(&ctx, set_what(what)).map(|_| 0),
        Some(Cmd::Reset { yes }) => cli::hotspot::reset(&ctx, yes).map(|_| 0),
        Some(Cmd::Doctor) => cli::doctor::run(lang).map(|_| 0),
        Some(Cmd::Devices) => cli::access::devices(&ctx).map(|_| 0),
        Some(Cmd::Access { what }) => cli::access::access(&ctx, access_what(what)).map(|_| 0),
        Some(Cmd::Daemon) => daemon::run(&ctx).map(|_| 0),
        Some(Cmd::Tui) | None => tui::run(&ctx).map(|_| 0),
        Some(Cmd::Open) => tui::open_in_omarchy().map(|_| 0),
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            report(&ctx, &e);
            if notify_errors {
                daemon::notify::send_now(t(lang, Msg::LabelHotspot), &user_text(&ctx, &e));
            }
            std::process::exit(1);
        }
    }
}

fn access_what(cmd: AccessCmd) -> cli::access::AccessWhat {
    use cli::access::AccessWhat as W;
    match cmd {
        AccessCmd::List => W::List,
        AccessCmd::Allow { mac } => W::Allow(mac),
        AccessCmd::Block { mac } => W::Block(mac),
        AccessCmd::Unblock { mac } => W::Unblock(mac),
        AccessCmd::Approval { value } => W::Approval(matches!(value, OnOff::On)),
    }
}

fn set_what(cmd: SetCmd) -> SetWhat {
    match cmd {
        SetCmd::Ssid { name } => SetWhat::Ssid(name),
        SetCmd::IdleOff { minutes } => SetWhat::IdleOff(minutes),
        SetCmd::Password { generate, stdin } => SetWhat::Password { generate, stdin },
        SetCmd::Band { band } => SetWhat::Band(match band {
            BandArg::Auto => Band::Auto,
            BandArg::Ghz2_4 => Band::Ghz2_4,
            BandArg::Ghz5 => Band::Ghz5,
        }),
        SetCmd::Security { security } => SetWhat::Security(match security {
            SecurityArg::Wpa3 => Security::Wpa3,
            SecurityArg::Wpa2 => Security::Wpa2,
        }),
    }
}

/// Короткий текст ошибки без технических подробностей.
fn user_text(ctx: &Ctx, e: &anyhow::Error) -> String {
    match e.downcast_ref::<CoreError>() {
        Some(core) => core.user_message(ctx.lang).to_string(),
        None => e.to_string(),
    }
}

/// Ошибка для человека: понятный текст, а с --verbose — технические подробности.
fn report(ctx: &Ctx, e: &anyhow::Error) {
    match e.downcast_ref::<CoreError>() {
        Some(core) => {
            eprintln!("✗ {}", core.user_message(ctx.lang));
            // Текст ошибки NM при активации полезен всегда (пароля в нём нет).
            if ctx.verbose || matches!(core, CoreError::ActivationFailed(_)) {
                eprintln!("  {}: {core}", t(ctx.lang, Msg::ErrDetails));
            }
        }
        None => {
            eprintln!("✗ {e}");
            if ctx.verbose {
                eprintln!("  {e:?}");
            }
        }
    }
}
