#!/usr/bin/env bash
# Полное удаление omarchy-hotspot: выключает раздачу, снимает правила брандмауэра,
# убирает файлы, службы и вставки в конфиги. Повторный запуск безопасен.
# Два режима, как у install.sh:
#   ./uninstall.sh           — установка из исходников: убирает и системные файлы (sudo);
#   omarchy-hotspot-remove   — пакет AUR (скрипт в /usr/share/omarchy-hotspot): убирает всё своё
#                              у пользователя, системные файлы потом удаляет pacman.
set -euo pipefail

cd "$(dirname "$(readlink -f "$0")")"

APP=omarchy-hotspot
PACKAGED=0
[[ $PWD == /usr/share/omarchy-hotspot ]] && PACKAGED=1
BIN="$HOME/.local/bin/omarchy-hotspot"
HELPER_DIR=/usr/lib/omarchy-hotspot
HELPER="$HELPER_DIR/omarchy-hotspot-helper"
POLICY=/usr/share/polkit-1/actions/org.omarchy.hotspot.policy
CLEANUP_UNIT=/etc/systemd/system/omarchy-hotspot-cleanup.service
USER_UNIT="$HOME/.config/systemd/user/omarchy-hotspot.service"
HYPR_CONF="$HOME/.config/hypr/hyprland.lua"
MENU_CONF="$HOME/.config/omarchy/extensions/omarchy-menu.jsonc"
SHELL_JSON="$HOME/.config/omarchy/shell.json"
PLUGINS="$HOME/.config/omarchy/plugins"
# Новый id значка и прежний (до публикации).
PLUGIN_IDS=(io.github.jok3rbro1337.omarchy-hotspot omarchy-hotspot)
STAMP="$(date +%Y%m%d-%H%M%S)"
# Временный файл правки конфигов убирается при любом выходе.
tmp=""
trap 'rm -f -- "${tmp:-}"' EXIT

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

# Убрать из файла блок между строками-маркерами (с копией файла).
remove_block() {
  local file=$1 begin=$2 end=$3 b_line e_line
  [[ -f $file ]] && grep -qF -- "$begin" "$file" || return 0
  # Ровно одна пара маркеров, begin выше end — иначе файл правили руками, не трогаем.
  b_line="$(grep -nxF -- "$begin" <(sed 's/^[[:space:]]*//; s/[[:space:]]*$//' "$file") | cut -d: -f1 || true)"
  e_line="$(grep -nxF -- "$end" <(sed 's/^[[:space:]]*//; s/[[:space:]]*$//' "$file") | cut -d: -f1 || true)"
  if [[ ! $b_line =~ ^[0-9]+$ || ! $e_line =~ ^[0-9]+$ ]] || ((b_line >= e_line)); then
    warn "В $file маркеры omarchy-hotspot повреждены — убери вставку вручную."
    return 0
  fi
  cp -p -- "$file" "$file.bak-$STAMP" || die "Не удалось сделать копию $file — файл не трогаю."
  tmp="$(mktemp)"
  awk -v b="$b_line" -v e="$e_line" 'NR < b || NR > e' "$file" >"$tmp"
  cat -- "$tmp" >"$file" || die "Не удалось записать $file — верни копию $file.bak-$STAMP"
  rm -f -- "$tmp"
  say "Убрана вставка из $file (копия: $file.bak-$STAMP)"
}

[[ $EUID -ne 0 ]] || die "Запускай без sudo: root-команды скрипт попросит сам."
[[ -t 0 ]] || die "Нужен терминал: скрипт задаёт вопросы."

# Помощник из пакета, а скрипт запущен из папки исходников — системные файлы удалит только pacman.
if ((!PACKAGED)) && owner="$(package_owner)"; then
  die "omarchy-hotspot установлен пакетом ($owner). Удаление: omarchy-hotspot-remove, затем sudo pacman -Rns $owner"
fi

if ((PACKAGED)); then
  system_note="  • системные файлы (помощник, polkit, службы) удалит pacman: sudo pacman -Rns omarchy-hotspot"
else
  system_note="  • программа $BIN и системные файлы: $HELPER_DIR, $POLICY, $CLEANUP_UNIT"
