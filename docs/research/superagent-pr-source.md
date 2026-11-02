# T3: PR comment/review làm Source

Status: **draft**, chưa brainstorm. Index: [superagent.md](superagent.md). Nguồn: spec gốc §3.1–3.2 ([superagent-source-spec.md](superagent-source-spec.md)); audit F2 ([superagent-audit.md](superagent-audit.md)).
Baseline: `main` @ `cde0394`.

## Là gì

Thêm Source `github-pr`. Nó biến comment, review và review comment trên PR của branch `quaqua/*` thành trigger trên thread của branch đó. Owner của thread nhận trigger qua digest và wake của T1, giống `ci_failed`.

## Vì sao

- Spec gốc coi GitHub là nguồn sự kiện chính. Audit F2 đo được rằng 15 PR #173–#194 có tổng 3 comment và 0 review. Vì vậy đợt T1 chỉ lấy CI fail (quyết định 8).
- Track này giữ chỗ cho lúc traffic review có thật, ví dụ khi có reviewer ngoài hoặc khi PR được review bởi taskforce chạy trên GitHub.

## Phụ thuộc

- T1 P5: tái dùng `source_state`, khung `quaqua source <name> poll`, systemd user timer, và quy tắc thread = branch.
- T1 P1: thread `quaqua/<name>` tồn tại sau `worker cleanup`, nên comment tới muộn vẫn có owner.

## Điểm chạm code (tại `cde0394`)

- `crates/quaqua/src/gate/policy.rs:27`: boundary `gh pr view --json body,comments` đã có ở gate, nhưng chỉ dùng để chặn merge. Đây là ví dụ cách gọi `gh` sau một trait.
- Source `github-ci` của T1 P5 (chưa có code): mẫu để copy.

## Câu hỏi cho brainstorm

- Lấy những loại nào: issue comment, review, review comment, review request? Bỏ comment của chính bot quaqua?
- Ngưỡng traffic nào thì mở track này? Ví dụ ≥N comment/tuần trong 2 tuần liên tiếp.
- Dedupe theo id của comment, và đọc tiếp từ `updated_at` hay theo cursor của REST API?
- Ai trả lời trên GitHub? Gửi comment ra ngoài cần anh iu duyệt (AGENTS.md). Agent chỉ soạn nháp, hay được phép trả lời theo norms của T4?
- Webhook thay cho poll? Không có server công khai, nên mặc định vẫn là poll.

## Ngoài phạm vi track này

Tự fix theo review (thuộc T5 nếu mở rộng). Trả lời ra GitHub khi chưa có T4.
