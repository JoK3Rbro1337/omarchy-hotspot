//! Точка входа отрисовки: проверка размера окна → рамка → содержимое режима → оверлей.
//! Размер окна один на оба режима (больший из двух, с запасами) и меняется, только когда
//! открывают QR, которому не хватает места (`desired_size`); окно Omarchy подгоняется под него (`fit`).

use ratatui::Frame;

use super::mouse::{Hits, Target};
use super::state::{AppState, Mode};
use super::{advanced, simple, widgets};

pub const MIN_WIDTH: u16 = 44;
pub const MIN_HEIGHT: u16 = 8;
/// Внешняя рамка окна: по столбцу рамки и отступа с каждой стороны, строка рамки сверху и снизу.
const FRAME_H: u16 = 4;
const FRAME_V: u16 = 2;

/// Сколько столбцов и строк нужно окну. Не зависит от режима и от того, включена ли раздача:
/// больший из режимов с запасами; открытый QR добавляет место (и в продвинутом режиме тоже —
/// чтобы переключение вкладок не меняло размер).
pub fn desired_size(app: &AppState) -> (u16, u16) {
    size(app, true)
}

/// Обычный размер окна: без открытого QR и всплывающих окон. Его окно запоминает в пикселях,
/// чтобы следующее открылось сразу им (`fit`, `omarchy-hotspot open`).
pub fn base_size(app: &AppState) -> (u16, u16) {
    size(app, false)
}

fn size(app: &AppState, extras: bool) -> (u16, u16) {
    let content_w = advanced::CONTENT_WIDTH.max(simple::LEFT_WIDTH);
    let mut w = content_w;
    let mut h = simple::base_height(app, content_w).max(advanced::envelope_height(app, content_w));
    if extras && let Some((qw, qh)) = simple::qr_size(app, content_w) {
        w = w.max(qw);
        h = h.max(qh);
    }
    let (mut w, mut h) = (w + FRAME_H, h + FRAME_V);
    let overlays = [widgets::overlay_size(app), advanced::input_size(app)];
    for (ow, oh) in overlays.into_iter().flatten().filter(|_| extras) {
        w = w.max(ow);
        h = h.max(oh);
    }
    (w.max(MIN_WIDTH), h.max(MIN_HEIGHT))
}

