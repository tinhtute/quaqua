# Spec: superagent bus (Source → Trigger → Thread → Subscription)

Status: **locked** (2026-11-02). Plan thực thi: `.harness/plans/active/superagent-bus.md`. Bằng chứng và quyết định: [superagent-audit.md](superagent-audit.md).
Baseline: `main` @ `a189d30`. Mọi `file:line` dưới đây đúng tại baseline đó.

## 1. Vì sao

Bus hiện tại có hai bảng sự kiện (`farm_events`, `mail`), ba kênh cho một tin (DB, `<lead>.inbox.md`, `runs/<name>.md`), một cơ chế ack không ai dùng (F1), và không đánh thức agent idle (F4). Spec này thay tất cả bằng một mô hình duy nhất, lấy từ domain của prototype DeepSeek: **Source** sinh **Trigger**, Trigger thuộc một **Thread**, consumer theo dõi Thread qua **Subscription** và đọc bằng **cursor**. Chi tiết dài nằm ngoài digest (spill) và được lấy lại bằng `quaqua recall`.

Người dùng của thay đổi: CoS và Lead. Họ nhận mỗi tin đúng một lần, ở một chỗ, và được nhắc khi rảnh. Người bảo trì nhận một bảng, một đường ghi, một đường đọc.

## 2. Thuật ngữ (một từ cho một khái niệm)

| Từ | Nghĩa trong quaqua |
|---|---|
| Source | Nơi sinh sự kiện: `herdr` (hook pane), `mail` (`quaqua mail send`), `watchdog` (`worker prompt`), `worker` (report viết xong, spawn, cleanup), `github-ci` |
| Trigger | Một dòng trong `triggers`. Bất biến sau khi ghi |
| Thread | Chuỗi trigger về cùng một việc. Key = tên branch (`quaqua/<name>`); tin không gắn branch dùng `agent:<to_name>` |
| Owner | Agent chịu trách nhiệm Thread: `runs/<name>.owner` nếu có (`state.rs:27`), không thì người spawn |
| Subscription | `(thread_id, consumer, until_ms)`. Consumer nhận trigger của Thread tới `until_ms` |
| Cursor | `(consumer, trigger_id)`. Vị trí đã đọc. Thay toàn bộ ack |
| Fast Path | Lọc tất định tại Source, trước khi ghi trigger |
| Digest | Bản render một batch trigger chưa đọc cho một consumer |
| Spill | Nội dung dài nằm ở file (`runs/<name>.md`, `raw/…`) hoặc `payload_json`; digest chỉ giữ `ref` |
| Recall | Đi từ một trigger sang chi tiết, tối đa 2 bước |

Không mô hình lại: Ticket, Assignment (đã là worker run + brief), Mandate, Policy version (đã là brief budget + `gate/policy.rs`), Eval assessment (vẫn là CoS/Lead đọc digest), Channel (chỉ có terminal).

## 3. Schema

Trong `runs/quaqua.db` (WAL), thay `farm_events`, `mail`, `cursors` cũ.

```sql
CREATE TABLE triggers (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  ts_ms        INTEGER NOT NULL,
  source       TEXT NOT NULL CHECK (source IN ('herdr','mail','watchdog','worker','github-ci')),
  kind         TEXT NOT NULL,          -- blocked, stalled, report_written, directive, ci_failed, ...
  actor        TEXT NOT NULL,          -- ai gây ra (agent name, 'herdr', 'github')
  to_name      TEXT,                   -- người nhận trực tiếp (mail); NULL = theo subscription
  thread_id    TEXT NOT NULL REFERENCES threads(id),
  summary      TEXT NOT NULL CHECK (length(summary) <= 80),
  ref          TEXT,                   -- path spill hoặc URL; NULL nếu summary đủ
  payload_json TEXT,                   -- tối đa 4096 byte, giữ thân mail cũ
  dedupe_key   TEXT UNIQUE,            -- thay incident_key; ví dụ 'gh-run:<run_id>'
  farm         TEXT NOT NULL
);
CREATE INDEX triggers_thread ON triggers(thread_id, id);

CREATE TABLE threads (
  id              TEXT PRIMARY KEY,    -- 'quaqua/<branch>' hoặc 'agent:<name>'
  owner           TEXT NOT NULL,
  state           TEXT NOT NULL CHECK (state IN ('open','closed')),
  last_trigger_ms INTEGER NOT NULL
);

CREATE TABLE subscriptions (
  thread_id TEXT NOT NULL REFERENCES threads(id),
  consumer  TEXT NOT NULL,
  until_ms  INTEGER NOT NULL,
  PRIMARY KEY (thread_id, consumer)
);

CREATE TABLE cursors (
  consumer   TEXT PRIMARY KEY,
  trigger_id INTEGER NOT NULL
);

CREATE TABLE source_state (            -- vị trí đọc của Source poll (github-ci)
  source TEXT PRIMARY KEY,
  value  TEXT NOT NULL
);
```

