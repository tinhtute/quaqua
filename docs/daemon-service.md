# Daemon service: sleep and resume

The daemon lives in its own process, but a machine sleep still cuts the workers it watches.
Linux (systemd) and macOS (launchd) are supported. The sleep guard is Linux only; a macOS guard such as `caffeinate -i` is a follow-up and not done.

- `quaqua daemon install [--dry-run]` writes `${XDG_CONFIG_HOME:-~/.config}/systemd/user/quaqua-daemon.service`
  (`QUAQUA_HOME` is the current workspace, `PATH` is the installer's own `PATH` at install time,
  because a systemd user unit starts with a bare one; re-run install after changing it) and prints the path and the enable command
  `systemctl --user enable --now quaqua-daemon.service`. It never enables or starts the unit.
  `--dry-run` prints the unit text only and writes nothing.
- On macOS `quaqua daemon install [--dry-run]` writes a LaunchAgent plist, `~/Library/LaunchAgents/com.quaqua.daemon.plist` by default
  (`daemon.launchd_label` and `daemon.launchd_dir` in `team.toml` move it), and prints `launchctl bootstrap gui/<uid> <plist>`.
  The plist runs `quaqua daemon` with `QUAQUA_HOME`, `PATH`, `HERDR_SESSION` and `HERDR_SOCKET_PATH` from the installing environment,
  `RunAtLoad`, `KeepAlive` on a non-zero exit only (a clean `quaqua stop` stays stopped) and a 5s `ThrottleInterval`; stdout and stderr go to `runs/daemon.log`.
  Like the systemd path, install never loads or starts it. A legacy `com.quaqua.auditor*.plist` is printed as a `duplicate:` and left alone.
- `quaqua up` starts the daemon on either OS through the same service seam when it does not answer. On systemd it runs `systemctl --user start quaqua-daemon.service`.
  On launchd it runs `launchctl print gui/<uid>/<label>` first: a loaded label gets `launchctl kickstart` (never `-k`, so a live daemon is not killed),
  an unloaded one gets `launchctl bootstrap gui/<uid> <plist>`. Only the exit status of `print` counts. `up` never boots out an agent. With no plist it offers the install, as on Linux.
  A failed command is shown as `starting <unit> failed: \`<command>\` failed: <stderr>`, and a start that does not answer in time names `launchctl print gui/<uid>/<label>`.
  The `gui/<uid>` domain needs a GUI login: over SSH, `bootstrap` fails and its error is shown verbatim.
- The unit also carries `HERDR_SESSION` and `HERDR_SOCKET_PATH`, copied from the installing environment, so the daemon
  reaches herdr (a systemd user unit starts with neither). Install refuses, naming the missing variable, when either is unset or empty:
  run it from inside the herdr session the daemon should use. The socket path is
  `~/.config/herdr/sessions/<session>/herdr.sock`; it is keyed by session name, so it stays the same across reboots, but
  re-run install if you move the daemon to another session.
- The daemon's Supervisor job owns the auditor (#316). If `systemd/user/` still holds a legacy `quaqua-auditor*.timer` or
  `.service`, install prints each as a `duplicate:` and leaves it alone; disable and remove it yourself
  (`systemctl --user disable --now quaqua-auditor.timer`). There is no `quaqua doctor` in the engine, so install is where this is checked.
- While any Lead or Peer agent is open, the daemon holds a `systemd-inhibit --what=sleep` lock and
  drops it when none is open. The lock process exits with the daemon (`tail --pid`).
  `QQ_INHIBIT=0` turns this and wake detection off. A missing `systemd-inhibit` is silently skipped; a lock child that exits at once (logind refuses,
  no D-Bus) is dropped and retried on the next tick. A failed store read keeps the current lock state,
  and the wake mail is skipped when no agent was open.
- Limit: the lock is `--what=sleep`, which stops idle and manual suspend but not a lid-close
  suspend (logind's `LidSwitchIgnoreInhibited=yes` default). Blocking `handle-lid-switch` too would
  keep a closed laptop running hot in a bag, so we do not; keep the lid open, or set
  `LidSwitchIgnoreInhibited=no` in logind.conf, while workers run.
- The service thread (inhibitor and wake mail) runs only under `quaqua daemon`; a daemon built in
  code (tests, embedders) is inert unless it calls `Daemon::enable_service`.
- Every 5s the daemon compares the wall clock to the monotonic clock. A wall clock more than 10s
  ahead is a resume; the daemon mails cos a `state` mail naming the agents that were open.
