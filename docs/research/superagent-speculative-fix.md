# T5: Speculative CI auto-fix

Status: **draft**, chưa brainstorm. Index: [superagent.md](superagent.md). Nguồn: spec gốc §4 "Speculative work" ([superagent-source-spec.md](superagent-source-spec.md)), NG4 của plan bus.
Baseline: `main` @ `cde0394`.

## Là gì

Khi có trigger `ci_failed` trên một thread, quaqua tự spawn một worker. Worker này lấy log CI, repro ở local và commit bản sửa lên một branch riêng trước khi owner kịp xem. Owner chỉ nhận một trigger: "đã có bản sửa ở branch X, test Y pass".

## Vì sao

- Spec gốc để ý này ở "Tạm bỏ qua" vì hai lý do. Nó cần có 3.1 trước, và nó tốn token trong khi mỗi brief đã có budget thời gian và chi phí (`templates/workspace/AGENTS.md:63`).
- T1 P5 gỡ được lý do thứ nhất, vì `ci_failed` sẽ có owner và thread. Lý do thứ hai vẫn còn và là câu hỏi chính của track này.

## Phụ thuộc

- T1 P5: trigger `ci_failed` có `run id` và branch.
- T1 P4: `quaqua recall` để worker lấy log và report liên quan.
- Tuỳ chọn T2: watchdog LLM để chặn bản sửa lệch mục tiêu.

## Điểm chạm (tại `cde0394`)

- `quaqua worker spawn` và `quaqua worker prompt`: đường spawn và brief sẵn có. Không thêm đường spawn thứ hai.
- `templates/workspace/team.toml`: cap 8 slot của CoS. Worker speculative chiếm slot, nên cần quy tắc ưu tiên.

## Câu hỏi cho brainstorm

- Ai được tự spawn? CoS (theo AGENTS.md mọi spawn đi qua CoS/Lead), hay một Source handler không có LLM?
- Budget riêng cho speculative là bao nhiêu (thời gian, chi phí, số lần/ngày)? Vượt thì dừng và chỉ báo.
- Chỉ sửa loại lỗi nào: test fail tất định, lint, fmt? Bỏ qua flaky và lỗi mạng?
- Bản sửa chỉ ở local branch. Push và merge vẫn cần anh iu duyệt, không có ngoại lệ.
- Đo bằng gì: tỉ lệ bản sửa được nhận so với chi phí token mỗi tuần?

## Ngoài phạm vi track này

Tự push, tự mở PR, tự merge.
