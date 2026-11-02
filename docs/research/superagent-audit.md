# Audit: spec Super-agent (DeepSeek Harness) đối chiếu quaqua engine

Status: **final** (2026-11-02, CoS + anh iu, brainstorm grill 3 vòng).
Baseline: engine `main` @ `a189d30`. Spec gốc ghim `4f1acbf` (2 commit trước). Integ `quaqua/quaqua-rs-integ` (#197–#200 + fix #198) chưa merge: nó thêm auditor precheck đọc `*-lead.inbox.md` (#199), ảnh hưởng R2.
Đi kèm: [superagent-spec.md](superagent-spec.md) (thiết kế) · plan `.harness/plans/active/superagent-bus.md`.
Prior art đã đọc: prototype `deepseek-ai/dsh-superagent-prototype@6337fc8` docs 001-domain, 002-authorization, 003-execution, `src/policy.ts`.

## 1. Kết luận

Spec đúng về line ref nhưng sai trọng tâm. Traffic thật của quaqua nằm ở **bus nội bộ** (mail, incident, report), không phải GitHub. Bus đó đang có 3 kênh trùng, ack không ai dùng, và không đánh thức được agent idle. Vì vậy đợt này áp domain model của prototype (Source, Trigger, Thread, Subscription) vào bus nội bộ trước, thay bảng cũ, rồi mới thêm GitHub CI như một Source.

## 2. Kiểm chứng claim của spec

| Claim | Kết quả | Bằng chứng |
|---|---|---|
| `handle_event_hook` ở `bus/events.rs#L882` | Đúng | `grep -n "fn handle_event_hook"` → :882 |
| Lọc nhiễu tất định ở `bus/events.rs#L100` | Đúng | `is_consumer_own_pane_blocked_noise` quanh :97 |
| `InboxDigest` có cursor riêng (`#L12`) | Đúng | struct :12, bảng `cursors(consumer, seq, mail_id)` `bus/mod.rs:138` |
| `send_mail` / `read_unacked_mail_since` / `ack_mail` ở mail.rs 589/808/912 | Đúng | grep khớp cả 3 dòng |
| Chỉ có sự kiện Herdr | Đúng | `handle_event_hook` chỉ xử lý `pane.*`; không có code gọi `gh api` để đọc sự kiện (`gh` chỉ xuất hiện ở `gate/policy.rs` để chặn lệnh ghi) |
| Pass-through giảm 37% token | Không áp dụng được | Số đo trên workload tổng hợp nhiều traffic; quaqua gần như không có traffic PR (F2) |
| Đo được phần token CoS chuyển tiếp mail (prompt 7) | Không đo được | `quaqua usage` chỉ audit OpenCode; CoS chạy Claude Code |
| "Worker sở hữu PR thì subscribe Thread" | Tiền đề sai | Peer bị dọn ngay sau verify, push cần anh iu duyệt; lúc PR có CI thì Peer đã không còn (F5) |

## 3. Phát hiện (đo ngày 2026-11-02)

| # | Phát hiện | Bằng chứng | Hệ quả |
|---|---|---|---|
| F1 | Ack là gánh nặng chết | `mail`: 106 dòng, 100 gửi `cos`, 93 chưa ack; cursor `cos` ở `mail_id=104` khi max id 106 → đã đọc nhưng không ack | Bỏ ack, chỉ giữ cursor (R2) |
| F2 | GitHub gần như không có traffic | 15 PR gần nhất (#173–#194): tổng 3 comment, 0 review, đa số merge trong ngày; CI duy nhất là `.github/workflows/test.yml` | GitHub chỉ lấy CI fail (R7); comment/review là follow-up |
| F3 | 3 kênh trùng cho một tin | `send_mail` ghi DB **và** append `<to>.inbox.md` cho Lead (`bus/mail.rs:440`, :465); kết quả worker còn ở `runs/<name>.md` | Một kênh: trigger trong DB, chi tiết spill ra report (R2, R4) |
| F4 | Mail không đánh thức agent | `send_mail` chỉ ghi DB + `notification_show` desktop (mail.rs ~:580); không chạm pane. Quan sát: Lead đứng "Waiting for sude-fix198" không đọc mail 106, CoS phải `herdr pane send-text` tay | Nudge khi pane chuyển idle (R3) |
| F5 | Thứ sống lâu là branch và Lead, không phải Peer | `worker cleanup` sau verify (AGENTS.md); owner ghi ở `runs/<name>.owner` (`state.rs:27`) | Thread = branch, subscriber = owner (R6) |
| F6 | Watchdog báo stall khi Lead chỉ đang chờ Peer | `WATCHDOG exit=3 stall: no worktree diff or session write for 1381s | name=quaqua-rs-lead` (`worker/prompt/mod.rs:1347`) lúc `tieude-fix198` vẫn chạy | Lead chờ trên subscription còn sống = waiting (R8) |
| F7 | Test có thể vẫn ghi DB thật | Dòng "Test hermeticity" trong `.harness/plans/active/remaining-work-2026-10-25.md` chưa DONE | Đổi schema DB cần R0 trước |
| F8 | Nhiễu incident lớn | 14 ngày: `blocked` 172 (168 từ actor `herdr`), `spawn_failed` 27, `would_deny` 53 (29 từ `llmhub-lead`) | Fast Path ở Source phải lọc trước khi tới digest; `spawn_failed` là follow-up riêng |

Lệnh tái đo:

```bash
sqlite3 -readonly ~/quaqua/runs/quaqua.db "select count(*), sum(to_name='cos'), sum(acked_ms is null) from mail; select consumer, seq, mail_id from cursors;"
sqlite3 -readonly ~/quaqua/runs/quaqua.db "select type, count(*) from farm_events where ts_ms > (strftime('%s','now')-14*86400)*1000 group by 1 order by 2 desc;"
gh pr list --state all --limit 15 --json number,comments,reviews --jq '.[] | "\(.number) c=\(.comments|length) r=\(.reviews|length)"'
```

## 4. Map domain prototype sang quaqua

| Prototype | quaqua hiện có | Đợt này |
|---|---|---|
| Source | Herdr hook (`handle_event_hook`), `send_mail`, watchdog | Source có tên: `herdr`, `mail`, `watchdog`, `github-ci` |
| Trigger | `farm_events` + `mail` (2 bảng) | Bảng `triggers` duy nhất |
| Thread | Không có | `threads`, key = branch (hoặc `agent:<name>` cho tin không gắn branch) |
| Subscription | Không có | `subscriptions(thread, consumer, until_ms)` |
| Cursor / read position | `cursors(seq, mail_id)` | `cursors(consumer, trigger_id)` |
| Eval (gate + assessment) | Fast Path: `is_consumer_own_pane_blocked_noise`; Slow Path: CoS đọc digest | Fast Path chuẩn hoá theo Source; Slow Path vẫn là CoS/Lead |
| Ticket / Assignment | Worker run + brief, `runs/<name>.md` | Không mô hình lại (đã có) |
| Mandate / Policy version | Brief budget, approval gate, `gate/policy.rs` | Không mô hình lại |
| Channel | Terminal Herdr | Không đổi |

## 5. Ý tưởng của spec: quyết định

| Spec § | Ý tưởng | Quyết định |
|---|---|---|
| 3.1 | Trigger lọc 2 tầng | Làm, nhưng cho Source nội bộ trước; GitHub chỉ CI fail (R7) |
| 3.2 | Thread subscription, pass-through | Làm; Thread = branch, subscriber = owner, sống qua Peer cleanup, hết hạn 14 ngày (R6) |
| 3.3 | Inline & Spill, Chain of Recall | Làm (R4, R5) |
| 3.4 | Router batching | Làm như một phần digest (R4) |
| 3.5 | Watchdog LLM | Non-goal đợt này; chỉ sửa false stall tất định (R8) |
| 3.6 | Quy chuẩn trả lời theo kênh | Non-goal (chỉ có terminal) |
| 4 | Cloud, BYOM, speculative, bỏ Lead | Non-goal, giữ lý do của spec |
| 7.5 | Foreground >5s chuyển background | Non-goal; `quaqua worker prompt` đã chạy nền |

## 6. Quyết định của anh iu (2026-11-02)

1. Bus nội bộ trước, GitHub sau.
2. Tái mô hình và xoá bảng/kênh cũ cùng wave, không thêm bảng đứng cạnh.
3. Đóng plan `remaining-work-2026-10-25.md`, dòng hermeticity thành P0.
4. Wake = nudge khi pane chuyển idle; agent bận thì xếp hàng.
5. Bỏ ack, chỉ giữ cursor.
6. Watchdog LLM là non-goal.
7. CI fail đi về owner của branch.
8. GitHub chỉ lấy CI fail.
9. Giữ R8 (false stall của Lead).
10. Bộ file ở engine repo, branch `quaqua/cos-superagent-plan`, không push.

## 7. Follow-up ngoài đợt

- `spawn_failed` 27 lần/14 ngày: điều tra riêng (bug-fix).
- PR comment/review làm Source khi traffic có thật.
- Watchdog LLM so diff với `Goal` của brief.
- Đo token CoS (Claude Code) để biết pass-through tiết kiệm bao nhiêu.
