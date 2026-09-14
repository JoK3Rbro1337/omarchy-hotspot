//! Окно программы (ratatui). Долгие операции — в фоне (`tokio::task::spawn_blocking`),
//! их результат приходит через канал вместе с вводом и тиком таймера: интерфейс не зависает.

mod advanced;
mod advanced_actions;
mod events;
mod fit;
mod mouse;
mod settings;
mod simple;
mod state;
mod ui;
mod widgets;

use std::io;
use std::time::Duration;

use crossterm::event::{
    DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture, KeyCode,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc::UnboundedSender;

use crate::cli::Ctx;

use events::{Event, EventHandler};
use mouse::Hits;
use state::{AppState, EditField, Mode};

const TICK_RATE: Duration = Duration::from_millis(200);

/// `omarchy-hotspot open` (значок на панели, меню): сообщить Hyprland размер и место окна
/// (запомненный размер, а до первой подгонки — стартовый), чтобы оно сразу появилось в углу
/// нужного размера, и открыть окно так же, как остальные TUI Omarchy.
pub fn open_in_omarchy() -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    let remembered = fit::remembered_size();
    let (w, h) = remembered.unwrap_or(fit::DEFAULT_SIZE);
    let result = fit::apply_size_rule(w, h);
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    fit::log_open(&format!(
        "{secs} open size={w}x{h} remembered={} rule={}",
        remembered.is_some(),
        match &result {
            Ok(()) => "ok".to_string(),
            Err(e) => e.clone(),
        }
    ));
    if let Err(e) = result {
        tracing::debug!("window size rule skipped: {e}");
    }
    let err = std::process::Command::new("omarchy-launch-or-focus-tui")
        .arg("omarchy-hotspot")
        .exec();
    Err(anyhow::anyhow!("omarchy-launch-or-focus-tui: {err}"))
}

/// Каталог запомненного размера окна — его убирает `reset`.
pub fn state_dir() -> Option<std::path::PathBuf> {
    fit::state_dir()
}

/// `reset`: снять правило размера окна из памяти Hyprland (если Hyprland запущен).
pub fn forget_window_size() {
    fit::clear_size_rule();
}

pub fn run(ctx: &Ctx) -> anyhow::Result<()> {
    let (cfg, cfg_path) = ctx.load_config()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_time()
        .build()?;
    runtime.block_on(run_async(AppState::new(ctx.lang, cfg, cfg_path)))
}

type Term = Terminal<CrosstermBackend<io::Stdout>>;

async fn run_async(app: AppState) -> anyhow::Result<()> {
    let mut terminal = init_terminal()?;
    let result = event_loop(&mut terminal, app).await;
    // Терминал возвращаем в обычный режим независимо от того, как завершился цикл.
    let restore = restore_terminal(&mut terminal);
    result.and(restore)
}

async fn event_loop(terminal: &mut Term, mut app: AppState) -> anyhow::Result<()> {
    let mut events = EventHandler::new(TICK_RATE);
    app.request_initial_refresh(&events.sender());
    let mut hits = Hits::default();
    let mut fitter = fit::Fitter::new();
    loop {
        terminal.draw(|f| ui::draw(f, &app, &mut hits))?;
        if app.should_quit {
            return Ok(());
        }
        let wanted = ui::desired_size(&app);
        fitter.want(wanted, wanted == ui::base_size(&app));
        match events.next().await {
            Some(Event::Key(key)) => handle_key(&mut app, key, &events.sender()),
            Some(Event::Mouse(m)) => mouse::handle(&mut app, m, &hits, &events.sender()),
            Some(Event::FocusGained) => app.focus_gained_at = Some(std::time::Instant::now()),
            // Закрываемся сразу и только по самому событию (не на тике после операции).
            Some(Event::FocusLost) => app.on_focus_lost(),
            Some(Event::Resize) => {}
            Some(Event::Tick) => {
                if let Ok(size) = crossterm::terminal::size() {
                    fitter.tick(size);
                }
                app.own_window = fitter.own_window();
                app.on_tick(&events.sender());
            }
            Some(Event::Bg(resp)) => app.on_bg(*resp, &events.sender()),
            None => return Ok(()),
        }
    }
}

/// Буква горячей клавиши для нажатой клавиши. Учитывает заглавные (Caps Lock) и
/// кириллическую раскладку: при русской/украинской раскладке клавиша «e» присылает «у»,
/// «p» — «з», «g» — «п», «q» — «й». Без этого горячие клавиши молча не работали.
fn hotkey(c: char) -> char {
    match c {
        'й' | 'Й' => 'q',
        'у' | 'У' => 'e',
        'з' | 'З' => 'p',
        'п' | 'П' => 'g',
        'с' | 'С' => 'c',
        'ы' | 'Ы' | 'і' | 'І' => 's',
        'и' | 'И' => 'b',
        'л' | 'Л' => 'k',
        'ф' | 'Ф' => 'a',
        'щ' | 'Щ' => 'o',
        other => other.to_ascii_lowercase(),
    }
}

