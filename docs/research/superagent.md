# Superagent: index toàn bộ chương trình

Status: **index** (2026-11-02). Gom toàn bộ ý tưởng của spec gốc vào một chỗ, mỗi track một file con.
Baseline: `main` @ `cde0394`. Spec gốc (giữ nguyên văn): [superagent-source-spec.md](superagent-source-spec.md).

Chỉ track **T1 bus** đã khoá và có plan chạy được. Các track khác là **draft**: đã ghi đủ ý tưởng, bằng chứng và câu hỏi, nhưng chưa brainstorm, chưa lock và chưa có plan. Mỗi lần chỉ có một plan trong `.harness/plans/active/`, vì skill `work` từ chối chạy khi có nhiều hơn một. Vì vậy track draft chỉ thành plan sau khi T1 xong, hoặc khi anh iu chọn chạy nó trước.

## Tracks

| # | Track | Spec § | Status | File | Phụ thuộc |
|---|---|---|---|---|---|
| T1 | Bus: Source → Trigger → Thread → Subscription, digest, wake, recall, GitHub CI fail | 3.1–3.4 | **locked**, plan P0–P5 | [superagent-spec.md](superagent-spec.md) · [audit](superagent-audit.md) · plan `.harness/plans/active/superagent-bus.md` | none |
| T2 | Watchdog LLM: so tiến độ với `Goal` của brief | 3.5 | draft | [superagent-watchdog-llm.md](superagent-watchdog-llm.md) | T1 P1 (Source `watchdog` ghi trigger), T1 P3 (R8) |
| T3 | PR comment/review làm Source | 3.1, 3.2 | draft | [superagent-pr-source.md](superagent-pr-source.md) | T1 P5 (`source_state`, poll, thread = branch) |
| T4 | Quy chuẩn trả lời theo kênh | 3.6 | draft | [superagent-channel-norms.md](superagent-channel-norms.md) | T3 (kênh đầu tiên mà agent phải trả lời ra ngoài terminal) |
| T5 | Speculative CI auto-fix | 4 | draft | [superagent-speculative-fix.md](superagent-speculative-fix.md) | T1 P5 (trigger `ci_failed`), T1 P4 (recall) |
| T6 | Cloud, BYOM, bỏ tầng Lead | 4 | parked | [superagent-parked.md](superagent-parked.md) | ngoài phạm vi máy đơn |

Backlog nhỏ của T1 (chưa thành track riêng): retention/pruning bảng `triggers` (NG7 của plan bus).

## Thứ tự đề xuất

1. T1 theo plan bus, P0 → P5.
2. T2 có thể brainstorm song song với T1 P3, vì nó chỉ cần Source `watchdog` và R8.
3. T3 sau T1 P5, rồi tới T4 ngay sau T3, vì norms chỉ có giá trị khi có kênh thứ hai.
4. T5 sau T3. Nó dùng chung đường CI và cần biết owner của branch.
5. T6 chỉ mở lại khi quaqua bỏ giả định một máy.

## Cách mở một track

1. `/brainstorm` trên file con, chốt câu hỏi ở mục "Câu hỏi cho brainstorm".
2. Lock thành `.harness/plans/active/superagent-<track>.md` khi không còn plan active nào khác.
3. `to-plan` nếu là high-risk, rồi `work full`.
4. Cập nhật cột Status ở bảng trên và dòng `plan:` trong `.harness/PROJECT.md`.

## Quyết định đang có hiệu lực

Các quyết định 1–10 trong [superagent-audit.md §6](superagent-audit.md) vẫn áp cho T1. Quyết định 6 (watchdog LLM là non-goal) và 8 (GitHub chỉ lấy CI fail) chỉ áp cho **đợt T1**. Ngày 2026-11-02 anh iu đã đưa T2–T6 vào chương trình để brainstorm sau.
