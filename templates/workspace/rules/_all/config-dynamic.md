<!-- audience: orchestrator -->
# Cấu hình động & profile của quaqua

Cấu hình đổi **không cần sửa code**: mọi giá trị nằm trong `team.toml`, đọc lại mỗi lần spawn.

## Đọc ở đâu, khi nào
- Thứ tự resolve (`config.rs:load_team_config`): `$QUAQUA_TEAM_TOML` → `./team.toml` (cwd) → `workspace_root()/team.toml` → bản **nhúng trong binary** (`include_str!` của `templates/workspace/team.toml` trong engine checkout).
- **Hệ quả:** sửa `team.toml` có tác dụng ngay cho spawn kế tiếp (worker, pipeline, lead — không rebuild). NHƯNG nếu không tìm thấy file lúc chạy, binary rơi về bản nhúng lúc build — muốn đổi giá trị nhúng phải rebuild.
- `[projects.<slug>]` merge đè lên default theo project (`config::resolve`).

## Cap
- CoS: `[limits] max_workers_per_orchestrator` (trần `CAP_CEILING` = 8). Lead: `[roles.lead] max_peers`, mặc định 4, phải trong `1..=trần`.
- Một cổng duy nhất cho mọi đường spawn (worker, pipeline, taskforce): `cap::check_owner_cap` — owner `<slug>-lead` bị chặn ở `max_peers`, owner khác ở `[limits]`. Pipeline nặng 2 slot (`[limits.weights]`).

## Kind = một profile
`[kinds.<name>]` gói `harness` (opencode/omp/claude/pi/agy) · `model` · `effort` (bắt buộc; opencode: variant `#effort`) · `fallback` (1 hop khi start fail) · `can_review` · `on_cost_cap` (`warn` mặc định | `kill`: watchdog interrupt 1 lần ở 100% cost budget; `kill` cần stop key trên harness của kind **và** của fallback — opencode/omp/agy có, claude/pi không; `config validate` và `worker spawn` từ chối, watchdog hạ xuống warn).
- Đổi model/effort của một kind = sửa 1 dòng trong team.toml; spawn kế tiếp dùng giá trị mới.
- Reviewer: bất kỳ kind `can_review = true` — pipeline không còn allowlist cứng (2026-10-17, runs/sude-guard.md); fallback của reviewer cũng phải `can_review = true`.
- Builder/reviewer khác harness: `strategy.builder_reviewer_differ = "harness"` (mặc định) — `config validate` so cặp mặc định (`purposes.build`, không có thì `default_kind`, với `default_reviewer`); pipeline còn từ chối mọi cặp trùng harness lúc chạy (kể cả `--builder`/`--reviewer` chỉ định). `"kind"` và `"none"` chỉ tắt kiểm tra lúc validate, không tắt kiểm tra lúc chạy.
- Lead: `strategy.lead_kind` (override `LEAD_KIND`), đi qua cùng `build_launch_cmd` như worker.

## Một lần, không đổi file
- `LEAD_KIND=<kind>` cho lead spawn.
- `QUAQUA_TEAM_TOML=<path>` trỏ file config khác (test/experiment).
- `GRID_TAB`, `QUAQUA_OWNER` — định vị grid.
- `QUAQUA_LAYOUT_SHADOW=1` — escape hatch đưa layout gate về shadow mode (ghi would_deny, không chặn).

## Profile "Claude out" (mẫu đã chạy)
Khi hết token Claude: `suhuynh` → `pi · opencode-go/deepseek-v4.1-flash · max` (thay thế: `claude · opus-5.5 · medium`); `ni`/`truonglao` pause; builder = `sude` (omp) để reviewer khác harness; CoS chạy trên opencode. Quay lại bảng gốc = sửa lại kind, không build lại.

## Kiểm tra nhanh
- `quaqua config get kinds.suhuynh.model` — đọc giá trị hiệu lực.
- `quaqua config validate` — so doc↔doc (models.md ↔ team.toml); chưa so runtime.