fi
cat <<MSG
Будет удалено:
  • раздача выключится, её профиль в NetworkManager (с паролем) и настройки
    ~/.config/omarchy-hotspot будут удалены
  • правила брандмауэра и файл DNS раздачи
  • фоновая служба, значок на панели, пункты меню, правило окна
$system_note
MSG
ask "Удалить omarchy-hotspot?" || die "Отменено."

# 1. Выключить раздачу и убрать всё, что она создала (пока помощник ещё на месте)
if [[ -x $BIN ]]; then
  "$BIN" reset --yes || warn "reset завершился с ошибкой — продолжаю удаление."
elif command -v "$APP" >/dev/null 2>&1; then
  "$APP" reset --yes || warn "reset завершился с ошибкой — продолжаю удаление."
else
  # Программы уже нет — убираем профиль сети и настройки напрямую.
  warn "Программа не найдена — удаляю профиль сети и настройки напрямую."
  nmcli connection delete id omarchy-hotspot >/dev/null 2>&1 || true
  rm -rf -- "$HOME/.config/omarchy-hotspot"
fi
# Запомненный размер окна (его убирает и reset; здесь — на случай, если reset не сработал).
rm -rf -- "$HOME/.local/state/omarchy-hotspot"

# 2. Фоновая служба пользователя (файл службы из пакета удалит pacman)
systemctl --user disable --now omarchy-hotspot.service >/dev/null 2>&1 || true
rm -f -- "$USER_UNIT"
systemctl --user daemon-reload || warn "Не удалось перечитать службы пользователя (нет сеанса?)."

# 3. Значок на панели (новый и прежний id)
for id in "${PLUGIN_IDS[@]}"; do
  [[ -d $PLUGINS/$id ]] || continue
  if ! own_plugin "$PLUGINS/$id" "$id"; then
    warn "$PLUGINS/$id — не удалось подтвердить, что это значок omarchy-hotspot (другой автор в manifest.json или нет jq), не трогаю."
    continue
  fi
  [[ -f $SHELL_JSON ]] && cp -p -- "$SHELL_JSON" "$SHELL_JSON.bak-$STAMP"
  omarchy-plugin-disable "$id" >/dev/null 2>&1 || true
  rm -rf -- "${PLUGINS:?}/$id"
  say "Значок $id убран с панели"
done
omarchy-shell shell rescanPlugins >/dev/null 2>&1 || true

# 4. Вставки в конфиги
remove_block "$HYPR_CONF" '-- omarchy-hotspot begin' '-- omarchy-hotspot end'
hyprctl reload >/dev/null 2>&1 || true
remove_block "$MENU_CONF" '// omarchy-hotspot begin' '// omarchy-hotspot end'

if ((PACKAGED)); then
  echo
  say "Всё своё у пользователя убрано. Осталось удалить пакет: sudo pacman -Rns omarchy-hotspot"
  say "Копии изменённых конфигов — файлы *.bak-$STAMP."
  exit 0
fi

# 5. Системная часть
cat <<MSG

Сейчас будут выполнены команды с правами администратора (sudo):
  $HELPER fw-clear      — снять правила брандмауэра раздачи (если остались)
  $HELPER dns-clear     — удалить файл DNS раздачи (если остался)
  systemctl disable omarchy-hotspot-cleanup.service
  rm -f $CLEANUP_UNIT $POLICY
  rm -rf $HELPER_DIR
  systemctl daemon-reload

MSG
if ask "Продолжить?"; then
  if [[ -x $HELPER ]]; then
    sudo "$HELPER" fw-clear || warn "fw-clear завершился с ошибкой."
    sudo "$HELPER" dns-clear || warn "dns-clear завершился с ошибкой."
  fi
  sudo systemctl disable --quiet omarchy-hotspot-cleanup.service 2>/dev/null || true
  sudo rm -f -- "$CLEANUP_UNIT" "$POLICY"
  sudo rm -rf -- "$HELPER_DIR"
  sudo systemctl daemon-reload
  say "Системные файлы удалены"
else
  warn "Системные файлы оставлены. Запусти ./uninstall.sh ещё раз, чтобы удалить их."
fi

# 6. Сама программа — последней: выше она нужна для reset
rm -f -- "$BIN"

echo
say "omarchy-hotspot удалён. Копии изменённых конфигов — файлы *.bak-$STAMP."
