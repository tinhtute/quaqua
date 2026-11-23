# Daemon service: sleep and resume

The daemon lives in its own process, but a machine sleep still cuts the workers it watches.
Linux (systemd) is supported; macOS launchd is not.

- `quaqua daemon install [--dry-run]` writes `${XDG_CONFIG_HOME:-~/.config}/systemd/user/quaqua-daemon.service`
  (`QUAQUA_HOME` is the current workspace) and prints the path and the enable command
  `systemctl --user enable --now quaqua-daemon.service`. It never enables or starts the unit.
  `--dry-run` prints the unit text only and writes nothing.
- While any Lead or Peer agent is open, the daemon holds a `systemd-inhibit --what=sleep` lock and
  drops it when none is open. The lock process exits with the daemon (`tail --pid`).
  `QQ_INHIBIT=0` turns this and wake detection off. A missing `systemd-inhibit` is silently skipped.
- Every 5s the daemon compares the wall clock to the monotonic clock. A wall clock more than 10s
  ahead is a resume; the daemon mails cos a `state` mail naming the agents that were open.
