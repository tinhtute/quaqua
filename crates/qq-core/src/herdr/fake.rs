//! In-memory herdr, the only place tests replace herdr. Replies mirror the
//! CLI's JSON shapes. Tests drive it with [`FakeHerdr::set_status`],
//! [`FakeHerdr::script_status`], per-verb [`FakeHerdr::set_reply`] /
//! [`FakeHerdr::set_fault`], and read effects back with [`FakeHerdr::prompts`].

use super::{HerdrClient, Reply, Verb, NAME_CLASH, PANE_BUSY};
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::sync::{Mutex, MutexGuard};

#[derive(Debug, Clone)]
struct Agent {
    kind: String,
    pane: String,
    status: String,
    start_args: Vec<String>,
    /// Statuses the agent moves through, one per unmatched wait step.
    script: VecDeque<String>,
}

#[derive(Default)]
struct State {
    /// pane id -> text `pane_read` returns.
    text: BTreeMap<String, String>,
    agents: BTreeMap<String, Agent>,
    prompts: Vec<(String, String)>,
    keys: Vec<(String, Vec<String>)>,
    /// `(title, body, sound)` of every desktop notification.
    notifications: Vec<(String, String, Option<String>)>,
    /// Per agent: the key sequence that idles it (anything else changes nothing).
    idle_on: BTreeMap<String, Vec<String>>,
    /// The next start leaves the agent `blocked` and answers `agent_not_ready`
    /// (a startup dialog holds it).
    block_next_start: bool,
    /// When set, the next call (any verb) fails with this transport error kind.
    fail_next: Option<io::ErrorKind>,
    /// Sticky per-verb transport errors, until [`FakeHerdr::clear`].
    faults: BTreeMap<Verb, io::ErrorKind>,
    /// Sticky per-verb canned replies, until [`FakeHerdr::clear`].
    replies: BTreeMap<Verb, Reply>,
    /// tab id -> label.
    tabs: BTreeMap<String, String>,
    /// pane id -> tab id.
    panes: BTreeMap<String, String>,
    /// pane id -> harness of a pane seeded without an agent of ours (a CoS, a
    /// human's shell, another farm's worker).
    foreign: BTreeMap<String, String>,
    /// pane id -> merged tokens and display agent.
    meta: BTreeMap<String, (BTreeMap<String, String>, Option<String>)>,
    /// `(anchor pane, direction)` of every split, in order.
    splits: Vec<(String, String)>,
    /// `(new pane, cwd)` of every `pane_split_in`.
    split_cwds: Vec<(String, String)>,
    /// pane id -> (shell pid, foreground pids) `pane_process_info` reports.
    pids: BTreeMap<String, (u32, Vec<u32>)>,
    /// Agents whose prompts do not reach a turn (claude still booting).
    lose: BTreeMap<String, Lose>,
    /// Agents holding a lost prompt that an `enter` submits.
    awaiting_enter: Vec<String>,
    /// pane id -> texts `pane_read` returns one per read; the last one sticks.
    text_script: BTreeMap<String, VecDeque<String>>,
    next_id: u32,
}

/// How a prompt is lost: [`FakeHerdr::lose_prompts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lose {
    /// Every prompt is swallowed, an enter included.
    Always,
    /// The prompt is typed but not submitted: the next `enter` lands it.
    UntilEnter,
}

#[derive(Default)]
pub struct FakeHerdr {
    st: Mutex<State>,
}

fn agent_json(name: &str, a: &Agent) -> String {
    serde_json::json!({"name": name, "kind": a.kind, "pane_id": a.pane, "agent_status": a.status})
        .to_string()
}

fn agent_reply(name: &str, a: &Agent) -> Reply {
    Reply::ok(format!(
        r#"{{"result":{{"agent":{}}}}}"#,
        agent_json(name, a)
    ))
}