fn handle_key(app: &mut AppState, key: crossterm::event::KeyEvent, tx: &UnboundedSender<Event>) {
    let hot = match key.code {
        KeyCode::Char(c) => Some(hotkey(c)),
        _ => None,
    };
    if hot == Some('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return;
    }
    // Вопрос «включить раздачу поверх подключения?» отвечается Enter/Esc: буквы y/n
    // на кириллической раскладке попадают на «н»/«т» и легко нажать не то.
    if app.confirm.is_some() {
        match key.code {
            KeyCode::Enter => app.confirm_accept(tx),
            KeyCode::Esc => app.confirm_cancel(),
            _ => {}
        }
        return;
    }
    if app.editing.is_some() {
        match key.code {
            KeyCode::Esc => app.cancel_edit(),
            KeyCode::Enter => app.edit_submit(tx),
            KeyCode::Tab => app.edit_toggle_field(),
            KeyCode::Backspace => app.edit_backspace(),
            // Имя сети может быть кириллицей, поэтому раскладку подменяем только для «g»
            // на пустом поле пароля: пароль всё равно принимает лишь латиницу.
            KeyCode::Char(_) if hot == Some('g') && editing_password_is_empty(app) => {
                app.edit_generate()
            }
            KeyCode::Char(c) => app.edit_push_char(c),
            _ => {}
        }
        return;
    }
    if app.mode == Mode::Advanced && app.advanced_key(key.code, hot, tx) {
        return;
    }
    match key.code {
        // Открытый QR Esc сначала прячет, выход — следующим Esc.
        KeyCode::Esc if app.mode == Mode::Simple && app.show_qr && app.is_on() => {
            app.show_qr = false
        }
        KeyCode::Esc => app.request_quit(),
        KeyCode::Tab => app.toggle_mode(tx),
        KeyCode::Char(' ') => app.request_toggle(tx),
        KeyCode::Up | KeyCode::Down if app.mode == Mode::Simple => {
            app.scroll_simple_devices(key.code == KeyCode::Down)
        }
        KeyCode::Char(_) => match hot {
            Some('q') => app.request_quit(),
            Some('e') => app.start_edit(),
            Some('p') => app.toggle_password(tx),
            Some('c') if app.mode == Mode::Simple => app.toggle_qr(tx),
            _ => {}
        },
        _ => {}
    }
}
fn editing_password_is_empty(app: &AppState) -> bool {
    app.editing
        .as_ref()
        .is_some_and(|e| e.field == EditField::Password && e.password.is_empty())
}

fn init_terminal() -> anyhow::Result<Term> {
    enable_raw_mode()?;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableFocusChange
    )?;
    install_panic_hook();
    Ok(Terminal::new(CrosstermBackend::new(io::stdout()))?)
}

/// Все шаги выполняются независимо: мышь отпускаем обязательно, иначе терминал продолжит
/// слать её коды в оболочку. Возвращается первая ошибка.
fn restore_terminal(terminal: &mut Term) -> anyhow::Result<()> {
    let raw = disable_raw_mode();
    let mouse = execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        DisableFocusChange
    );
    let screen = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let cursor = terminal.show_cursor();
    raw.and(mouse).and(screen).and(cursor)?;
    Ok(())
}

