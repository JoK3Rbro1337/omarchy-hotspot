#!/usr/bin/env bash
# Установщик omarchy-hotspot. Повторный запуск безопасен: обновляет файлы и не дублирует вставки.
# Два режима:
#   ./install.sh            — из исходников: сборка, помощник root (sudo), программа в ~/.local/bin
#                              и настройка Omarchy для текущего пользователя;
#   omarchy-hotspot-setup   — после пакета AUR (этот же скрипт в /usr/share/omarchy-hotspot):
#                              только настройка Omarchy, всё системное уже поставил pacman.
# Требования к установке — docs/SECURITY.md §8.
set -euo pipefail

cd "$(dirname "$(readlink -f "$0")")"

APP=omarchy-hotspot
PACKAGED=0
[[ $PWD == /usr/share/omarchy-hotspot ]] && PACKAGED=1
BIN_DIR="$HOME/.local/bin"
HELPER_DIR=/usr/lib/omarchy-hotspot
HELPER="$HELPER_DIR/omarchy-hotspot-helper"
POLICY=/usr/share/polkit-1/actions/org.omarchy.hotspot.policy
CLEANUP_UNIT=/etc/systemd/system/omarchy-hotspot-cleanup.service
USER_UNIT="$HOME/.config/systemd/user/omarchy-hotspot.service"
HYPR_CONF="$HOME/.config/hypr/hyprland.lua"
MENU_CONF="$HOME/.config/omarchy/extensions/omarchy-menu.jsonc"
SHELL_JSON="$HOME/.config/omarchy/shell.json"
PLUGINS="$HOME/.config/omarchy/plugins"
PLUGIN_ID=io.github.jok3rbro1337.omarchy-hotspot
# До публикации значок назывался так — переносим на новый id.
OLD_PLUGIN_ID=omarchy-hotspot
PLUGIN_DIR="$PLUGINS/$PLUGIN_ID"
STAMP="$(date +%Y%m%d-%H%M%S)"

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31m✗\033[0m %s\n' "$*" >&2; exit 1; }
ask() {
  local answer
  read -rp "$1 [y/N] " answer || return 1
  [[ ${answer,,} =~ ^(y|yes|д|да|т|так)$ ]]
}

# Пакет, которому принадлежат системные файлы omarchy-hotspot; код 1 — установлено не пакетом.
package_owner() {
  local f
  for f in "$HELPER" "$HELPER_DIR" "$POLICY"; do
    [[ -e $f ]] && pacman -Qqo -- "$f" 2>/dev/null | head -n 1 && return 0
  done
  pacman -Qq omarchy-hotspot 2>/dev/null && return 0
  return 1
}

# Каталог значка — наш (по манифесту), а не чужой плагин с тем же id.
own_plugin() {
  local dir=$1 id=$2
  [[ -d $dir && ! -L $dir && -f $dir/manifest.json ]] &&
    jq -e --arg id "$id" '.id == $id and .author == "JoK3Rbro1337"' "$dir/manifest.json" >/dev/null 2>&1
}
# Временный файл правки конфигов убирается при любом выходе.
tmp=""
trap 'rm -f -- "${tmp:-}"' EXIT
# Номер строки-маркера без учёта пробелов по краям (так же ищет uninstall.sh).
marker_line() {
  grep -nxF -- "$2" <(sed 's/^[[:space:]]*//; s/[[:space:]]*$//' "$1") | cut -d: -f1 || true
}
# Копия файла перед правкой: <файл>.bak-<дата>.
backup() {
  [[ -f $1 ]] || return 0
  cp -p -- "$1" "$1.bak-$STAMP" || die "Не удалось сделать копию $1 — файл не трогаю."
  say "Копия: $1.bak-$STAMP"
}

[[ $EUID -ne 0 ]] || die "Запускай без sudo: root-команды установщик попросит сам."
[[ -t 0 ]] || die "Нужен терминал: установщик задаёт вопросы."

if ((PACKAGED)); then
  # ---- После пакета AUR: программа, помощник, polkit и службы уже на месте ----
  [[ -x /usr/bin/$APP && -x $HELPER ]] || die "Пакет omarchy-hotspot установлен не полностью: переустанови его."
  # Копия из исходников в ~/.local/bin стоит в PATH раньше /usr/bin и заслонила бы пакет.
  if [[ -e $BIN_DIR/$APP || -f $USER_UNIT ]]; then
    echo "Найдена прошлая установка из исходников ($BIN_DIR/$APP, $USER_UNIT)."
    echo "Она заслоняет пакет: команда и служба запускали бы старую версию."
    if ask "Убрать её (программу и файл службы; настройки и сеть не трогаются)?"; then
      systemctl --user disable --now omarchy-hotspot.service >/dev/null 2>&1 || true
      rm -f -- "$BIN_DIR/$APP" "$USER_UNIT"
      say "Старая установка из исходников убрана"
    else
      warn "Оставлена: пока она есть, запускается старая версия."
    fi
  fi
  systemctl --user daemon-reload || warn "Не удалось перечитать службы пользователя (нет сеанса?)."
  systemctl --user try-restart omarchy-hotspot.service >/dev/null 2>&1 || true
else
  # Помощник уже поставлен пакетом — sudo install перезаписал бы файлы, которыми владеет pacman.
  if owner="$(package_owner)"; then
    die "omarchy-hotspot уже установлен пакетом ($owner). Настройка Omarchy: omarchy-hotspot-setup. Чтобы ставить из исходников, сначала: omarchy-hotspot-remove и sudo pacman -Rns $owner"
  fi
  # 1. Зависимости
  say "Проверяю пакеты"
  need=()
  for pkg in iw networkmanager dnsmasq nftables polkit libnotify jq; do
    pacman -Qq "$pkg" >/dev/null 2>&1 || need+=("$pkg")
  done
  command -v cargo >/dev/null 2>&1 || need+=(rust)
  if ((${#need[@]})); then
    echo "Не хватает пакетов: ${need[*]}"
    echo "Команда установит их из репозиториев Arch: sudo pacman -S --needed ${need[*]}"
    ask "Установить?" || die "Без этих пакетов раздача работать не будет."
    sudo pacman -S --needed -- "${need[@]}"
  fi

  # 2. Сборка
  say "Собираю программу (первый раз это несколько минут)"
  cargo build --release --locked --quiet
  [[ -x target/release/omarchy-hotspot && -x target/release/omarchy-hotspot-helper ]] || die "Сборка не удалась."

  # 3. Системная часть (нужен пароль sudo)
  cat <<MSG

Сейчас будут выполнены команды с правами администратора (sudo):
  install -d -o root -g root -m 0755 $HELPER_DIR
  install -o root -g root -m 0755 target/release/omarchy-hotspot-helper $HELPER
      — помощник защиты: единственная часть программы с правами root
  install -o root -g root -m 0644 packaging/org.omarchy.hotspot.policy $POLICY
      — разрешение polkit запускать помощника без пароля в активном сеансе
  install -o root -g root -m 0644 packaging/omarchy-hotspot-cleanup.service $CLEANUP_UNIT
  systemctl daemon-reload
  systemctl enable omarchy-hotspot-cleanup.service
      — при загрузке ПК убирает правила брандмауэра, если раздача не выключилась штатно

MSG
  ask "Продолжить?" || die "Установка отменена. Ничего системного не изменено."

  # Помощник и все каталоги над ним должны принадлежать root и не быть записываемыми
  # для других: иначе подменённый файл получил бы root через polkit.
  check_root_owned() {
    local path owner mode
    for path in "$@"; do
      owner="$(stat -c '%u' -- "$path")" || die "Не удалось проверить $path — установка остановлена."
      mode="$(stat -c '%a' -- "$path")" || die "Не удалось проверить $path — установка остановлена."
      [[ $owner == 0 ]] || die "$path принадлежит не root — это небезопасно, установка остановлена."
      (( (8#$mode & 8#022) == 0 )) || die "$path могут менять другие пользователи — это небезопасно."
    done
  }
  check_root_owned / /usr /usr/lib
  sudo install -d -o root -g root -m 0755 "$HELPER_DIR"
  sudo install -o root -g root -m 0755 target/release/omarchy-hotspot-helper "$HELPER"
  check_root_owned "$HELPER_DIR" "$HELPER"
  # Разрешение polkit — только после успешной проверки помощника.
  sudo install -o root -g root -m 0644 packaging/org.omarchy.hotspot.policy "$POLICY"
  sudo install -o root -g root -m 0644 packaging/omarchy-hotspot-cleanup.service "$CLEANUP_UNIT"
  sudo systemctl daemon-reload
  sudo systemctl enable --quiet omarchy-hotspot-cleanup.service
  say "Помощник защиты установлен и проверен"

  # 4. Программа для пользователя
  say "Ставлю программу в $BIN_DIR"
  mkdir -p "$BIN_DIR"
  # На этапах разработки здесь могла быть ссылка на папку сборки — заменяем настоящим файлом.
  [[ -L $BIN_DIR/$APP ]] && rm -f -- "$BIN_DIR/$APP"
  install -m 0755 target/release/omarchy-hotspot "$BIN_DIR/$APP"
  case ":$PATH:" in *":$BIN_DIR:"*) ;; *) warn "$BIN_DIR нет в PATH — добавь его, чтобы команда omarchy-hotspot находилась." ;; esac

  tmp="$(mktemp)"
  sed "s|@BIN@|$BIN_DIR/$APP|" packaging/omarchy-hotspot.service >"$tmp"
  install -D -m 0644 "$tmp" "$USER_UNIT"
  rm -f -- "$tmp"
  systemctl --user daemon-reload || warn "Не удалось перечитать службы пользователя (нет сеанса?)."
  # Обновление: работающая служба перезапускается, чтобы работал новый код.
  systemctl --user try-restart omarchy-hotspot.service >/dev/null 2>&1 || true
fi

# ---- Настройка Omarchy для текущего пользователя: только с согласия ----
cat <<MSG

Для удобства установщик может настроить Omarchy (перед каждой правкой — копия *.bak-<дата>):
  • $HYPR_CONF — правило окна: плавающее, стартовый размер (вставка между маркерами)
  • $MENU_CONF — пункты «Hotspot» и «Hotspot On/Off» в меню Setup → Network
  • значок на панели: плагин $PLUGIN_ID в $PLUGINS и запись о нём в $SHELL_JSON
MSG
if own_plugin "$PLUGINS/$OLD_PLUGIN_ID" "$OLD_PLUGIN_ID"; then
  echo "  • старый значок $PLUGINS/$OLD_PLUGIN_ID будет убран (заменяется новым)"
fi
echo

if ! ask "Изменить эти файлы?"; then
  echo
  say "Программа установлена, настройки Omarchy не тронуты. Открыть окно: omarchy-hotspot"
  say "Настроить позже: запусти установщик ещё раз (из пакета — omarchy-hotspot-setup)."
  exit 0
fi

# 5. Окно: плавающее, стартовый размер. Место в правом верхнем углу под панелью (как у панелей
# Omarchy) и точный размер задаёт `omarchy-hotspot open` правилом в памяти Hyprland перед показом
# окна — числами, без `window_w`: иначе окно появлялось левее и прыгало в угол (docs/DECISIONS.md §8.2).
# Размер 760×440 здесь и в `fit::DEFAULT_SIZE` должен совпадать.
HYPR_BEGIN='-- omarchy-hotspot begin'
HYPR_END='-- omarchy-hotspot end'
HYPR_BLOCK="$HYPR_BEGIN
o.window(\"^(org\\\\.omarchy\\\\.omarchy-hotspot)\$\", { float = true, size = { 760, 440 } })
$HYPR_END"
if [[ -f $HYPR_CONF ]]; then
  b_line="$(marker_line "$HYPR_CONF" "$HYPR_BEGIN")"
  e_line="$(marker_line "$HYPR_CONF" "$HYPR_END")"
  if [[ -z $b_line && -z $e_line ]]; then
    backup "$HYPR_CONF"
    # Если файл не кончается переводом строки — добавим его, чтобы вставка не склеилась.
    [[ -z $(tail -c1 -- "$HYPR_CONF") ]] || echo >>"$HYPR_CONF"
    printf '%s\n' "$HYPR_BLOCK" >>"$HYPR_CONF"
    say "Добавлено правило окна в $HYPR_CONF"
    hyprctl reload >/dev/null 2>&1 || true
  elif [[ ! $b_line =~ ^[0-9]+$ || ! $e_line =~ ^[0-9]+$ ]] || ((b_line >= e_line)); then
    # Маркеров не по одному или они перепутаны — файл правили руками, не трогаем.
    warn "В $HYPR_CONF маркеры omarchy-hotspot повреждены — правило окна не обновлено."
  elif [[ $(sed -n "${b_line},${e_line}p" "$HYPR_CONF") == "$HYPR_BLOCK" ]]; then
    say "Правило окна Hyprland уже есть"
  else
    # Правило от прошлой версии: заменяем блок между маркерами целиком.
    backup "$HYPR_CONF"
    tmp="$(mktemp)"
    HYPR_BLOCK="$HYPR_BLOCK" awk -v b="$b_line" -v e="$e_line" \
      'NR == b { print ENVIRON["HYPR_BLOCK"] } NR < b || NR > e { print }' "$HYPR_CONF" >"$tmp"
    cat -- "$tmp" >"$HYPR_CONF" || die "Не удалось записать $HYPR_CONF — верни копию $HYPR_CONF.bak-$STAMP"
    rm -f -- "$tmp"
    say "Обновлено правило окна в $HYPR_CONF"
    hyprctl reload >/dev/null 2>&1 || true
  fi
else
  warn "Не найден $HYPR_CONF — окно откроется обычным, не плавающим."
fi

# 6. Меню Omarchy: Setup → Network → Hotspot
MENU_BLOCK='  // omarchy-hotspot begin
  "setup.network.hotspot": {"icon":"󱜠","label":"Hotspot","aliases":["hotspot"],"action":"omarchy-hotspot open"},
  "setup.network.hotspot-toggle": {"icon":"󰐥","label":"Hotspot On/Off","checked":"omarchy-hotspot status --quiet","action":"omarchy-hotspot toggle"},
  // omarchy-hotspot end'
mkdir -p "$(dirname "$MENU_CONF")"
if [[ ! -f $MENU_CONF ]]; then
  printf '{\n%s\n}\n' "$MENU_BLOCK" >"$MENU_CONF"
  say "Создан $MENU_CONF с пунктами раздачи"
elif grep -q '// omarchy-hotspot begin' "$MENU_CONF"; then
  b_line="$(marker_line "$MENU_CONF" '// omarchy-hotspot begin')"
  e_line="$(marker_line "$MENU_CONF" '// omarchy-hotspot end')"
  if [[ ! $b_line =~ ^[0-9]+$ || ! $e_line =~ ^[0-9]+$ ]] || ((b_line >= e_line)); then
    warn "В $MENU_CONF маркеры omarchy-hotspot повреждены — пункты меню не обновлены."
  elif [[ $(sed -n "${b_line},${e_line}p" "$MENU_CONF") == "$MENU_BLOCK" ]]; then
    say "Пункты меню уже есть"
  else
    # Пункты от прошлой версии (например, другое действие открытия окна): заменяем блок целиком.
    backup "$MENU_CONF"
    tmp="$(mktemp)"
    MENU_BLOCK="$MENU_BLOCK" awk -v b="$b_line" -v e="$e_line" \
      'NR == b { print ENVIRON["MENU_BLOCK"] } NR < b || NR > e { print }' "$MENU_CONF" >"$tmp"
    cat -- "$tmp" >"$MENU_CONF" || die "Не удалось записать $MENU_CONF — верни копию $MENU_CONF.bak-$STAMP"
    rm -f -- "$tmp"
    say "Обновлены пункты меню в $MENU_CONF"
  fi
elif grep -qx '{' "$MENU_CONF"; then
  backup "$MENU_CONF"
  # Вставляем сразу после первой строки «{»: у наших строк запятая в конце,
  # поэтому то, что ниже, остаётся правильным JSONC.
  tmp="$(mktemp)"
  MENU_BLOCK="$MENU_BLOCK" awk '!done && $0 == "{" { print; print ENVIRON["MENU_BLOCK"]; done = 1; next } { print }' \
    "$MENU_CONF" >"$tmp"
  cat -- "$tmp" >"$MENU_CONF" || die "Не удалось записать $MENU_CONF — верни копию $MENU_CONF.bak-$STAMP"
  rm -f -- "$tmp"
  say "Добавлены пункты меню в $MENU_CONF"
else
  warn "Не понял формат $MENU_CONF — пункты меню не добавлены (вставь их вручную, см. README)."
fi

# 7. Значок на панели Omarchy
if command -v omarchy-shell >/dev/null 2>&1; then
  # Значок до публикации назывался omarchy-hotspot — убираем его, новый ставится ниже.
  if own_plugin "$PLUGINS/$OLD_PLUGIN_ID" "$OLD_PLUGIN_ID"; then
    backup "$SHELL_JSON"
    omarchy-plugin-disable "$OLD_PLUGIN_ID" >/dev/null 2>&1 || true
    rm -rf -- "${PLUGINS:?}/$OLD_PLUGIN_ID"
    say "Значок со старым id $OLD_PLUGIN_ID убран"
  fi
  if [[ -d $PLUGIN_DIR/.git ]]; then
    # Поставлен через `omarchy plugin add`: файлы ведёт git, установщик их не перезаписывает.
    say "Значок поставлен через «omarchy plugin add» — обновление: omarchy plugin update $PLUGIN_ID"
    omarchy-shell shell rescanPlugins >/dev/null 2>&1 || true
  else
    # Обновление уже установленного значка: `rescanPlugins` не выгружает из памяти оболочки старый
    # код значка (проверено: клик продолжал выполнять прежнюю команду), поэтому оболочку перезапускаем.
    plugin_updated=0
    if [[ -d $PLUGIN_DIR ]]; then
      for f in manifest.json BarWidget.qml; do
        cmp -s -- "$f" "$PLUGIN_DIR/$f" || plugin_updated=1
      done
    fi
    mkdir -p "$PLUGIN_DIR"
    install -m 0644 manifest.json BarWidget.qml "$PLUGIN_DIR/"
    omarchy-plugin-validate "$PLUGIN_DIR" >/dev/null || die "Omarchy не принял плагин панели."
    if ((plugin_updated)) && command -v omarchy-restart-shell >/dev/null 2>&1; then
      say "Значок на панели обновился — перезапускаю оболочку Omarchy (панель мигнёт), чтобы загрузить новую версию"
      omarchy-restart-shell >/dev/null 2>&1 ||
        warn "Не удалось перезапустить оболочку. Выполни вручную: omarchy-restart-shell"
    else
      omarchy-shell shell rescanPlugins >/dev/null 2>&1 || true
    fi
  fi
  if [[ -f $SHELL_JSON ]] &&
    jq -e --arg id "$PLUGIN_ID" '[.. | objects | select(.id? == $id)] | length > 0' "$SHELL_JSON" >/dev/null; then
    say "Значок на панели уже включён"
  else
    backup "$SHELL_JSON"
    if omarchy-plugin-enable "$PLUGIN_ID" --section right --before omarchy.network >/dev/null 2>&1 ||
      omarchy-plugin-enable "$PLUGIN_ID" >/dev/null 2>&1; then
      say "Значок добавлен на панель"
    else
      warn "Не удалось включить значок. Попробуй вручную: omarchy plugin enable $PLUGIN_ID"
    fi
  fi
else
  warn "omarchy-shell не найден — значок на панели не добавлен."
fi

echo
say "Готово. Открыть окно: значок раздачи на панели, меню Omarchy → Setup → Network → Hotspot,"
say "или команда: omarchy-hotspot"