/// herdr's wait: block until the status matches. The fake has no clock, so the
/// scripted statuses stand in for time passing; a script that runs out without a
/// match is a timeout.
fn wait_in(a: &mut Agent, name: &str, until: &[&str]) -> Reply {
    let matches = |s: &str| {
        if until.is_empty() {
            ["idle", "done", "blocked"].contains(&s)
        } else {
            until.contains(&s)
        }
    };
    while !matches(&a.status) {
        match a.script.pop_front() {
            Some(next) => a.status = next,
            None => return Reply::err("timeout"),
        }
    }
    agent_reply(name, a)
}

impl FakeHerdr {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Test control: a pane herdr knows that this fake has no agent for (a CoS,
    /// a human's shell, another farm's worker), in tab `tab_label`, with the
    /// harness `agent` reports and the tokens `pane list` shows.
    pub fn seed_pane(
        &self,
        tab_label: &str,
        harness: Option<&str>,
        tokens: &[(&str, &str)],
    ) -> String {
        let mut st = self.lock();
        let tab = match st.tabs.iter().find(|(_, l)| l.as_str() == tab_label) {
            Some((t, _)) => t.clone(),
            None => {
                st.next_id += 1;
                let t = format!("t{}", st.next_id);
                st.tabs.insert(t.clone(), tab_label.to_string());
                t
            }
        };
        st.next_id += 1;
        let id = format!("p{}", st.next_id);
        st.panes.insert(id.clone(), tab);
        if let Some(h) = harness {
            st.foreign.insert(id.clone(), h.to_string());
        }
        let m = st.meta.entry(id.clone()).or_default();
        for (k, v) in tokens {
            m.0.insert((*k).into(), (*v).into());
        }
        id
    }

    /// The tokens `pane list` shows for `pane`.
    pub fn tokens(&self, pane: &str) -> BTreeMap<String, String> {
        self.lock()
            .meta
            .get(pane)
            .map(|m| m.0.clone())
            .unwrap_or_default()
    }

    /// Every split so far as `(anchor pane, direction)`.
    /// `(new pane, cwd)` of every cwd-carrying split, in order.
    pub fn split_cwds(&self) -> Vec<(String, String)> {
        self.lock().split_cwds.clone()
    }

    pub fn splits(&self) -> Vec<(String, String)> {
        self.lock().splits.clone()
    }

    /// Whether herdr still has `pane`.
    pub fn has_pane(&self, pane: &str) -> bool {
        self.lock().panes.contains_key(pane)
    }

    /// Test control: the shell pid and foreground pids `pane_process_info`
    /// reports for `pane`.
    pub fn set_pane_pids(&self, pane: &str, shell: u32, foreground: &[u32]) {
        self.lock()
            .pids
            .insert(pane.to_string(), (shell, foreground.to_vec()));
    }

    /// What `pane_read` returns for `pane` (its whole scrollback).
    pub fn set_pane_text(&self, pane: &str, text: &str) {
        self.lock().text.insert(pane.to_string(), text.to_string());
    }

    /// Test control: prompts sent to `name` are accepted by herdr but never
    /// start a turn (the agent stays as it was), as when claude is still booting.
    pub fn lose_prompts(&self, name: &str, how: Lose) {
        self.lock().lose.insert(name.to_string(), how);
    }

    /// Test control: successive `pane_read`s of `pane` return these texts in
    /// order; the last one stays.
    pub fn script_pane_text(&self, pane: &str, texts: &[&str]) {
        self.lock().text_script.insert(
            pane.to_string(),
            texts.iter().map(|t| t.to_string()).collect(),
        );
    }

    /// Test control: the next `agent_start` registers the agent `blocked` and
    /// answers `agent_not_ready`, as herdr does while a startup dialog is up.
    pub fn block_next_start(&self) {
        self.lock().block_next_start = true;
    }

    /// Test control: force an agent's status (`idle`, `working`, `blocked`, `done`).
    pub fn set_status(&self, name: &str, status: &str) {
        if let Some(a) = self.lock().agents.get_mut(name) {
            a.status = status.to_string();
        }
    }

    /// Test control: the statuses an agent moves through while a wait is pending,
    /// one step per non-matching poll. `prompt` then wait-for-`done` succeeds with
    /// `script_status(name, &["working", "done"])`.
    pub fn script_status(&self, name: &str, statuses: &[&str]) {
        if let Some(a) = self.lock().agents.get_mut(name) {
            a.script = statuses.iter().map(|s| s.to_string()).collect();
        }
    }

    /// Test control: drop an agent as if its pane closed.
    pub fn remove(&self, name: &str) {
        self.lock().agents.remove(name);
    }

    /// Test control: the next call (any verb) returns a transport error.
    pub fn fail_next(&self, kind: io::ErrorKind) {
        self.lock().fail_next = Some(kind);
    }

    /// Test control: every call of `verb` returns this transport error until
    /// [`FakeHerdr::clear`]. Other verbs are untouched.
    pub fn set_fault(&self, verb: Verb, kind: io::ErrorKind) {
        let mut st = self.lock();
        st.replies.remove(&verb);
        st.faults.insert(verb, kind);
    }

    /// Test control: every call of `verb` returns this reply, with no effect on
    /// the agents, until [`FakeHerdr::clear`]. Other verbs are untouched.
    pub fn set_reply(&self, verb: Verb, reply: Reply) {
        let mut st = self.lock();
        st.faults.remove(&verb);
        st.replies.insert(verb, reply);
    }

    /// Test control: drop the injection for `verb`.
    pub fn clear(&self, verb: Verb) {
        let mut st = self.lock();
        st.faults.remove(&verb);
        st.replies.remove(&verb);
    }

    /// Prompts delivered so far, as `(agent, text)`.
    pub fn prompts(&self) -> Vec<(String, String)> {
        self.lock().prompts.clone()
    }

    /// Every `send-keys` call, in order: (agent, keys).
    pub fn keys(&self) -> Vec<(String, Vec<String>)> {
        self.lock().keys.clone()
    }

    /// Every desktop notification shown, in order: (title, body, sound).
    pub fn notifications(&self) -> Vec<(String, String, Option<String>)> {
        self.lock().notifications.clone()
    }

    /// The agent goes idle when sent exactly `keys`; other sequences do nothing.
    pub fn idle_on_keys(&self, name: &str, keys: &[&str]) {
        self.lock().idle_on.insert(
            name.to_string(),
            keys.iter().map(|k| k.to_string()).collect(),
        );
    }

    /// The agent args the agent was started with (after the `--`).
    pub fn start_args(&self, name: &str) -> Vec<String> {
        self.lock()
            .agents
            .get(name)
            .map(|a| a.start_args.clone())
            .unwrap_or_default()
    }

    /// Lock the state and apply injections for `verb`: `Err` for a transport
    /// fault, `Ok(Err(reply))` for a canned reply, `Ok(Ok(guard))` to proceed.
    #[allow(clippy::type_complexity)]
    fn enter(&self, verb: Verb) -> io::Result<Result<MutexGuard<'_, State>, Reply>> {
        let mut st = self.lock();
        if let Some(k) = st.fail_next.take() {
            return Err(io::Error::new(k, "fake transport failure"));
        }
        if let Some(k) = st.faults.get(&verb) {
            return Err(io::Error::new(*k, "fake transport failure"));
        }
        if let Some(r) = st.replies.get(&verb) {
            return Ok(Err(r.clone()));
        }
        Ok(Ok(st))
    }
}

