# Spec: Đưa ý tưởng Super-agent (DeepSeek Harness) vào quaqua

> Tài liệu để brainstorm với Claude Code. Mọi link code được ghim theo commit:
> - quaqua `4f1acbf609775c0fb896ac5f256b3ba9be3cf347` → `Q = https://github.com/tinhtute/quaqua/blob/4f1acbf609775c0fb896ac5f256b3ba9be3cf347`
> - dsh-superagent-prototype `6337fc823ce20251c8de8085948165c81aaa701e` → `P = https://github.com/deepseek-ai/dsh-superagent-prototype/blob/6337fc823ce20251c8de8085948165c81aaa701e`
>
> Quy ước: **[fact]** = lấy từ code hoặc bài viết; **[suy luận]** = đánh giá của người viết spec.

## 1. Context

### 1.1 quaqua là gì
- Bộ setup multi-agent cá nhân theo **SLP (Supervisor – Lead – Peer)** trên [Herdr](https://herdr.dev). Principal (chủ repo) chỉ nói chuyện với CoS "Quả Quả" chạy trên Claude Code. [README L1-L8](https://github.com/tinhtute/quaqua/blob/4f1acbf609775c0fb896ac5f256b3ba9be3cf347/README.md#L1-L8)
- Kiến trúc: Principal → CoS → Lead (`<slug>-lead`) → Peers (build ⇄ review, mỗi Peer một worktree), cộng một Auditor read-only chạy 2 lần mỗi ngày qua systemd hoặc launchd. [README L18-L40](https://github.com/tinhtute/quaqua/blob/4f1acbf609775c0fb896ac5f256b3ba9be3cf347/README.md#L18-L40)
- Stack: Rust (`crates/quaqua`), SQLite WAL (`runs/quaqua.db`), plugin `herdr-quaqua`. Worker chạy trên Claude Code, omp, opencode và pi, với các model như `deepseek-v4.1-flash` và `opus-5.5`.
- Module: `worker/` (spawn, prompt, watchdog, stop), `bus/` (events, mail), `engine/` (workflow journal), `lead/`, `pipeline/`, `grid/`, `gate/`, `auditor.rs`, `liveness.rs`, `cos.rs`, `harness/`.
- Entry point: `quaqua init`, `quaqua worker spawn`, `quaqua worker prompt`, `quaqua worker stop`, `quaqua status`, `quaqua auditor run`.
- Repo: MIT, 0 sao, tạo ngày 7/10/2026, commit mới nhất `4f1acbf` lúc 10:04 ngày 2/11/2026 (UTC+7).
- Repo không có liên hệ nào với DSH hay dsh-superagent-prototype. Chữ "deepseek" chỉ xuất hiện trong tên model. **[fact, grep]**

### 1.2 Super-agent là gì
Bài "Exploring Super-agent Design: Towards a More Autonomous Agent System" của DeepSeek Harness team, do Yujia Qiao viết. Bản đọc được: [mirror](https://haowenji.com/articles/x-2116829218935857152); link X gốc chưa fetch được.
- 4 đặc tính: **Continuous, Productive, Proactive, Persistent**.
- 5 module:
  1. Hai tầng Router/Worker. Router không thực thi; Worker có context cô lập và không trả lời người dùng.
  2. Event-driven Triggers: lọc hai tầng (code rồi mới tới LLM), Thread subscription và pass-through, Inline & Spill cùng Chain of Recall, speculative work.
  3. Full-duplex: tool async `start()`/`stop()`, và quy chuẩn trả lời riêng cho từng kênh.
  4. Tách compute, state và execution: cloud loop stateless, state trong DB, daemon BYOM.
  5. Watchdog session độc lập.
- Prototype tham khảo: [deepseek-ai/dsh-superagent-prototype](https://github.com/deepseek-ai/dsh-superagent-prototype). README tự nhận đây là research code, và một số thiết kế trong bài chưa được triển khai.

## 2. Goals
1. Làm quaqua **proactive**: các sự kiện ngoài (GitHub PR, review, CI) tự đi vào hệ thống mà không cần Principal gõ.
2. Giảm tải và token cho CoS bằng cách route sự kiện tiếp theo thẳng tới worker sở hữu task.
3. Giúp CoS recall được việc cũ qua ID thay vì đọc lại file dài.
4. Giữ nguyên các thế mạnh hiện có: watchdog tất định, worktree cô lập, approval gate, chạy cục bộ.

Non-goals: chuyển lên cloud, BYOM, bỏ tầng Lead (lý do ở mục 4).

## 3. Các ý tưởng theo thứ tự ưu tiên

Các con số lợi ích lấy từ Mục 7 của bài viết. Đó là thí nghiệm sơ bộ trên **workload tổng hợp**, không đo trên quaqua. Thứ hạng và effort là **[suy luận]**.

### 3.1 Trigger lọc hai tầng (effort M, ưu tiên 1)
- **Là gì:** code lọc sự kiện theo nguồn và allowlist trước (Fast Path), LLM chỉ xét các ứng viên còn lại (Slow Path).
- **Vì sao hợp với quaqua:**
  - Đã có hook nhận sự kiện `handle_event_hook`: [bus/events.rs#L882](https://github.com/tinhtute/quaqua/blob/4f1acbf609775c0fb896ac5f256b3ba9be3cf347/crates/quaqua/src/bus/events.rs#L882).
  - Đã có bộ lọc nhiễu tất định: [bus/events.rs#L100](https://github.com/tinhtute/quaqua/blob/4f1acbf609775c0fb896ac5f256b3ba9be3cf347/crates/quaqua/src/bus/events.rs#L100).
  - Hiện chỉ nhận sự kiện Herdr.
- **Lợi ích:** CI fail hay review comment trên PR của worker tự vào hệ thống. Bài viết không có số liệu riêng cho phần này.
- **Bước đầu tiên:** thêm nguồn `github` (poll `gh api` notifications, hoặc một webhook cục bộ) ghi vào `farm_events`. Policy theo từng nguồn (`respond` / `candidate` / `ignore`) đặt trong `team.toml`.
- **Mẫu tham khảo:** [P/src/sources/github-trigger.ts#L65-L110](https://github.com/deepseek-ai/dsh-superagent-prototype/blob/6337fc823ce20251c8de8085948165c81aaa701e/src/sources/github-trigger.ts#L65-L110), [P/src/policy.ts#L40-L125](https://github.com/deepseek-ai/dsh-superagent-prototype/blob/6337fc823ce20251c8de8085948165c81aaa701e/src/policy.ts#L40-L125).
- **Câu hỏi mở:** chọn poll hay webhook cho một máy cá nhân? Làm sao lọc CI của chính worker để không spam CoS?

### 3.2 Thread subscription và pass-through (effort M, ưu tiên 2)
- **Là gì:** worker sở hữu PR thì subscribe Thread của PR đó, và sự kiện tiếp theo đi thẳng tới worker, không qua CoS.
- **Vì sao hợp với quaqua:**
  - Mail đã có `send_mail` ([bus/mail.rs#L589](https://github.com/tinhtute/quaqua/blob/4f1acbf609775c0fb896ac5f256b3ba9be3cf347/crates/quaqua/src/bus/mail.rs#L589)), `read_unacked_mail_since` ([#L808](https://github.com/tinhtute/quaqua/blob/4f1acbf609775c0fb896ac5f256b3ba9be3cf347/crates/quaqua/src/bus/mail.rs#L808)) và `ack_mail` ([#L912](https://github.com/tinhtute/quaqua/blob/4f1acbf609775c0fb896ac5f256b3ba9be3cf347/crates/quaqua/src/bus/mail.rs#L912)).
  - `InboxDigest` có cursor riêng cho từng người đọc: [bus/events.rs#L12](https://github.com/tinhtute/quaqua/blob/4f1acbf609775c0fb896ac5f256b3ba9be3cf347/crates/quaqua/src/bus/events.rs#L12).
  - Worker đã có worktree và branch riêng.
- **Lợi ích:** bài viết đo được Router ít hơn khoảng 30% quyết định, và token cache-read giảm 37% (31.8M → 23.2M). Đây là số từ workload tổng hợp.
- **Bước đầu tiên:** thêm bảng `subscriptions(thread_id, worker)` vào `runs/quaqua.db`. Sự kiện có thread khớp thì `send_mail` tới worker thay vì CoS.
- **Mẫu tham khảo:** [P/docs/002-authorization.md#L18-L45](https://github.com/deepseek-ai/dsh-superagent-prototype/blob/6337fc823ce20251c8de8085948165c81aaa701e/docs/002-authorization.md#L18-L45), [P/src/core/model.ts#L75](https://github.com/deepseek-ai/dsh-superagent-prototype/blob/6337fc823ce20251c8de8085948165c81aaa701e/src/core/model.ts#L75) (`followupOwners`). Các tool phía worker: `orchestrator_thread_follow`, `orchestrator_thread_unrelated`, `orchestrator_trigger_handback`.
- **Câu hỏi mở:** Thread nên là PR, branch hay worktree? Khi `worker cleanup` thì subscription xử lý thế nào?

### 3.3 Inline & Spill và Chain of Recall (effort S, ưu tiên 3)
- **Là gì:** inline ID kèm tóm tắt ngắn, phần dư spill ra ngoài kèm con trỏ, và recall theo nhiều hop qua đồ thị ID.
- **Vì sao hợp với quaqua:**
  - Memory đã có giới hạn: `memory/rules.md` ≤2200 ký tự, `## Now` ≤5 mục.
  - Đã có trích dẫn dạng `(<sha> · raw/<file>)`: [templates/workspace/AGENTS.md#L20-L28](https://github.com/tinhtute/quaqua/blob/4f1acbf609775c0fb896ac5f256b3ba9be3cf347/templates/workspace/AGENTS.md#L20-L28).
  - Còn thiếu ID liên kết giữa event, worker và report.
- **Lợi ích:** CoS ít phải đọc lại file dài và recall được việc cũ ở phiên sau. Bài viết không có số liệu cho phần này.
- **Bước đầu tiên:** cho `InboxDigest` in ra `event_id`, `worker`, `report` kèm 50 ký tự đầu, rồi thêm lệnh `quaqua recall <id>` trả về các ID ở hop kế tiếp.
- **Câu hỏi mở:** CoS cần những ID nào để recall được report cũ trong tối đa 2 hop?

### 3.4 Router batching (effort S, ưu tiên 4)
- **Là gì:** gộp các trigger tồn đọng vào một lượt suy luận của CoS.
- **Vì sao hợp với quaqua:**
  - CoS đã "drain" ở các điểm wake và cuối wave: [templates/workspace/AGENTS.md#L62](https://github.com/tinhtute/quaqua/blob/4f1acbf609775c0fb896ac5f256b3ba9be3cf347/templates/workspace/AGENTS.md#L62).
  - Digest dùng cursor: [bus/events.rs#L12-L21](https://github.com/tinhtute/quaqua/blob/4f1acbf609775c0fb896ac5f256b3ba9be3cf347/crates/quaqua/src/bus/events.rs#L12-L21).
- **Lợi ích:** bài viết đo được thời gian mỗi quyết định giảm từ 6.4s xuống 1.6s khi tải tăng từ 1 lên 40 trigger/phút. Đây là stress test tổng hợp.
- **Bước đầu tiên:** khi wake, render toàn bộ digest thành một prompt gộp thay vì xử lý từng mail một.
- **Liên quan full-duplex:**
  - quaqua đã chạy `quaqua worker prompt` ở nền và có `worker/stop.rs` để ngắt theo từng harness ([worker/stop.rs#L1-L6](https://github.com/tinhtute/quaqua/blob/4f1acbf609775c0fb896ac5f256b3ba9be3cf347/crates/quaqua/src/worker/stop.rs#L1-L6)).
  - Prototype tự chuyển lệnh shell foreground sang background sau 5 giây: [P/docs/003-execution.md#L27-L47](https://github.com/deepseek-ai/dsh-superagent-prototype/blob/6337fc823ce20251c8de8085948165c81aaa701e/docs/003-execution.md#L27-L47).
- **Câu hỏi mở:** một batch nên chứa tối đa bao nhiêu mục để CoS vẫn xử lý chính xác?

### 3.5 Watchdog dạng agent (effort M, ưu tiên 5)
- **Là gì:** dùng model đánh giá tiến độ và drift so với mục tiêu, có quyền sửa trong giới hạn.
- **Vì sao hợp với quaqua:**
  - Watchdog tất định đã tốt, với các exit 0/2/3/4/5/6: [worker/prompt/workflow.rs#L3-L12](https://github.com/tinhtute/quaqua/blob/4f1acbf609775c0fb896ac5f256b3ba9be3cf347/crates/quaqua/src/worker/prompt/workflow.rs#L3-L12).
  - Auditor có pre-check tất định nhưng chỉ read-only và chạy 2 lần mỗi ngày: [auditor.rs#L151-L160](https://github.com/tinhtute/quaqua/blob/4f1acbf609775c0fb896ac5f256b3ba9be3cf347/crates/quaqua/src/auditor.rs#L151-L160).
- **Lợi ích [suy luận]:** bắt được trường hợp worker vẫn sửa file nhưng lệch mục tiêu, thứ mà diff hash không phát hiện được.
- **Bước đầu tiên:** thêm bước LLM so diff với `Goal` của brief trong `worker/prompt/verdict.rs`. Giai đoạn đầu chỉ cảnh báo CoS, chưa cho tự sửa.
- **Lưu ý [fact]:** prototype không có file nào nhắc tới watchdog.
- **Câu hỏi mở:** tín hiệu drift nên đặt ở `verdict.rs` hay ở Auditor?

### 3.6 Quy chuẩn trả lời theo kênh (effort S, ưu tiên 6)
- **Là gì:** IM trả lời ngắn (bài viết dùng trần 100 chữ); GitHub, email và tài liệu thì có cấu trúc.
- **Vì sao:** hiện quaqua chỉ có một kênh là terminal. Ý này chỉ có giá trị sau khi làm xong 3.1.
- **Bước đầu tiên:** thêm `rules/_all/channels.md` khi bật nguồn đầu tiên.

### 3.7 Ma trận ưu tiên [suy luận, định tính]

| Ý tưởng | Giá trị | Effort | Ô |
|---|---|---|---|
| 3.1 Trigger 2 tầng | Cao | M | Làm ngay |
| 3.2 Thread pass-through | Cao | M | Làm ngay |
| 3.3 Inline & Spill / Recall | Khá cao | S | Làm ngay |
| 3.4 Batching | Trung bình | S | Tiện thì làm |
| 3.5 Watchdog LLM | Trung bình | M | Đầu tư |
| 3.6 Channel norms | Thấp (lúc này) | S | Tiện thì làm |
| Speculative work | Thấp (lúc này) | M–L | Bỏ qua |
| BYOM / cloud | Thấp | L | Bỏ qua |

## 4. Tạm bỏ qua

| Ý tưởng | Lý do |
|---|---|
| Tách compute và state (cloud loop stateless) | quaqua cố ý giữ state cục bộ và gitignore ([README L10-L12](https://github.com/tinhtute/quaqua/blob/4f1acbf609775c0fb896ac5f256b3ba9be3cf347/README.md#L10-L12)), CoS chạy trong pane Herdr. Đưa lên cloud là viết lại hoàn toàn (L) và đi ngược thiết kế harness cá nhân. |
| BYOM daemon | Chỉ có ích khi đã có loop trên cloud. Trên một máy, worktree cục bộ đã là execution env. Bài viết Mục 8.4 cũng nêu rủi ro bảo mật khi nối cloud với thiết bị đầu cuối. |
| Speculative work ("silently reproduce and fix") | Cần có 3.1 trước. Ngoài ra còn tốn token, trong khi quaqua đã có ngân sách thời gian và chi phí cho mỗi brief ([AGENTS.md#L63](https://github.com/tinhtute/quaqua/blob/4f1acbf609775c0fb896ac5f256b3ba9be3cf347/templates/workspace/AGENTS.md#L63)). |
| Ép về đúng hai tầng (bỏ Lead) | Lead phục vụ pipeline nhiều ngày và chặn self-review ([AGENTS.md#L42](https://github.com/tinhtute/quaqua/blob/4f1acbf609775c0fb896ac5f256b3ba9be3cf347/templates/workspace/AGENTS.md#L42)). Bài viết cũng nói mỗi tầng có thể có cấu trúc nội bộ riêng. |

## 5. So sánh từng tiêu chí (hiện trạng)

| Tiêu chí | Super-agent yêu cầu | quaqua hiện có | Đánh giá |
|---|---|---|---|
| Router không thực thi | Router chỉ tương tác với người và dispatch việc, không cần Bash | CoS và Lead "Never edit code or config" ([AGENTS.md#L61](https://github.com/tinhtute/quaqua/blob/4f1acbf609775c0fb896ac5f256b3ba9be3cf347/templates/workspace/AGENTS.md#L61)), nhưng CoS vẫn có Bash và tự merge | Một phần |
| Worker cô lập, không trả lời người dùng | Context riêng, một task mỗi worker, báo cáo lên Router | Worktree riêng, báo cáo qua `runs/<name>.md`, Principal chỉ nói với CoS, tool `Agent` native bị deny ([AGENTS.md#L73-L80](https://github.com/tinhtute/quaqua/blob/4f1acbf609775c0fb896ac5f256b3ba9be3cf347/templates/workspace/AGENTS.md#L73-L80)) | Có |
| Số tầng | Hai tầng logic | Ba tầng: CoS → Lead → Peer | Khác |
| Trigger lọc hai tầng | Code lọc trước, rồi mới hỏi LLM | Không có nguồn sự kiện ngoài; `bus/events.rs` chỉ chứa sự kiện nội bộ của farm | Thiếu |
| Thread subscription và pass-through | Worker subscribe Thread, sự kiện đi thẳng tới worker | Chỉ có mail nội bộ (`bus/mail.rs`), không có khái niệm Thread | Thiếu |
| Inline & Spill, Chain of Recall | ID liên kết, recall nhiều hop | Memory file có giới hạn, trích dẫn `(<sha> · raw/<file>)`, không có đồ thị ID | Một phần |
| Speculative work | Làm trước trong nền | Không thấy | Thiếu |
| Full-duplex start/stop | Tool async | `worker prompt` chạy nền và `worker stop`, theo quy ước prompt ([AGENTS.md#L62](https://github.com/tinhtute/quaqua/blob/4f1acbf609775c0fb896ac5f256b3ba9be3cf347/templates/workspace/AGENTS.md#L62)) | Một phần |
| Quy chuẩn trả lời theo kênh | IM ≤100 chữ, GitHub có cấu trúc | Chỉ có kênh terminal Herdr | Thiếu |
| Tách compute và state | Cloud loop stateless, state trong DB | SQLite cục bộ, chạy trên máy cá nhân | Thiếu |
| Tách execution (BYOM) | Daemon nhẹ trên từng máy | Không có | Thiếu |
| Watchdog | Session độc lập, model đánh giá drift và tự sửa | Watchdog tất định (diff hash, mtime; [watchdog-event-driven.md](https://github.com/tinhtute/quaqua/blob/4f1acbf609775c0fb896ac5f256b3ba9be3cf347/docs/research/plans/watchdog-event-driven.md)), `liveness.rs`, Auditor read-only | Một phần |

Bốn đặc tính:

| Đặc tính | quaqua | Đánh giá |
|---|---|---|
| Continuous | Một CoS có memory file, nhưng chỉ có kênh terminal | Một phần |
| Productive | Tối đa 8 slot worker mỗi CoS, 4 Peer mỗi Lead, chạy nền song song | Có |
| Proactive | Không theo dõi môi trường, chỉ có Auditor chạy theo lịch | Thiếu |
| Persistent | Có resume, liveness và journal SQLite, nhưng phụ thuộc máy cục bộ | Một phần |

## 6. Tài liệu đọc thêm (đọc theo thứ tự)

Mọi link đã được kiểm tra, đều trả về HTTP 200 vào ngày 2/11/2026. Riêng link X gốc bị chặn (403), nên dùng bản mirror.

### 6.0 Nền tảng
1. [Exploring Super-agent Design (mirror)](https://haowenji.com/articles/x-2116829218935857152)
2. [dsh-superagent-prototype README](https://github.com/deepseek-ai/dsh-superagent-prototype/blob/6337fc823ce20251c8de8085948165c81aaa701e/README.md)
3. [001 Domain model](https://github.com/deepseek-ai/dsh-superagent-prototype/blob/6337fc823ce20251c8de8085948165c81aaa701e/docs/001-domain.md#L5-L44): Source, Trigger, Thread, Eval, Subscription.
4. [007 Architecture: Main Session và Executor tools](https://github.com/deepseek-ai/dsh-superagent-prototype/blob/6337fc823ce20251c8de8085948165c81aaa701e/docs/007-architecture.md#L488-L534): Main Session có 14 tool `orchestrator_*`, Executor có 6 tool. Main Session vẫn có `bash`; quy tắc "không đọc code" chỉ là prompt rule.

### 6.1 Trigger lọc hai tầng
- [002 Authorization](https://github.com/deepseek-ai/dsh-superagent-prototype/blob/6337fc823ce20251c8de8085948165c81aaa701e/docs/002-authorization.md)
- [src/sources/github-trigger.ts#L65-L110](https://github.com/deepseek-ai/dsh-superagent-prototype/blob/6337fc823ce20251c8de8085948165c81aaa701e/src/sources/github-trigger.ts#L65-L110)
- [src/policy.ts#L40-L125](https://github.com/deepseek-ai/dsh-superagent-prototype/blob/6337fc823ce20251c8de8085948165c81aaa701e/src/policy.ts#L40-L125)
- [Martin Fowler: What do you mean by "Event-Driven"?](https://martinfowler.com/articles/201701-event-driven.html)

### 6.2 Thread subscription và pass-through
- [002 Authorization: Subscriptions, hand-back](https://github.com/deepseek-ai/dsh-superagent-prototype/blob/6337fc823ce20251c8de8085948165c81aaa701e/docs/002-authorization.md#L18-L45)
- [003 Execution: Follow-ups and context Threads](https://github.com/deepseek-ai/dsh-superagent-prototype/blob/6337fc823ce20251c8de8085948165c81aaa701e/docs/003-execution.md#L108-L117)
- [src/core/model.ts#L75 `followupOwners`](https://github.com/deepseek-ai/dsh-superagent-prototype/blob/6337fc823ce20251c8de8085948165c81aaa701e/src/core/model.ts#L75)
- [EIP: Publish-Subscribe Channel](https://www.enterpriseintegrationpatterns.com/patterns/messaging/PublishSubscribeChannel.html)

### 6.3 Inline & Spill và Chain of Recall
- [Anthropic: Effective context engineering for AI agents](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents)
- [MemGPT: Towards LLMs as Operating Systems (arXiv 2310.08560)](https://arxiv.org/abs/2310.08560)
- [Letta docs: MemGPT concepts](https://docs.letta.com/concepts/memgpt)

### 6.4 Router batching và full-duplex
- [DeepSeek Harness `dsh-tool-jobs` README](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/jobs/tool-jobs/README.md): `job_output`, `job_list`, `job_kill`; khi job xong, agent nhận notice trong session, nhiều job xong cùng lúc chỉ tốn một step.
- [DeepSeek Harness docs: Background Job Runtime](https://deepseek-harness.github.io/deepseek-harness/en/reference/subsystems/jobs)
- [003 Execution: Background work and worker slots](https://github.com/deepseek-ai/dsh-superagent-prototype/blob/6337fc823ce20251c8de8085948165c81aaa701e/docs/003-execution.md#L27-L47)
- [ReAct (arXiv 2210.03629)](https://arxiv.org/abs/2210.03629)
- [An LLM Compiler for Parallel Function Calling (arXiv 2312.04511)](https://arxiv.org/abs/2312.04511)

### 6.5 Watchdog, speculative work, tách compute (tham khảo)
- [Anthropic: Scaling Managed Agents with stable interfaces ("decoupling the brain from the hands")](https://www.anthropic.com/engineering/managed-agents)
- [003 Execution: Failure and recovery](https://github.com/deepseek-ai/dsh-superagent-prototype/blob/6337fc823ce20251c8de8085948165c81aaa701e/docs/003-execution.md#L118-L200)
- [Speculative Actions: A Lossless Framework for Faster Agentic Systems (arXiv 2510.04371)](https://arxiv.org/abs/2510.04371)
- [Anthropic: How we built our multi-agent research system](https://www.anthropic.com/engineering/multi-agent-research-system)

## 7. Brainstorm prompts
1. Map từng concept trong `001-domain.md` (Source, Trigger, Thread, Eval, Mandate, Ticket, Subscription) sang bảng hoặc module có sẵn trong `runs/quaqua.db`. Cái nào đã có, cái nào thiếu?
2. Nếu thêm nguồn `github`, Fast Path policy tối thiểu trong `team.toml` cần những field gì để CoS không bị spam bởi CI của chính worker?
3. Thread của quaqua nên là PR, branch hay worktree? Khi Peer bị cleanup thì subscription xử lý thế nào?
4. `InboxDigest` cần những ID nào để CoS recall được report cũ trong tối đa 2 hop?
5. Có nên áp quy tắc "lệnh foreground quá 5 giây thì chuyển sang background" cho worker omp và opencode? `worker/stop.rs` đã hỗ trợ ngắt theo từng harness chưa?
6. Watchdog hiện có exit 2–6. Thêm một tín hiệu drift (diff lệch khỏi `Goal` của brief) thì đặt ở `verdict.rs` hay ở Auditor?
7. Trong chi phí token hiện tại (`quaqua usage`), phần nào là CoS chuyển tiếp mail? Đo được thì mới biết pass-through giúp quaqua bao nhiêu.

## 8. Chưa kiểm chứng
- Bài X gốc và bản tiếng Trung gốc chưa mở hay tìm được.
- Chưa đọc kỹ `engine/` và `harness/` của quaqua; mới đọc code prototype ở mức các file chính.
- Chưa chạy thử quaqua.