pub fn draw(f: &mut Frame, app: &AppState, hits: &mut Hits) {
    hits.clear();
    let full = f.area();
    if full.width < MIN_WIDTH || full.height < MIN_HEIGHT {
        widgets::draw_too_small(f, full, app.lang);
        return;
    }
    // Окно больше нужного (подгонка не сработала или её нет) — рамка по центру, без пустоты вокруг.
    let (w, h) = desired_size(app);
    let area = widgets::centered_rect(full, w, h);
    let inner = widgets::draw_frame(f, area, app, hits);
    match app.mode {
        Mode::Simple => simple::draw(f, inner, app, hits),
        Mode::Advanced => advanced::draw(f, inner, app, hits),
    }
    if app.mode == Mode::Simple && simple::qr_place(app, inner) == simple::QrPlace::Overlay {
        widgets::draw_qr_overlay(f, area, app);
        // Клик в любом месте прячет QR.
        hits.click(area, Target::Qr);
    } else if app.editing.is_some() {
        widgets::draw_editor(f, area, app);
    } else if app.confirm.is_some() {
        widgets::draw_confirm(f, area, app);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::SystemTime;

    use omarchy_hotspot_core::Lang;
    use omarchy_hotspot_core::backend::HotspotState;
    use omarchy_hotspot_core::config::Config;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::super::state::{Confirm, StatusView};
    use super::*;
    use omarchy_hotspot_core::t;

    fn app() -> AppState {
        AppState::new(
            Lang::Ru,
            Config::default(),
            PathBuf::from("/tmp/oh-tui-test.toml"),
        )
    }

    /// Текст буфера терминала построчно — удобно для `contains` в тестах.
    fn render(app: &AppState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw(f, app, &mut Hits::default()))
            .unwrap();
        let buf = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf.cell((x, y)).unwrap().symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn too_small_window_shows_message() {
        let text = render(&app(), 40, 8);
        assert!(text.contains("Окно слишком маленькое"));
    }

    #[test]
    fn off_state_shows_turn_on_hint() {
        let text = render(&app(), 100, 30);
        assert!(text.contains("Раздача выключена"));
        assert!(text.contains("Нажми Space"));
        assert!(!text.contains("Подключить")); // QR скрыт, когда раздача выключена
    }

    #[test]
    fn frame_hugs_content_and_is_centered_on_a_huge_screen() {
        // На весь экран рамку не растягиваем: она по размеру содержимого и стоит по центру.
        let backend = TestBackend::new(200, 60);
        let mut terminal = Terminal::new(backend).unwrap();
        let a = app();
        terminal
            .draw(|f| draw(f, &a, &mut Hits::default()))
            .unwrap();
        let buf = terminal.backend().buffer();
        let (w, h) = desired_size(&a);
        let (x, y) = ((200 - w) / 2, (60 - h) / 2);
        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), " ");
        assert_eq!(buf.cell((x, y)).unwrap().symbol(), "╭");
        assert_eq!(buf.cell((x + w - 1, y + h - 1)).unwrap().symbol(), "╯");
    }

    #[test]
    fn password_row_visible_even_when_hotspot_off() {
        // Раньше строка "Пароль" рисовалась только при включённой раздаче — из-за этого
        // казалось, что `p` не работает, если её нажали при выключенной раздаче.
        let mut a = app();
        let text = render(&a, 100, 30);
        assert!(text.contains("Пароль"));

        // Пока не известно, есть ли профиль вообще, — точки (не выдаём длину пароля).
        assert!(text.contains("••••••••••"));

        // Если уже точно знаем, что профиля нет (никогда не включали раздачу) — тире.
        a.password_absent = true;
        let text = render(&a, 100, 30);
        assert!(text.contains("Пароль"));
        assert!(text.contains('—'));
    }

    #[test]
    fn on_state_shows_ssid_and_badges() {
        let mut a = app();
        a.cfg.hotspot.ssid = "Omarchy-PC".into();
        a.status = StatusView {
            state: HotspotState::On {
                since: Some(SystemTime::now()),
                ap_iface: "wlp15s0".into(),
                uplink: Some("enp14s0".into()),
            },
            channel: Some(36),
        };
        let text = render(&a, 100, 30);
        assert!(text.contains("Раздача включена"));
        assert!(text.contains("Omarchy-PC"));
        assert!(text.contains("WPA3"));
        assert!(text.contains("изоляция"));
    }

    /// Включённая раздача с интерфейсом `wlp15s0`.
    fn app_on() -> AppState {
        let mut a = app();
        a.status = StatusView {
            state: HotspotState::On {
                since: Some(SystemTime::now()),
                ap_iface: "wlp15s0".into(),
                uplink: Some("enp14s0".into()),
            },
            channel: Some(36),
        };
        a
    }

    fn device(name: &str, dbm: i32) -> omarchy_hotspot_core::devices::Device {
        use omarchy_hotspot_core::devices::{Device, DeviceStatus};
        Device {
            mac: omarchy_hotspot_core::helper_proto::Mac::parse("3c:2e:f5:11:22:33").unwrap(),
            ip: Some(std::net::Ipv4Addr::new(10, 42, 0, 34)),
            hostname: Some(name.into()),
            signal_dbm: Some(dbm),
            rx_bytes: 1000,
            tx_bytes: 2000,
            connected_secs: 30,
            status: DeviceStatus::Allowed,
        }
    }

    #[test]
    fn device_row_shows_name_signal_and_ip() {
        let mut a = app_on();
        a.devices = vec![device("Pixel-8", -50)];
        let text = render(&a, 100, 30);
        assert!(text.contains("Устройства · 1"));
        assert!(text.contains("Pixel-8"));
        assert!(text.contains("10.42.0.34"));
        assert!(text.contains("▂▄▆█"), "нет полосок сигнала:\n{text}");
    }

    #[test]
    fn empty_device_list_says_so() {
        let text = render(&app_on(), 100, 30);
        assert!(text.contains("Устройства · 0"));
        assert!(text.contains("Пока никто не подключился"));
    }

    #[test]
    fn traffic_block_shows_rates_and_session_total() {
        let text = render(&app_on(), 100, 30);
        assert!(text.contains("Трафик"));
        assert!(text.contains("0 Б/с"));
        assert!(text.contains("за сеанс"));
    }

    #[test]
    fn confirm_asks_before_taking_the_adapter() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut a = app();
        a.wifi_client = Some("Home-WiFi".into());
        a.request_toggle(&tx);
        assert_eq!(
            a.confirm,
            Some(Confirm::StartOverClient("Home-WiFi".into())),
            "не спросили перед тем, как занять адаптер"
        );
        let text = render(&a, 100, 30);
        assert!(text.contains("Home-WiFi"));
        assert!(text.contains("Внимание"));
        assert!(text.contains("Esc"));
        // Отказ ничего не запускает.
        a.confirm_cancel();
        assert!(a.confirm.is_none());
        assert!(a.busy.is_none());
    }

    #[test]
    fn no_question_when_adapter_is_free() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut a = app();
        // Без рантайма tokio фоновая задача не запустится, поэтому проверяем только вопрос.
        assert!(a.wifi_client.is_none());
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.request_toggle(&tx)));
        assert!(a.confirm.is_none());
    }

    #[test]
    fn qr_is_hidden_until_asked_and_then_whole() {
        let mut a = app_on();
        let lines: Vec<String> = (0..17).map(|i| format!("QRLINE{i:02}")).collect();
        a.qr = Some(lines);
        let (w, h) = desired_size(&a);
        assert!(
            !render(&a, w, h).contains("QRLINE00"),
            "QR показан без запроса"
        );
        a.show_qr = true;
        let (w, h) = desired_size(&a);
        let text = render(&a, w, h);
        assert!(text.contains("QRLINE00"), "верх QR обрезан:\n{text}");
        assert!(text.contains("QRLINE16"), "низ QR обрезан:\n{text}");
        assert!(text.contains("наведи камеру"));
    }

    #[test]
    fn new_device_question_is_shown() {
        let mut a = app_on();
        a.confirm = Some(Confirm::NewDevice {
            mac: omarchy_hotspot_core::helper_proto::Mac::parse("3c:2e:f5:11:22:33").unwrap(),
            name: "Galaxy-Tab".into(),
        });
        let text = render(&a, 100, 30);
        assert!(text.contains("Galaxy-Tab"));
        assert!(text.contains("Новое устройство"));
        assert!(text.contains("Enter"), "нет подсказки про ответ:\n{text}");
    }

    #[test]
    fn approval_badge_and_warning() {
        let mut a = app_on();
        a.daemon_approval = true;
        let text = render(&a, 100, 30);
        assert!(text.contains("одобрение"));
        // Одобрение включено в настройках, но службы нет — предупреждаем.
        a.daemon_approval = false;
        a.cfg.access.approval_required = true;
        let text = render(&a, 100, 30);
        assert!(text.contains("служба одобрения не запущена"));
    }

    #[test]
    fn editor_warns_that_devices_will_ask_again() {
        let mut a = app();
        a.cfg.access.approval_required = true;
        a.start_edit();
        let text = render(&a, 100, 30);
        assert!(
            text.contains("снова попросят одобрения"),
            "нет подсказки:\n{text}"
        );
    }

    #[test]
    fn editor_overlay_shows_current_ssid() {
        let mut a = app();
        a.cfg.hotspot.ssid = "Test-Net".into();
        a.start_edit();
        let text = render(&a, 100, 30);
        assert!(text.contains("Test-Net"));
    }

    fn app_advanced() -> AppState {
        let mut a = app_on();
        a.mode = Mode::Advanced;
        a
    }

    #[test]
    fn advanced_shows_sections_items_and_hint() {
        use super::super::settings::Focus;
        let mut a = app_advanced();
        let text = render(&a, 100, 30);
        for s in [
            "Разделы",
            "Радио",
            "Безопасность",
            "Эксперт",
            "Диапазон",
            "Ширина канала",
        ] {
            assert!(text.contains(s), "нет «{s}»:\n{text}");
        }
        assert!(text.contains("Раздача включена"));
        // Подсказка к выбранному параметру.
        a.adv.focus = Focus::Items;
        a.adv.item_idx = 3;
        let text = render(&a, 100, 30);
        assert!(
            text.contains("Сеть не видна в списке"),
            "нет подсказки:\n{text}"
        );
    }

    #[test]
    fn unsaved_changes_are_marked() {
        use super::super::settings::Section;
        let mut a = app_advanced();
        a.adv.section = Section::Automation;
        a.adv.draft.automation.timer_minutes = 30;
        let text = render(&a, 100, 30);
        assert!(text.contains("Автоматизация *"));
        assert!(text.contains("30 мин *"));
        assert!(text.contains("несохранённые изменения"));
    }

    #[test]
    fn devices_section_lists_block_list_offline() {
        use super::super::settings::{Focus, Section};
        let mut a = app_advanced();
        a.devices = vec![device("Pixel-8", -50)];
        a.cfg.access.blocked_macs = vec!["aa:bb:cc:dd:ee:01".into()];
        a.adv.section = Section::Devices;
        a.adv.focus = Focus::Items;
        let text = render(&a, 100, 30);
        assert!(text.contains("Pixel-8"));
        assert!(
            text.contains("-50 dBm"),
            "нет сведений об устройстве:\n{text}"
        );
        assert!(text.contains("aa:bb:cc:dd:ee:01"));
        assert!(text.contains("не в сети"));
        assert!(
            text.contains("списке: 1"),
            "нет числа в чёрном списке:\n{text}"
        );
        assert!(text.contains("b отобрать"));
    }

    #[test]
    fn expert_doctor_output_and_input_overlay() {
        use super::super::settings::{ExpertOutput, Focus, Item, Section, TextInput};
        use omarchy_hotspot_core::doctor::{Check, CheckId, Status};
        let mut a = app_advanced();
        a.adv.section = Section::Expert;
        a.adv.focus = Focus::Items;
        a.adv.output = ExpertOutput::Doctor(vec![Check {
            id: CheckId::NetworkManager,
            status: Status::Ok,
            detail: "работает".into(),
            advice: None,
        }]);
        let text = render(&a, 100, 30);
        assert!(text.contains("✓ NetworkManager"), "нет результата:\n{text}");
        assert!(text.contains("Всё готово"));

        a.adv.input = Some(TextInput {
            item: Item::Subnet,
            value: "192.168.".into(),
            error: Some("плохой адрес".into()),
        });
        let text = render(&a, 100, 30);
        assert!(text.contains("192.168."));
        assert!(text.contains("плохой адрес"));
    }

    #[test]
    fn advanced_survives_tiny_window() {
        use super::super::settings::Section;
        let mut a = app_advanced();
        for s in Section::ALL {
            a.adv.section = s;
            let _ = render(&a, MIN_WIDTH, MIN_HEIGHT);
        }
    }

    fn fake_qr(lines: usize, width: usize) -> Vec<String> {
        (0..lines)
            .map(|i| format!("Q{i:02}{}", "#".repeat(width - 3)))
            .collect()
    }

    #[test]
    fn window_size_does_not_jump() {
        use super::super::settings::{Focus, Section};
        let base = desired_size(&app());
        let mut a = app_on();
        assert_eq!(desired_size(&a), base, "включение раздачи меняет размер");
        // Обычная работа: служба одобрения запущена (без неё — ещё строка предупреждения,
        // вместе с сообщением окно подрастёт на строку; так и задумано, это редкость).
        a.daemon_approval = true;
        assert_eq!(desired_size(&a), base, "служба одобрения меняет размер");
        a.devices = vec![device("Pixel-8", -50)];
        a.notice = Some("Устройство заблокировано".into());
        assert_eq!(
            desired_size(&a),
            base,
            "сообщение или устройство меняют размер"
        );
        a.mode = Mode::Advanced;
        for s in Section::ALL {
            for focus in [Focus::Sections, Focus::Items] {
                a.adv.section = s;
                a.adv.focus = focus;
                assert_eq!(desired_size(&a), base, "раздел {s:?} меняет размер");
            }
        }
        // Компактный QR помещается в тот же размер — открытие его ничего не двигает.
        a.mode = Mode::Simple;
        a.qr = Some(fake_qr(16, 31));
        a.show_qr = true;
        assert_eq!(desired_size(&a), base, "QR меняет размер, хотя помещается");
        let text = render(&a, base.0, base.1);
        assert!(
            text.contains(&("Q15".to_string() + &"#".repeat(28))),
            "{text}"
        );
    }

    #[test]
    fn device_reserve_grows_at_once_and_shrinks_slowly() {
        use std::time::{Duration, Instant};
        let mut a = app_on();
        let (_, h0) = desired_size(&a);
        let t0 = Instant::now();
        a.devices = vec![device("A", -50), device("B", -60)];
        a.update_device_reserve(t0);
        assert_eq!(desired_size(&a).1, h0, "два устройства помещаются в запас");
        a.devices.push(device("C", -70));
        a.update_device_reserve(t0);
        let (_, h3) = desired_size(&a);
        assert_eq!(
            h3,
            h0 + 2,
            "третье устройство — запас растёт сразу на две строки"
        );
        a.devices.push(device("D", -70));
        a.update_device_reserve(t0);
        assert_eq!(desired_size(&a).1, h3, "четвёртое — в том же запасе");
        // Ушли — окно не сжимается сразу.
        a.devices.truncate(1);
        a.update_device_reserve(t0);
        a.update_device_reserve(t0 + Duration::from_secs(10));
        assert_eq!(desired_size(&a).1, h3);
        a.update_device_reserve(t0 + Duration::from_secs(40));
        assert_eq!(desired_size(&a).1, h0, "через полминуты окно вернулось");
        // Больше восьми — прокрутка и «и ещё N».
        a.devices = (0..20).map(|i| device(&format!("D{i:02}"), -50)).collect();
        a.update_device_reserve(t0);
        let (w, h) = desired_size(&a);
        let text = render(&a, w, h);
        assert!(text.contains("D06"), "{text}");
        assert!(!text.contains("D09"), "показано больше 8 строк:\n{text}");
        assert!(text.contains("и ещё"), "нет строки «и ещё»:\n{text}");
    }

    #[test]
    fn open_qr_is_never_clipped_at_any_size() {
        let mut a = app_on();
        a.qr = Some(fake_qr(17, 33));
        a.show_qr = true;
        let first = "Q00".to_string() + &"#".repeat(30);
        let last = "Q16".to_string() + &"#".repeat(30);
        for w in (MIN_WIDTH..=110).step_by(3) {
            for h in (MIN_HEIGHT..=40).step_by(2) {
                let text = render(&a, w, h);
                if text.contains("Q00") || text.contains("Q16") {
                    assert!(
                        text.contains(&first) && text.contains(&last),
                        "QR обрезан в окне {w}×{h}:\n{text}"
                    );
                } else {
                    assert!(
                        text.contains("слишком маленькое"),
                        "QR пропал без объяснения в окне {w}×{h}:\n{text}"
                    );
                }
            }
        }
        let (w, h) = desired_size(&a);
        assert!(render(&a, w, h).contains(&last));
    }

    #[test]
    fn show_qr_shows_whole_code_or_asks_for_a_bigger_window() {
        let mut a = app_on();
        a.qr = Some(fake_qr(17, 33));
        a.show_qr = true;
        let last = "Q16".to_string() + &"#".repeat(30);
        assert!(render(&a, 60, 30).contains(&last));
        assert!(render(&a, 50, 14).contains("слишком маленькое для QR"));
    }

    #[test]
    fn long_status_lines_wrap_instead_of_being_cut() {
        let mut a = app();
        a.error = Some(
            "очень длинное сообщение об ошибке которое никак не помещается в одну строку окна"
                .into(),
        );
        let (w, h) = desired_size(&a);
        let text = render(&a, w, h);
        for word in ["очень", "помещается", "окна"] {
            assert!(text.contains(word), "нет «{word}»:\n{text}");
        }
    }

    #[test]
    fn device_row_drops_columns_in_a_narrow_window() {
        let mut a = app_on();
        a.devices = vec![device("Pixel-8", -50)];
        let (w, h) = desired_size(&a);
        let text = render(&a, w, h);
        assert!(
            text.contains("10.42.0.34") && text.contains("↓ 0 Б/с  ↑"),
            "{text}"
        );
        let narrow = render(&a, 54, h);
        assert!(narrow.contains("10.42.0.34"), "{narrow}");
        assert!(
            !narrow.contains("↓ 0 Б/с  ↑"),
            "скорость не влезает, но нарисована:\n{narrow}"
        );
    }

    #[test]
    fn every_advanced_section_fits_its_desired_size() {
        use super::super::settings::{Focus, Section};
        let mut a = app_advanced();
        a.devices = vec![device("Pixel-8", -50)];
        for s in Section::ALL {
            for focus in [Focus::Sections, Focus::Items] {
                a.adv.section = s;
                a.adv.focus = focus;
                let (w, h) = desired_size(&a);
                let text = render(&a, w, h);
                assert!(
                    text.contains("Эксперт"),
                    "разделы обрезаны ({s:?}):\n{text}"
                );
                if let Some(last) = s.items().last() {
                    let label = t(Lang::Ru, last.label());
                    assert!(text.contains(label), "нет «{label}»:\n{text}");
                }
            }
        }
    }
}
