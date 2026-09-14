// Значок omarchy-hotspot на панели Omarchy.
// Клик — окно раздачи, правый клик — вкл/выкл, средний — обновить.
// Состояние берём из `omarchy-hotspot status --waybar` каждые 5 секунд.
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
  property string tip: "Hotspot"
  property string hotspotState: "off"

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  function refresh() {
    if (!statusProc.running) statusProc.running = true
  }

  function apply(raw) {
    try {
      var data = JSON.parse(String(raw || "").trim())
      root.icon = String(data.text || root.icon)
      root.tip = String(data.tooltip || "")
      root.hotspotState = String(data.class || "off")
    } catch (e) {
      // Программа не установлена или вывод не JSON — показываем «выключено».
      root.hotspotState = "off"
    }
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
    dimmed: root.hotspotState === "off"
    tooltipText: root.tip

    onPressed: function(b) {
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
