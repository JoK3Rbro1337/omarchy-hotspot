pub mod access;
pub mod doctor;
pub mod hotspot;

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::Context;
use omarchy_hotspot_core::config::{self, Config};
use omarchy_hotspot_core::{Lang, Msg, tf};

/// Общее для всех подкоманд.
pub struct Ctx {
    pub lang: Lang,
    pub verbose: bool,
}

impl Ctx {
    /// Конфиг и путь к нему; неизвестные ключи — предупреждение в stderr.
    pub fn load_config(&self) -> anyhow::Result<(Config, PathBuf)> {
        let path = config::config_path().context("cannot find home directory ($HOME)")?;
        let loaded = config::load(&path)?;
        for key in &loaded.unknown_keys {
            eprintln!("{}", tf(self.lang, Msg::WarnUnknownKey, &[key]));
        }
        Ok((loaded.config, path))
    }

    pub fn stdin_is_tty(&self) -> bool {
        std::io::stdin().is_terminal()
    }
}

/// Обрезать по числу символов (для колонок вывода).
pub fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}
