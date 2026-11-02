# T2: Watchdog LLM

Status: **draft**, chưa brainstorm. Index: [superagent.md](superagent.md). Nguồn: spec gốc §3.5 ([superagent-source-spec.md](superagent-source-spec.md)).
Baseline: `main` @ `cde0394`.

## Là gì

Thêm một bước đánh giá bằng model. Bước này so diff và tiến độ của worker với `Goal:` trong brief, rồi báo khi worker vẫn sửa file nhưng lệch mục tiêu. Giai đoạn đầu chỉ cảnh báo CoS hoặc Lead, chưa được tự sửa.

## Vì sao

- Watchdog hiện tại là tất định. Nó trả exit 0/2/3/4/5/6 dựa trên mtime của report, diff hash, idle và budget (`crates/quaqua/src/worker/prompt/workflow.rs:3-14`). Nó không biết diff có đúng mục tiêu hay không.
- Auditor có pre-check tất định (`crates/quaqua/src/auditor.rs:527` `run_deterministic_prechecks`), nhưng chỉ đọc và chạy theo lịch, không chạy trong lúc worker làm.
- Lỗi được nhắm tới: worker có diff đều đặn, nên watchdog thấy "đang làm", nhưng nó đang làm sai việc. Hiện chỉ phát hiện được khi CoS đọc report. Đây là suy luận, chưa có số đo.

## Phụ thuộc

- T1 P1: cảnh báo drift đi ra dưới dạng trigger của Source `watchdog`, không thêm kênh mới.
- T1 P3: R8 tách `waiting` khỏi stall. Nếu thiếu R8, tín hiệu drift sẽ lẫn với false stall của Lead.

## Điểm chạm code (tại `cde0394`)

- `crates/quaqua/src/worker/prompt/watchdog.rs` và `workflow.rs`: vòng lặp watchdog.
- `crates/quaqua/src/worker/prompt/verdict.rs:190` `record_verdict_on_acceptance`: chỗ spec gốc gợi ý gắn bước LLM.
- `crates/quaqua/src/worker/prompt/brief.rs`: parse brief, gồm `Goal:`.
- `crates/quaqua/src/auditor.rs:527`: phương án đặt tín hiệu ở Auditor.

## Câu hỏi cho brainstorm

- Đặt tín hiệu ở `verdict.rs` (lúc nghiệm thu), ở watchdog (trong lúc chạy), hay ở Auditor?
- Model nào, và tính tiền vào budget của brief hay budget riêng? Cấu hình nằm ở `models.md`/`team.toml`, không hard-code.
- Gọi bao lâu một lần? Theo số dòng diff, theo thời gian, hay chỉ ở checkpoint?
- Định dạng cảnh báo: một trigger `drift_suspected` có `summary≤80` và spill ra report?
- Ngưỡng nào thì được phép tự sửa (ví dụ gửi `--resume` kèm nhắc Goal)? Hay giữ chỉ cảnh báo vĩnh viễn?
- Đo bằng gì: tỉ lệ cảnh báo đúng trên một bộ run cũ trong `raw/`?

## Ngoài phạm vi track này

Speculative fix (T5). Watchdog không bao giờ tự commit hay push.
