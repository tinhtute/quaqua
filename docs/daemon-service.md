# Daemon service: sleep and resume

The daemon lives in its own process, but a machine sleep still cuts the workers it watches.
Linux (systemd) is supported. macOS (a launchd agent plus a sleep guard such as `caffeinate -i`) is a follow-up and not done.

- `quaqua daemon install [--dry-run]` writes `${XDG_CONFIG_HOME:-~/.config}/systemd/user/quaqua-daemon.service`
  (`QUAQUA_HOME` is the current workspace, `PATH` is the installer's own `PATH` at install time,
  because a systemd user unit starts with a bare one; re-run install after changing it) and prints the path and the enable command
  `systemctl --user enable --now quaqua-daemon.service`. It never enables or starts the unit.
  `--dry-run` prints the unit text only and writes nothing.
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