/// Если TUI паникует, терминал всё равно должен вернуться в обычный режим —
/// иначе пользователь останется с «сломанным» терминалом (нет эха, альтернативный экран).
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Окно живёт в главном потоке; паника фоновой задачи его не закрывает — терминал не трогаем.
        if std::thread::current().name() == Some("main") {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), DisableMouseCapture, DisableFocusChange);
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
        }
        original(info);
    }));
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use omarchy_hotspot_core::Lang;
    use omarchy_hotspot_core::config::Config;

    use super::*;

    fn key(c: char) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn app() -> AppState {
        AppState::new(
            Lang::Ru,
            Config::default(),
            PathBuf::from("/tmp/oh-keys.toml"),
        )
    }

    #[test]
    fn cyrillic_layout_maps_to_latin_hotkeys() {
        assert_eq!(hotkey('у'), 'e');
        assert_eq!(hotkey('з'), 'p');
        assert_eq!(hotkey('й'), 'q');
        assert_eq!(hotkey('п'), 'g');
        assert_eq!(hotkey('E'), 'e');
        assert_eq!(hotkey('x'), 'x');
    }

    #[test]
    fn edit_opens_from_both_layouts() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut a = app();
        handle_key(&mut a, key('e'), &tx);
        assert!(a.editing.is_some(), "«e» не открыла окно изменения");
        a.cancel_edit();
        handle_key(&mut a, key('у'), &tx);
        assert!(
            a.editing.is_some(),
            "«у» (русская раскладка) не открыла окно"
        );
    }

    #[test]
    fn password_toggles_from_both_layouts() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut a = app();
        // Чтобы обработчик не полез в фоновую задачу (в тесте нет рантайма tokio).
        a.password_absent = true;
        handle_key(&mut a, key('p'), &tx);
        assert!(a.show_password);
        handle_key(&mut a, key('з'), &tx);
        assert!(
            !a.show_password,
            "«з» (русская раскладка) не спрятала пароль"
        );
    }

    #[test]
    fn cyrillic_text_still_types_into_ssid() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut a = app();
        a.cfg.hotspot.ssid = String::new();
        a.start_edit();
        for c in "Сеть".chars() {
            handle_key(&mut a, key(c), &tx);
        }
        assert_eq!(a.editing.as_ref().unwrap().ssid, "Сеть");
    }

    fn code(c: KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn advanced_app() -> AppState {
        let mut a = app();
        a.mode = state::Mode::Advanced;
        a
    }

    #[test]
    fn advanced_arrows_edit_the_draft_only() {
        use settings::{Focus, Section};
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut a = advanced_app();
        handle_key(&mut a, code(KeyCode::Down), &tx);
        assert_eq!(a.adv.section, Section::Security);
        handle_key(&mut a, code(KeyCode::Right), &tx);
        assert_eq!(a.adv.focus, Focus::Items);
        // Защита → PMF → Изоляция; Enter переключает.
        handle_key(&mut a, code(KeyCode::Down), &tx);
        handle_key(&mut a, code(KeyCode::Down), &tx);
        handle_key(&mut a, code(KeyCode::Enter), &tx);
        assert!(!a.adv.draft.hotspot.ap_isolation);
        assert!(
            a.cfg.hotspot.ap_isolation,
            "конфиг изменился без сохранения"
        );
        // Esc возвращает к разделам и окно не закрывает.
        handle_key(&mut a, code(KeyCode::Esc), &tx);
        assert_eq!(a.adv.focus, Focus::Sections);
        assert!(!a.should_quit);
    }

    #[test]
    fn quitting_with_unsaved_changes_asks_first() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut a = advanced_app();
        a.adv.draft.automation.idle_off_minutes = 15;
        handle_key(&mut a, key('й'), &tx);
        assert_eq!(a.confirm, Some(state::Confirm::UnsavedQuit));
        assert!(!a.should_quit);
        // Esc в вопросе — выйти без сохранения.
        handle_key(&mut a, code(KeyCode::Esc), &tx);
        assert!(a.should_quit);
    }

    #[test]
    fn save_asks_before_restarting_a_running_hotspot() {
        use omarchy_hotspot_core::backend::{Band, HotspotState};
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut a = advanced_app();
        // Без изменений сохранять нечего.
        handle_key(&mut a, key('ы'), &tx);
        assert!(
            a.notice
                .as_deref()
                .is_some_and(|n| n.contains("сохранять нечего"))
        );
        a.status.state = HotspotState::On {
            since: None,
            ap_iface: "wlp15s0".into(),
            uplink: None,
        };
        a.adv.draft.hotspot.band = Band::Ghz2_4;
        handle_key(&mut a, key('s'), &tx);
        assert_eq!(a.confirm, Some(state::Confirm::SaveRestart));
        // Плохая подсеть не доходит даже до вопроса.
        a.confirm = None;
        a.adv.draft.network.subnet = "8.8.8.1/24".into();
        handle_key(&mut a, key('s'), &tx);
        assert!(a.confirm.is_none());
        assert!(a.error.as_deref().is_some_and(|e| e.contains("8.8.8.1/24")));
    }

    #[test]
    fn text_input_takes_letters_that_are_hotkeys_elsewhere() {
        use settings::Section;
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut a = advanced_app();
        a.adv.section = Section::Radio;
        handle_key(&mut a, code(KeyCode::Enter), &tx);
        for _ in 0..4 {
            handle_key(&mut a, code(KeyCode::Down), &tx); // до «Страна Wi-Fi»
        }
        handle_key(&mut a, code(KeyCode::Enter), &tx);
        assert!(a.adv.input.is_some());
        // «q» и «s» в поле ввода — просто буквы, не выход и не сохранение.
        for c in ['q', 's'] {
            handle_key(&mut a, key(c), &tx);
        }
        assert!(!a.should_quit);
        handle_key(&mut a, code(KeyCode::Enter), &tx);
        assert!(a.adv.input.as_ref().is_some_and(|i| i.error.is_some()));
        for _ in 0..2 {
            handle_key(&mut a, code(KeyCode::Backspace), &tx);
        }
        for c in "pl".chars() {
            handle_key(&mut a, key(c), &tx);
        }
        handle_key(&mut a, code(KeyCode::Enter), &tx);
        assert!(a.adv.input.is_none());
        assert_eq!(a.adv.draft.hotspot.country, "PL");
    }

    #[test]
    fn approval_toggles_at_once_in_devices() {
        use settings::{Focus, Section};
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut a = advanced_app();
        let dir = std::env::temp_dir().join(format!("oh-adv-o-{}", std::process::id()));
        a.cfg_path = dir.join("config.toml");
        a.adv.section = Section::Devices;
        a.adv.focus = Focus::Items;
        handle_key(&mut a, key('щ'), &tx);
        assert!(!a.cfg.access.approval_required);
        let saved = omarchy_hotspot_core::config::load(&a.cfg_path)
            .unwrap()
            .config;
        assert!(!saved.access.approval_required, "в файл не записалось");
        // Черновик догнал конфиг — несохранённых изменений нет.
        assert!(!settings::any_dirty(&a.adv.draft, &a.cfg));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