Quy tắc:

- `triggers` chỉ insert. Không update, không delete trong vận hành thường. Retention là follow-up.
- Ghi trigger và cập nhật `threads.last_trigger_ms` trong cùng một transaction.
- `dedupe_key` trùng → `INSERT OR IGNORE`, coi là thành công (idempotent).
- Gói `bus_migration` giữ nguyên làm bảng version.

## 4. Migration (R1)

Một lệnh: `quaqua bus migrate` (đã có hay chưa, worker xác nhận ở P1; nếu chưa có thì migration chạy trong `open_bus` như hiện tại và chỉ có thêm cờ `--dry-run`).

1. Kiểm tra không có worker live (`herdr agent list`). Có → từ chối, exit khác 0.
2. Sao lưu: `VACUUM INTO 'raw/<date>-quaqua-db-pre-superagent.db'`.
3. Tạo bảng mới. Chép:
   - `farm_events` → `triggers` (`source` suy từ `actor`/`type`; `thread_id` = `agent:<subject|owner>`; `summary` = 80 ký tự đầu của type + subject; `incident_key` → `dedupe_key`).
   - `mail` → `triggers` (`source='mail'`, `to_name`, thân → `payload_json`, `summary` = dòng đầu ≤80).
   - `cursors(consumer, seq, mail_id)` → `cursors(consumer, trigger_id)` = id mới lớn nhất đã map từ max(seq, mail_id) của consumer đó.
4. Drop `farm_events`, `mail`, `cursors` cũ. Ghi version vào `bus_migration`.
5. Chạy lần hai là no-op.

`--dry-run` chạy trên bản copy và in số dòng trước/sau từng bảng. Đổi tên cột hay xoá bảng phải xong trong cùng wave với việc chuyển caller (Migrate Callers Then Delete Legacy APIs).

## 5. CLI contract

| Lệnh | Trạng thái | Hành vi |
|---|---|---|
| `quaqua mail send --to <n> --kind <k> --body-file -` | giữ | Ghi một trigger `source='mail'`. Không ghi `<to>.inbox.md`. Sau khi ghi gọi wake (§7) |
| `quaqua mail inbox` | giữ, đổi nguồn | Render digest (§6) từ cursor của caller, rồi đẩy cursor tới trigger cuối đã in |
| `quaqua mail ack` | **xoá** | Không có alias. Template và rule ngừng nhắc tới |
| `quaqua recall <id>` | **mới**, seam cần anh iu duyệt | In trigger `<id>`, Thread của nó, và đối tượng `ref` (§8) |
| `quaqua source github-ci poll` | **mới**, seam cần anh iu duyệt | Một lần poll (§9). Chạy bằng systemd user timer, cùng kiểu auditor |
| `quaqua bus migrate [--dry-run]` | mới hoặc cờ mới | §4 |

Exit code: 0 thành công, 1 lỗi dùng sai, 2 lỗi DB. Output digest là text thuần một dòng mỗi trigger. `--json` cho `inbox` và `recall` in đúng các cột của bảng.

## 6. Digest: Inline & Spill, batching (R4)

Mỗi dòng:

```
#<id> <kind> <actor> <thread> <summary≤80> → <ref>
```

- Bỏ `→ <ref>` khi `ref` NULL.
- Một lần render = một batch: mọi trigger từ `cursor+1` tới id lớn nhất hiện có, gộp theo Thread, Thread có trigger mới nhất lên trước.
- Trong một Thread, các trigger cùng `kind` liên tiếp gộp thành một dòng `#<first>..#<last> <kind> ×N …`.
- Giới hạn 20 dòng. Vượt → dòng cuối `… +N more: quaqua mail inbox --all`.
- Hook UserPromptSubmit của CoS (`templates/workspace/scripts/hooks/cos-inbox.sh`) gọi đúng renderer này; không giữ định dạng riêng.

## 7. Wake (R3)

Nguồn sự kiện: `handle_event_hook` (`bus/events.rs:882`) đã nhận `pane.agent_status_changed`.

Thuật toán, chạy trong hook và ngay sau `send_mail`:

1. Xác định consumer của pane (tên agent).
2. Nếu agent không idle → không làm gì. Trigger chờ trong DB.
3. Nếu idle và có trigger chưa đọc (id > cursor, thuộc `to_name = consumer` hoặc Thread consumer đang subscribe) → gửi **một dòng** vào pane: `quaqua: <N> new (#<min>..#<max>), run quaqua mail inbox`, rồi Enter.
4. Ghi `last_nudge_trigger_id` cho consumer (cột trong `cursors` hoặc `source_state`). Không nudge lại nếu không có trigger nào mới hơn giá trị đó. Tối đa một nudge mỗi batch.
5. CoS là ngoại lệ: CoS đã có hook UserPromptSubmit, chỉ nudge khi CoS idle quá 60 giây.

Không bao giờ gõ vào pane đang busy. Không dùng desktop notification làm kênh chính (có thể giữ như phụ).

## 8. Recall (R5)

`quaqua recall <id>`:

1. Trigger `<id>` đầy đủ, gồm `payload_json`.
2. Thread: owner, state, 5 trigger gần nhất.
3. `ref`: file trong workspace thì in đường dẫn và 40 dòng đầu; nếu là report `runs/<name>.md` thì thêm commit SHA ghi trong report (nếu có).

Tối đa 2 bước (trigger → thread/ref → commit). Không theo link ra ngoài.

## 9. Subscription và GitHub CI Source (R6, R7)

Subscription:

- `worker spawn` mở Thread `quaqua/<name>`, owner = người spawn (CoS hoặc Lead), subscribe owner với `until_ms = now + 14 ngày`.
- `worker cleanup` **không** đóng Thread và không xoá subscription. Branch còn sống thì Thread còn sống.
- Mỗi trigger mới trên Thread gia hạn `until_ms = ts_ms + 14 ngày` cho mọi subscriber.
- Lead nhận Peer → Lead là owner; khi Lead merge vào `quaqua/<slug>-integ`, Thread của integ có owner là Lead, CoS subscribe thêm.

GitHub CI Source (`quaqua source github-ci poll`):

- `gh run list --json databaseId,headBranch,conclusion,url,workflowName --limit 50` cho repo engine.
- Chỉ nhận `conclusion == "failure"` trên `headBranch` bắt đầu bằng `quaqua/` **và** đã có Thread. Mọi thứ khác bị lọc ở Fast Path, không ghi trigger.
- `dedupe_key = 'gh-run:<databaseId>'`. Vị trí đọc (id run lớn nhất đã xem) lưu ở `source_state`.
- Trigger: `source='github-ci'`, `kind='ci_failed'`, `actor='github'`, `summary` = `<workflowName> failed on <branch>`, `ref` = URL run. Gửi tới owner của Thread qua subscription, sau đó chạy wake.
- `gh` lỗi mạng → exit 0, ghi log, không đẩy vị trí đọc (lần sau đọc lại). Đây là đọc, không phải ghi; không chạm `gate/policy.rs`.
- Không dùng webhook. Chu kỳ timer: mặc định 5 phút, anh iu chỉnh trong unit.

## 10. Watchdog: Lead đang chờ (R8)

Ở `worker/prompt/mod.rs:1347` (`stall: no worktree diff or session write for {quiet}s`): trước khi báo stall cho agent X, kiểm tra X có subscription còn hạn trên một Thread mà owner Peer của nó còn live (`herdr agent list`) hoặc có trigger trong cửa sổ quiet. Có → trạng thái `waiting`, không exit 3, không ghi trigger `stalled`. Không → giữ hành vi cũ.

## 11. Bỏ đi trong đợt này

- `<lead>.inbox.md` và mọi chỗ ghi/đọc nó: `bus/mail.rs:440`, `:465`; drift check trong `doctor.rs` (~1095–1129); `templates/workspace/rules/_all/lead-inbox.md`; phần nói về inbox.md trong `templates/workspace/AGENTS.md`.
- Auditor precheck của #199 (đọc `runs/*-lead.inbox.md`) nếu integ đã merge: đổi sang đọc trigger chưa đọc của Lead từ DB.
- Cột `deliveries`, `last_delivered_ms`, `acked_ms`, `acked_by`; sự kiện `mail_unacked`, `mail_acked`.
- `read_unacked_mail_since` (`mail.rs:808`), `ack_mail` (`mail.rs:912`).

## 12. Không làm

Cloud, BYOM, bỏ tầng Lead, watchdog LLM, auto-fix CI, chuẩn trả lời theo kênh, PR comment/review làm Source, retention của `triggers`.

## 13. Câu hỏi mở cho phiên thực thi

- `open_question:` `quaqua bus migrate` đã tồn tại chưa, hay migration chạy ngầm khi mở DB? | owner: worker P1 (tự tra code, không phải quyết định).
- `open_question:` chu kỳ poll CI 5 phút có ổn không? | owner: anh iu (không chặn requirement).