macro_rules! enter {
    ($self:ident, $verb:expr) => {
        match $self.enter($verb)? {
            Ok(st) => st,
            Err(reply) => return Ok(reply),
        }
    };
}

impl HerdrClient for FakeHerdr {
    fn agent_start(
        &self,
        name: &str,
        kind: &str,
        pane: &str,
        _timeout_ms: u32,
        agent_args: &[&str],
    ) -> io::Result<Reply> {
        let mut st = enter!(self, Verb::Start);
        if let Some(a) = st.agents.get(name) {
            // Same name, same kind and pane: converge on the running agent
            // (idempotent start). Anything else is a name clash, not a busy pane.
            return Ok(if a.kind == kind && a.pane == pane {
                agent_reply(name, a)
            } else {
                Reply::err(NAME_CLASH)
            });
        }
        // A pane hosts one agent; a start there by another name is refused.
        // `PANE_BUSY` is this fake's own code (see its doc).
        if st.agents.values().any(|a| a.pane == pane) {
            return Ok(Reply::err(PANE_BUSY));
        }
        let blocked = std::mem::take(&mut st.block_next_start);
        let a = Agent {
            kind: kind.into(),
            pane: pane.into(),
            status: if blocked { "blocked" } else { "idle" }.into(),
            start_args: agent_args.iter().map(|s| s.to_string()).collect(),
            script: VecDeque::new(),
        };
        let reply = if blocked {
            Reply::err("agent_not_ready")
        } else {
            agent_reply(name, &a)
        };
        st.agents.insert(name.to_string(), a);
        Ok(reply)
    }

