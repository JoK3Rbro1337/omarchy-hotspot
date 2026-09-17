# Заявка в каталог плагинов Omarchy

Черновик для формы <https://github.com/omacom/omarchy-plugin-marketplace/issues/new?template=submit-plugin.yml>.
Заголовок задачи: `[Plugin]: Wi-Fi Hotspot`. Заголовки ниже и их порядок менять нельзя, галочки должны остаться
отмеченными. Требования каталога — `SUBMISSION.md` в его репозитории.

### Repository URL

https://github.com/JoK3Rbro1337/omarchy-hotspot

### Category

System

### Tags

bar, security, system

### Suggest a missing tag

network

### Maintainer notes

The listing is a bar widget (`BarWidget.qml`) that shows hotspot state and the number of connected
devices; click opens the hotspot window, right click toggles the hotspot. The widget runs no
privileged code: it only calls the `omarchy-hotspot` CLI. When that app is not installed, the icon
dims and a click shows a notification with the install command — the catalog installs the icon only,
and both README sections say so at the top.

The app itself (Rust, MIT) is installed separately with `./install.sh` from the repository; an AUR
package is prepared but not published yet. The automated baseline flags `privilege`, `installer`,
`service-management`, `package-manager` and `remote-build` for the repository. All of them come from
the app, not from the widget, and are documented: `install.sh` shows every `sudo` command and asks
before touching user config (backups plus `# omarchy-hotspot begin/end` markers, re-runs do not
duplicate), `uninstall.sh` reverses everything, and the only root component is a small helper with a
fixed command list (firewall rules for the hotspot, DNS file, kick/block a device, Wi-Fi country,
reading dnsmasq leases) called through pkexec with every argument validated. The threat model and
the full file list live in `docs/SECURITY.md`.

### Submission checklist

- [x] The repository is public and contains installation and removal instructions.
- [x] I have documented the plugin license and any external dependencies.
- [x] I confirm that I own or have permission to submit this plugin and its preview assets.
- [x] The plugin does not overwrite user configuration without explicit consent.
- [x] I understand that approval is for listing and is not a security review.
