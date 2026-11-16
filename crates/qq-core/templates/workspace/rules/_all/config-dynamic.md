<!-- audience: orchestrator -->
# Cấu hình của quaqua

Roster, harness, model và effort nằm trong SQLite (`runs/qq.db`) và đổi bằng lệnh, không sửa code.

## Đọc ở đâu, khi nào
- `quaqua init` seed các bảng config từ `team.toml` đúng một lần; DB đã có kind thì được giữ nguyên, nên chạy lại không ghi đè thứ `config set` đã đổi.
- Sau seed, spawn chỉ đọc SQLite. Sửa file `team.toml` không đổi gì; muốn nạp lại cả file dùng `quaqua config import <team.toml>`.
- Thay đổi qua `quaqua config set` có tác dụng ở spawn kế tiếp, kể cả spawn của Lead đang chạy.

## Lệnh
- `quaqua config get [key]` — đọc giá trị hiệu lực (ví dụ `kinds.suhuynh.model`).
- `quaqua config set <key> <value>` — đổi một giá trị; một kind có `harness`, `model`, `effort`, `fallback`, `can_review`, `spawn`, `on_cost_cap`, `use_for`, `avoid`.
- `quaqua config import <team.toml>` — nạp file vào DB.
- `quaqua config validate [team.toml]` — kiểm tra file, hoặc DB nếu không đưa file.
- `quaqua roster` — in các kind với harness, model, effort, use_for, avoid.

## Kind = một profile
- Harness hợp lệ: `claude`, `pi`, `agy` (driver native) và `opencode` (driver generic). Tên khác bị từ chối.
- `fallback`: 1 hop khi agent start thất bại; `can_review`: kind được làm reviewer, và fallback của reviewer cũng phải `can_review = true`.
- `[strategy]`, `[limits]`, `[timers]` và `[roles.*]` của team.toml được lưu thành setting trong DB; đọc bằng `quaqua config get <section>.<key>`. Roster hiện chạy toàn claude nên builder và reviewer khác nhau theo tên kind (`builder_reviewer_differ = "kind"`), CoS giữ đúng quy tắc này bằng tay.
