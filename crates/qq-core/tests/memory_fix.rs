//! memwrite T3 review M2: the Bash memory deny resolves `~/`, `$HOME` and
//! `${HOME}`. One test, because it sets HOME for the whole process.

use qq_core::authority::{gate_evaluate_in, GateVerdict};
use qq_core::cli::Env;
use qq_core::testkit::TestWorkspace;
use serde_json::json;

#[test]
fn memory_fix_home_spellings_denied() {
    let ws = TestWorkspace::new("mf-home");
    let root = ws.root().canonicalize().unwrap();
    let name = root.file_name().unwrap().to_str().unwrap().to_string();
    std::env::set_var("HOME", root.parent().unwrap());
    let env = Env {
        runs_dir: ws.runs_dir(),
    };
    // cwd is outside the workspace, as for a worker in a worktree; no daemon
    // runs, so a memory hit is a deny.
    let gate = |cmd: &str| {
        let hook = json!({"tool_name": "Bash", "tool_input": {"command": cmd}, "cwd": "/"});
        gate_evaluate_in(&env, &hook.to_string())
    };
    for cmd in [
        format!("echo x >> ~/{name}/USER.md"),
        format!("sed -i s/u/v/ ~/{name}/memory/rules.md"),
        format!("cp /dev/null $HOME/{name}/USER.md"),
        format!("cp /dev/null ${{HOME}}/{name}/USER.md"),
        format!("echo x >> \"$HOME/{name}/memory/rules.md\""),
        format!("dd of=~/{name}/USER.md if=/dev/null"),
        format!("python3 -c \"open('$HOME/{name}/USER.md','a')\""),
    ] {
        assert!(
            matches!(gate(&cmd), GateVerdict::Block(_)),
            "must deny: {cmd}"
        );
    }
    for cmd in [
        format!("cat ~/{name}/USER.md"),
        format!("echo x >> ~/{name}/notes.md"),
        "echo x >> ~/other/USER.md".to_string(),
    ] {
        assert_eq!(gate(&cmd), GateVerdict::Allow, "{cmd}");
    }
}
