//! HerdrPanes over a herdr whose success replies are empty (the real CLI prints
//! nothing for `pane report-metadata`) or malformed.

use std::io;

use qq_core::grid::{self, HerdrPanes, Scope};
use qq_core::herdr::{HerdrClient, Reply};

struct Stub {
    claim_body: &'static str,
    tab_create_body: &'static str,
}

fn no(what: &str) -> io::Result<Reply> {
    Err(io::Error::other(format!("stub: {what} unsupported")))
}

impl HerdrClient for Stub {
    fn agent_start(&self, _: &str, _: &str, _: &str, _: u32, _: &[&str]) -> io::Result<Reply> {
        no("agent start")
    }
    fn agent_get(&self, _: &str) -> io::Result<Reply> {
        no("agent get")
    }
    fn agent_list(&self) -> io::Result<Reply> {
        no("agent list")
    }
    fn agent_wait(&self, _: &str, _: &[&str], _: u64) -> io::Result<Reply> {
        no("agent wait")
    }
    fn agent_prompt(&self, _: &str, _: &str, _: Option<(&[&str], u64)>) -> io::Result<Reply> {
        no("agent prompt")
    }
    fn tab_list(&self) -> io::Result<Reply> {
        Ok(Reply::ok(r#"{"result":{"tabs":[]}}"#))
    }
    fn pane_list(&self) -> io::Result<Reply> {
        Ok(Reply::ok(r#"{"result":{"panes":[]}}"#))
    }
    fn tab_create(&self, _: &str) -> io::Result<Reply> {
        Ok(Reply::ok(self.tab_create_body))
    }
    fn pane_report_metadata(
        &self,
        _: &str,
        _: &str,
        _: &[(&str, &str)],
        _: Option<&str>,
    ) -> io::Result<Reply> {
        Ok(Reply::ok(self.claim_body))
    }
}

const TAB: &str = r#"{"result":{"root_pane":{"pane_id":"w1:p4"}}}"#;

fn acquire(stub: &Stub, tag: &str) -> io::Result<String> {
    let dir = std::env::temp_dir().join(format!("qq-gridempty-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    grid::acquire(&HerdrPanes(stub), "repo", &Scope::default(), &dir)
}

#[test]
fn grid_acquire_accepts_empty_claim_reply() {
    let stub = Stub {
        claim_body: "",
        tab_create_body: TAB,
    };
    assert_eq!(acquire(&stub, "empty").unwrap(), "w1:p4");
}

#[test]
fn grid_malformed_reply_names_the_herdr_call() {
    let stub = Stub {
        claim_body: "",
        tab_create_body: "",
    };
    let e = acquire(&stub, "malformed").unwrap_err().to_string();
    assert!(e.contains("tab create"), "error names the call: {e}");
}
