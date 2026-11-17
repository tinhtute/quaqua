# No GUI driving

Never drive anh iu's real display: do not launch GUI apps, move windows, send synthetic X11/Wayland input, or take screenshots on DISPLAY=:0 / the live Wayland session. Verify UI work code-only (build, tests, lint); the owner does the visual pass. A headless display (Xvfb) is allowed only if the task prompt explicitly says so.

On macOS, the same rule forbids `osascript`, `open`, `screencapture`, and `cliclick` for driving or capturing the owner's real display. Use code-only checks unless the task explicitly authorizes an isolated/headless UI test.

Why: 2026-09-29 a mono-desk worker hijacked the owner's mouse and screen.
