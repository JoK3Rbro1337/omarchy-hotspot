// Значок omarchy-hotspot на панели Omarchy.
// Клик — окно раздачи, правый клик — вкл/выкл, средний — обновить.
// Состояние берём из `omarchy-hotspot status --waybar` каждые 5 секунд.
// Программы нет (значок поставили из каталога отдельно) — приглушённый значок,
// подсказка в тултипе, по клику уведомление с командой установки и README.
// Команды запускаются массивом аргументов, без оболочки.
import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

BarWidget {
  id: root
  moduleName: "io.github.jok3rbro1337.omarchy-hotspot"

  // Программу ищем по PATH: из исходников она в ~/.local/bin, из пакета AUR — в /usr/bin.
  readonly property string binPath: "omarchy-hotspot"
  property string icon: "\uDB85\uDF21"
  property string tip: "Wi-Fi Hotspot"
  // "on" | "off" | "missing" (значок стоит, а программы нет)
  property string hotspotState: "off"
  readonly property string repoUrl: "https://github.com/JoK3Rbro1337/omarchy-hotspot"

  // Язык системы: ru, uk или en. Эти тексты — единственные в плагине,
  // остальные подписи приходят готовыми из программы.
  readonly property string lang: {
    const name = String(Qt.locale().name || "")
    if (name.startsWith("ru")) return "ru"
    if (name.startsWith("uk")) return "uk"
    return "en"
  }

  readonly property var texts: ({
    ru: {
      tip: "Wi-Fi Hotspot: программа не установлена.\nКлик — как установить",
      title: "Программа раздачи не установлена",
      body: "Значок из каталога плагинов стоит, а самой программы нет.\nУстановка: yay -S omarchy-hotspot (AUR) или ./install.sh из репозитория."
    },
    uk: {
      tip: "Wi-Fi Hotspot: програма не встановлена.\nКлік — як встановити",
      title: "Програма роздачі не встановлена",
      body: "Значок з каталогу плагінів є, а самої програми немає.\nВстановлення: yay -S omarchy-hotspot (AUR) або ./install.sh з репозиторію."
    },
    en: {
      tip: "Wi-Fi Hotspot: app is not installed.\nClick for install instructions",
      title: "Hotspot app is not installed",
      body: "The catalog installs the bar icon only.\nInstall the app: yay -S omarchy-hotspot (AUR) or ./install.sh from the repository."
    }
  })

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  function refresh() {
    if (!statusProc.running) statusProc.running = true
    // Если программы нет в PATH, процесс не запустится и ответа не будет вовсе —
    // тогда через 3 секунды сработает этот страж и покажет подсказку.
    answerWatchdog.restart()
  }

  function apply(raw) {
    answerWatchdog.stop()
    const out = String(raw || "").trim()
    if (out === "") {
      // Пусто на выходе — программы нет в PATH (или она не запустилась).
      root.showMissing()
      return
    }
    try {
      var data = JSON.parse(out)
      root.icon = String(data.text || root.icon)
      root.tip = String(data.tooltip || "")
      root.hotspotState = String(data.class || "off")
    } catch (e) {
      // Вывод не JSON — считаем, что программы нет: подсказать полезнее, чем молчать.
      root.showMissing()
    }
  }

  // Значок есть, программы нет: приглушённый значок и подсказка, как поставить.
  function showMissing() {
    root.hotspotState = "missing"
    root.tip = root.texts[root.lang].tip
  }

  // Клик по значку без программы: уведомление с командой и страница установки.
  function offerInstall() {
    const t = root.texts[root.lang]
    Quickshell.execDetached(["notify-send", "-a", "Wi-Fi Hotspot", t.title, t.body])
    Quickshell.execDetached(["xdg-open", root.repoUrl + "#readme"])
  }

  Process {
    id: statusProc
    command: [root.binPath, "status", "--waybar"]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: root.apply(text)
    }
  }

  Timer {
    interval: 5000
    running: true
    repeat: true
    triggeredOnStart: true
    onTriggered: root.refresh()
  }

  // Ответа не пришло — программы нет.
  Timer {
    id: answerWatchdog
    interval: 3000
    repeat: false
    onTriggered: root.showMissing()
  }

  // После вкл/выкл обновляем значок быстрее обычного.
  Timer {
    id: soonTimer
    interval: 1500
    repeat: true
    property int left: 0
    onTriggered: {
      root.refresh()
      if (--left <= 0) stop()
    }
  }

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: root.icon
    slotSize: Style.bar.statusSlot
    dimmed: root.hotspotState !== "on"
    tooltipText: root.tip

    onPressed: function(b) {
      if (root.hotspotState === "missing") {
        // Программы нет: средний клик — проверить ещё раз, любой другой — как установить.
        if (b === Qt.MiddleButton) root.refresh()
        else root.offerInstall()
        return
      }
      if (b === Qt.RightButton) {
        // Пока идёт прошлое вкл/выкл, повторные клики игнорируем.
        if (soonTimer.running) return
        Quickshell.execDetached([root.binPath, "toggle"])
        soonTimer.left = 6
        soonTimer.restart()
      } else if (b === Qt.MiddleButton) {
        root.refresh()
      } else {
        Quickshell.execDetached([root.binPath, "open"])
      }
    }
  }
}