    fn agent_get(&self, name: &str) -> io::Result<Reply> {
        let st = enter!(self, Verb::Get);
        Ok(match st.agents.get(name) {
            Some(a) => agent_reply(name, a),
            None => Reply::err("agent_not_found"),
        })
    }

    fn agent_list(&self) -> io::Result<Reply> {
        let st = enter!(self, Verb::List);
        let items: Vec<String> = st.agents.iter().map(|(n, a)| agent_json(n, a)).collect();
        Ok(Reply::ok(format!(
            r#"{{"result":{{"agents":[{}]}}}}"#,
            items.join(",")
        )))
    }

    fn agent_wait(&self, name: &str, until: &[&str], _timeout_ms: u64) -> io::Result<Reply> {
        let mut st = enter!(self, Verb::Wait);
        Ok(match st.agents.get_mut(name) {
            Some(a) => wait_in(a, name, until),
            None => Reply::err("agent_not_found"),
        })
    }

    fn agent_prompt(
        &self,
        name: &str,
        text: &str,
        wait: Option<(&[&str], u64)>,
    ) -> io::Result<Reply> {
        let mut st = enter!(self, Verb::Prompt);
        let st = &mut *st;
        let Some(a) = st.agents.get_mut(name) else {
            return Ok(Reply::err("agent_not_found"));
        };
        // herdr rejects a blocked agent before any input is sent.
        if a.status == "blocked" {
            return Ok(Reply::err("agent_blocked"));
        }
        match st.lose.get(name) {
            None => a.status = "working".into(),
            Some(Lose::UntilEnter) => st.awaiting_enter.push(name.to_string()),
            Some(Lose::Always) => {}
        }
        st.prompts.push((name.to_string(), text.to_string()));
        Ok(match wait {
            None => Reply::ok(r#"{"result":{"submitted":true}}"#),
            Some((until, _)) => wait_in(a, name, until),
        })
    }

    fn agent_send_keys(&self, name: &str, keys: &[&str]) -> io::Result<Reply> {
        let mut st = enter!(self, Verb::SendKeys);
        let st = &mut *st;
        if !st.agents.contains_key(name) {
            return Ok(Reply::err("agent_not_found"));
        }
        let keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
        if keys == ["enter"] && st.awaiting_enter.iter().any(|n| n == name) {
            st.awaiting_enter.retain(|n| n != name);
            if let Some(a) = st.agents.get_mut(name) {
                a.status = "working".into();
            }
        }
        if st.idle_on.get(name) == Some(&keys) {
            if let Some(a) = st.agents.get_mut(name) {
                a.status = "idle".into();
            }
        }
        st.keys.push((name.to_string(), keys));
        Ok(Reply::ok(r#"{"result":{"sent":true}}"#))
    }

    fn notification_show(&self, title: &str, body: &str, sound: Option<&str>) -> io::Result<Reply> {
        self.lock().notifications.push((
            title.to_string(),
            body.to_string(),
            sound.map(String::from),
        ));
        Ok(Reply::ok(r#"{"result":{"shown":true}}"#))
    }

    fn pane_split(&self, target: &str, dir: &str) -> io::Result<Reply> {
        let mut st = enter!(self, Verb::PaneSplit);
        st.splits.push((target.to_string(), dir.to_string()));
        let Some(tab) = st.panes.get(target).cloned() else {
            return Ok(Reply::err("pane_not_found"));
        };
        st.next_id += 1;
        let id = format!("p{}", st.next_id);
        st.panes.insert(id.clone(), tab.clone());
        Ok(Reply::ok(format!(
            r#"{{"result":{{"pane":{{"pane_id":"{id}","tab_id":"{tab}"}}}}}}"#
        )))
    }

    fn pane_split_in(&self, target: &str, dir: &str, cwd: &str) -> io::Result<Reply> {
        let reply = self.pane_split(target, dir)?;
        if reply.ok {
            if let Some(id) = serde_json::from_str::<serde_json::Value>(&reply.body)
                .ok()
                .and_then(|v| v.pointer("/result/pane/pane_id")?.as_str().map(String::from))
            {
                self.lock().split_cwds.push((id, cwd.to_string()));
            }
        }
        Ok(reply)
    }

    fn pane_close(&self, pane: &str) -> io::Result<Reply> {
        let mut st = enter!(self, Verb::PaneClose);
        if st.panes.remove(pane).is_none() {
            return Ok(Reply::err("pane_not_found"));
        }
        // Closing a pane ends the agent in it.
        st.agents.retain(|_, a| a.pane != pane);
        Ok(Reply::ok(r#"{"result":{"closed":true}}"#))
    }

    fn pane_list(&self) -> io::Result<Reply> {
        let st = enter!(self, Verb::PaneList);
        let items: Vec<serde_json::Value> = st
            .panes
            .iter()
            .map(|(id, tab)| {
                let own = st.agents.values().find(|a| &a.pane == id);
                let harness = own
                    .map(|a| a.kind.clone())
                    .or_else(|| st.foreign.get(id).cloned());
                let status = own.map_or("unknown", |a| a.status.as_str());
                let (tokens, display) = st.meta.get(id).cloned().unwrap_or_default();
                serde_json::json!({
                    "pane_id": id,
                    "tab_id": tab,
                    "workspace_id": "w1",
                    "cwd": "/work",
                    "agent": harness,
                    "agent_status": status,
                    "label": null,
                    "display_agent": display,
                    "tokens": tokens,
                })
            })
            .collect();
        Ok(Reply::ok(
            serde_json::json!({"result": {"panes": items}}).to_string(),
        ))
    }

    fn tab_list(&self) -> io::Result<Reply> {
        let st = enter!(self, Verb::TabList);
        let items: Vec<serde_json::Value> = st
            .tabs
            .iter()
            .enumerate()
            .map(|(i, (id, label))| {
                serde_json::json!({
                    "tab_id": id,
                    "label": label,
                    "number": i + 1,
                    "pane_count": st.panes.values().filter(|t| *t == id).count(),
                    "workspace_id": "w1",
                    "agent_status": "unknown",
                    "focused": false,
                })
            })
            .collect();
        Ok(Reply::ok(
            serde_json::json!({"result": {"tabs": items}}).to_string(),
        ))
    }

    fn tab_create(&self, name: &str) -> io::Result<Reply> {
        let mut st = enter!(self, Verb::TabCreate);
        st.next_id += 1;
        let tab = format!("t{}", st.next_id);
        st.next_id += 1;
        let pane = format!("p{}", st.next_id);
        st.tabs.insert(tab.clone(), name.to_string());
        st.panes.insert(pane.clone(), tab.clone());
        Ok(Reply::ok(format!(
            r#"{{"result":{{"tab":{{"tab_id":"{tab}","label":{}}},"root_pane":{{"pane_id":"{pane}","tab_id":"{tab}"}}}}}}"#,
            serde_json::json!(name)
        )))
    }

    fn tab_close(&self, tab: &str) -> io::Result<Reply> {
        let mut st = enter!(self, Verb::TabClose);
        if st.tabs.remove(tab).is_none() {
            return Ok(Reply::err("tab_not_found"));
        }
        let gone: Vec<String> = st
            .panes
            .iter()
            .filter(|(_, t)| t.as_str() == tab)
            .map(|(p, _)| p.clone())
            .collect();
        for p in &gone {
            st.panes.remove(p);
        }
        st.agents.retain(|_, a| !gone.contains(&a.pane));
        Ok(Reply::ok(r#"{"result":{"closed":true}}"#))
    }

    fn pane_read(&self, pane: &str, lines: u32) -> io::Result<Reply> {
        let mut st = enter!(self, Verb::PaneRead);
        if !st.panes.contains_key(pane) && !st.text.contains_key(pane) {
            return Ok(Reply::err("pane_not_found"));
        }
        let scripted = st.text_script.get_mut(pane).and_then(|q| {
            if q.len() > 1 {
                q.pop_front()
            } else {
                q.front().cloned()
            }
        });
        if let Some(t) = scripted {
            st.text.insert(pane.to_string(), t);
        }
        let text = st.text.get(pane).cloned().unwrap_or_default();
        let all: Vec<&str> = text.lines().collect();
        let tail = &all[all.len().saturating_sub(lines as usize)..];
        Ok(Reply::ok(tail.join("\n")))
    }

    fn pane_process_info(&self, pane: &str) -> io::Result<Reply> {
        let st = enter!(self, Verb::PaneProcessInfo);
        if !st.panes.contains_key(pane) {
            return Ok(Reply::err("pane_not_found"));
        }
        let Some((shell, fg)) = st.pids.get(pane) else {
            return Ok(Reply::err("process_info_unavailable"));
        };
        let fg: Vec<_> = fg.iter().map(|p| serde_json::json!({"pid": p})).collect();
        Ok(Reply::ok(
            serde_json::json!({"result":{"process_info":{
                "pane_id": pane, "shell_pid": shell, "foreground_processes": fg,
            }}})
            .to_string(),
        ))
    }

    fn pane_get(&self, pane: &str) -> io::Result<Reply> {
        let st = enter!(self, Verb::PaneGet);
        let Some(tab) = st.panes.get(pane) else {
            return Ok(Reply::err("pane_not_found"));
        };
        let (tokens, display) = st.meta.get(pane).cloned().unwrap_or_default();
        Ok(Reply::ok(
            serde_json::json!({"result":{"pane":{
                "pane_id": pane, "tab_id": tab, "tokens": tokens, "display_agent": display,
            }}})
            .to_string(),
        ))
    }

    fn pane_report_metadata(
        &self,
        pane: &str,
        _source: &str,
        tokens: &[(&str, &str)],
        display: Option<&str>,
    ) -> io::Result<Reply> {
        let mut st = enter!(self, Verb::PaneReportMetadata);
        if !st.panes.contains_key(pane) {
            return Ok(Reply::err("pane_not_found"));
        }
        let m = st.meta.entry(pane.to_string()).or_default();
        for (k, v) in tokens {
            m.0.insert((*k).into(), (*v).into());
        }
        if let Some(d) = display {
            m.1 = Some(d.into());
        }
        Ok(Reply::ok(r#"{"result":{"reported":true}}"#))
    }
}
