# T6: Parked (cloud, BYOM, bỏ tầng Lead)

Status: **parked**. Index: [superagent.md](superagent.md). Nguồn: spec gốc §4 ([superagent-source-spec.md](superagent-source-spec.md)), NG1–NG2 của plan bus.
Baseline: `main` @ `cde0394`.

Các ý này được ghi lại để chương trình phủ đủ spec. Spec gốc đã chủ động bỏ qua chúng. Mở lại chỉ khi điều kiện ở cột cuối thay đổi.

| Ý tưởng | Lý do đang park (spec gốc §4) | Điều kiện mở lại |
|---|---|---|
| Tách compute và state (cloud loop stateless) | quaqua cố ý giữ state cục bộ và gitignore (`README.md:10-12`), CoS chạy trong pane Herdr. Lên cloud là viết lại hoàn toàn (effort L). | quaqua cần chạy trên nhiều máy hoặc khi laptop tắt. |
| BYOM daemon | Chỉ có ích khi đã có loop trên cloud. Trên một máy, worktree local đã là execution env. Bài viết gốc §8.4 nêu rủi ro bảo mật khi nối cloud với máy đầu cuối. | Sau khi mở lại dòng trên. |
| Ép về đúng hai tầng (bỏ Lead) | Lead phục vụ pipeline nhiều ngày và chặn self-review (`templates/workspace/AGENTS.md:42`). Bài viết gốc cũng nói mỗi tầng có thể có cấu trúc nội bộ riêng. | Khi T1 R6 (subscription của Lead) cho thấy Lead chỉ chuyển tiếp tin mà không quyết định gì. Cần đo trước. |

## Câu hỏi cho brainstorm (khi mở lại)

- Phần nào của bus T1 phải đổi nếu `runs/quaqua.db` không còn ở local?
- Có số đo nào về thời gian Lead chỉ chuyển tiếp tin, so với thời gian Lead quyết định?
