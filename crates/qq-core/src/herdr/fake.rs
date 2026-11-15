//! In-memory herdr. Replies mirror the CLI's JSON shapes; tests drive state
//! with [`FakeHerdr::set_status`] and read effects back with [`FakeHerdr::prompts`].

use super::{HerdrClient, Reply};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;

#[derive(Debug, Clone)]
struct Agent {
    kind: String,
    pane: String,
    status: String,
}

#[derive(Default)]
struct State {
    agents: BTreeMap<String, Agent>,
    prompts: Vec<(String, String)>,
    /// When set, the next transport call fails with this error kind.
    fail_next: Option<io::ErrorKind>,
}

#[derive(Default)]
pub struct FakeHerdr {
    st: RefCell<State>,
}

fn agent_json(name: &str, a: &Agent) -> String {
    serde_json::json!({"name": name, "kind": a.kind, "pane_id": a.pane, "agent_status": a.status})
        .to_string()
}

impl FakeHerdr {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test control: force an agent's status (`idle`, `working`, `blocked`, `done`).
    pub fn set_status(&self, name: &str, status: &str) {
        if let Some(a) = self.st.borrow_mut().agents.get_mut(name) {
            a.status = status.to_string();
        }
    }

    /// Test control: drop an agent as if its pane closed.
    pub fn remove(&self, name: &str) {
        self.st.borrow_mut().agents.remove(name);
    }

    /// Test control: the next call returns a transport error (timeout, socket).
    pub fn fail_next(&self, kind: io::ErrorKind) {
        self.st.borrow_mut().fail_next = Some(kind);
    }

    /// Prompts delivered so far, as `(agent, text)`.
    pub fn prompts(&self) -> Vec<(String, String)> {
        self.st.borrow().prompts.clone()
    }

    fn transport(&self) -> io::Result<()> {
        match self.st.borrow_mut().fail_next.take() {
            Some(k) => Err(io::Error::new(k, "fake transport failure")),
            None => Ok(()),
        }
    }
}

impl HerdrClient for FakeHerdr {
    fn agent_start(
        &self,
        name: &str,
        kind: &str,
        pane: &str,
        _timeout_ms: u32,
    ) -> io::Result<Reply> {
        self.transport()?;
        let mut st = self.st.borrow_mut();
        // A pane hosts one agent; a second start there is refused, not stacked.
        // `pane_busy` is this fake's own code: herdr 0.9.5 documents none for a
        // start on an occupied pane. The documented busy error is `agent_blocked`
        // on prompt, replayed below.
        if st.agents.iter().any(|(n, a)| a.pane == pane && n != name) {
            return Ok(Reply::err("pane_busy"));
        }
        // Same name, same pane: converge on the running agent (idempotent start).
        let a = st.agents.entry(name.to_string()).or_insert_with(|| Agent {
            kind: kind.into(),
            pane: pane.into(),
            status: "idle".into(),
        });
        if a.kind != kind || a.pane != pane {
            return Ok(Reply::err("pane_busy"));
        }
        let body = format!(r#"{{"result":{{"agent":{}}}}}"#, agent_json(name, a));
        Ok(Reply::ok(body))
    }

    fn agent_get(&self, name: &str) -> io::Result<Reply> {
        self.transport()?;
        let st = self.st.borrow();
        Ok(match st.agents.get(name) {
            Some(a) => Reply::ok(format!(
                r#"{{"result":{{"agent":{}}}}}"#,
                agent_json(name, a)
            )),
            None => Reply::err("agent_not_found"),
        })
    }

    fn agent_list(&self) -> io::Result<Reply> {
        self.transport()?;
        let st = self.st.borrow();
        let items: Vec<String> = st.agents.iter().map(|(n, a)| agent_json(n, a)).collect();
        Ok(Reply::ok(format!(
            r#"{{"result":{{"agents":[{}]}}}}"#,
            items.join(",")
        )))
    }

    fn agent_wait(&self, name: &str, until: &[&str], _timeout_ms: u64) -> io::Result<Reply> {
        self.transport()?;
        let st = self.st.borrow();
        let Some(a) = st.agents.get(name) else {
            return Ok(Reply::err("agent_not_found"));
        };
        let matches = if until.is_empty() {
            ["idle", "done", "blocked"].contains(&a.status.as_str())
        } else {
            until.contains(&a.status.as_str())
        };
        // The fake has no clock: a state that does not match now never will.
        Ok(if matches {
            Reply::ok(format!(
                r#"{{"result":{{"agent":{}}}}}"#,
                agent_json(name, a)
            ))
        } else {
            Reply::err("timeout")
        })
    }

    fn agent_prompt(
        &self,
        name: &str,
        text: &str,
        wait: Option<(&[&str], u64)>,
    ) -> io::Result<Reply> {
        self.transport()?;
        {
            let mut st = self.st.borrow_mut();
            let Some(a) = st.agents.get(name) else {
                return Ok(Reply::err("agent_not_found"));
            };
            // herdr rejects a blocked agent before any input is sent.
            if a.status == "blocked" {
                return Ok(Reply::err("agent_blocked"));
            }
            st.prompts.push((name.to_string(), text.to_string()));
            if let Some(a) = st.agents.get_mut(name) {
                a.status = "working".into();
            }
        }
        match wait {
            None => Ok(Reply::ok(r#"{"result":{"submitted":true}}"#)),
            Some((until, timeout_ms)) => self.agent_wait(name, until, timeout_ms),
        }
    }
}
