# T4: Quy chuẩn trả lời theo kênh

Status: **draft**, chưa brainstorm. Index: [superagent.md](superagent.md). Nguồn: spec gốc §3.6 ([superagent-source-spec.md](superagent-source-spec.md)).
Baseline: `main` @ `cde0394`.

## Là gì

Mỗi kênh có một quy chuẩn trả lời. Trên terminal và IM thì trả lời ngắn (bài viết gốc dùng trần 100 chữ). Trên GitHub, email và tài liệu thì trả lời có cấu trúc. Agent chọn quy chuẩn theo Source của trigger mà nó đang trả lời.

## Vì sao

- Hiện quaqua chỉ có một kênh là terminal Herdr, nên ý này chưa có giá trị (spec gốc §3.6, ma trận §3.7 xếp "Thấp (lúc này)").
- Giá trị xuất hiện khi agent phải trả lời trên một kênh thứ hai, sớm nhất là GitHub ở T3.

## Phụ thuộc

- T3: kênh đầu tiên mà agent trả lời ra ngoài terminal.
- T1 P1: Source của trigger cho biết kênh.

## Điểm chạm (tại `cde0394`)

- `templates/workspace/rules/_all/`: spec gốc gợi ý thêm `channels.md` ở đây. Việc sửa `rules/` là task của worker.

## Câu hỏi cho brainstorm

- Một file `rules/_all/channels.md`, hay gắn norms vào từng Source?
- Kiểm tra bằng gì: lint độ dài khi soạn comment GitHub, hay chỉ là rule cho agent đọc?
- Ngôn ngữ trên từng kênh: tiếng Việt ở terminal, tiếng Anh trên GitHub?

## Ngoài phạm vi track này

Thêm kênh IM hoặc email. Chưa có Source nào cho chúng.
