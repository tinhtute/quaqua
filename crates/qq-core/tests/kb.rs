//! R10: workers read zbrain but never write it; the generated code-lookup config
//! is read-only with every editing and memory tool disabled.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use qq_core::kb::{
    code_lookup_config, enabled_tools, excluded_tools, zbrain_check, zbrain_tools_for, Caller,
    SERENA_EDIT_TOOLS, SERENA_MEMORY_TOOLS, ZBRAIN_READ_TOOLS, ZBRAIN_WRITE_TOOLS,
};

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("qq-kb-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn kb_worker_read_only() {
    for who in ["peer", "lead", "supervisor", "nobody-known"] {
        let c = Caller::from_role(who);
        for t in ZBRAIN_WRITE_TOOLS {
            assert!(zbrain_check(c, t).is_err(), "{who} wrote {t}");
            assert!(zbrain_check(c, &format!("mcp__zbrain__{t}")).is_err());
        }
        for t in ZBRAIN_READ_TOOLS {
            assert!(zbrain_check(c, t).is_ok(), "{who} could not read {t}");
        }
        // a tool zbrain gains later is refused until classified
        assert!(zbrain_check(c, "mcp__zbrain__brand_new_tool").is_err());
        assert!(zbrain_tools_for(c).iter().all(|t| !ZBRAIN_WRITE_TOOLS.iter().any(|w| t.ends_with(w))));
    }
    for who in ["cos", "supervisor-job"] {
        let c = Caller::from_role(who);
        for t in ZBRAIN_WRITE_TOOLS.iter().chain(ZBRAIN_READ_TOOLS.iter()) {
            assert!(zbrain_check(c, t).is_ok(), "{who} refused {t}");
        }
        assert_eq!(zbrain_tools_for(c).len(), 10);
    }
}

#[test]
fn code_lookup_read_only_config() {
    let root = scratch("cfg");
    let project = root.join("work");
    fs::create_dir_all(&project).unwrap();
    let home = root.join("serena-home");

    let cfg = code_lookup_config(&project, &home, &["rust"]).unwrap();
    let yml = fs::read_to_string(&cfg.project_yml).unwrap();

    assert!(yml.lines().any(|l| l == "read_only: true"));
    let excluded = excluded_tools(&yml);
    for t in SERENA_MEMORY_TOOLS {
        assert!(excluded.iter().any(|e| e == t), "{t} not excluded");
    }
    let enabled = enabled_tools(&yml);
    assert!(!enabled.is_empty());
    for t in enabled {
        assert!(!SERENA_EDIT_TOOLS.contains(&t.as_str()), "editing tool {t} enabled");
        assert!(!SERENA_MEMORY_TOOLS.contains(&t.as_str()), "memory tool {t} enabled");
    }

    // the server entry pins the tools interface and a Serena home outside the project
    let s = &cfg.mcp["mcpServers"]["code-lookup"];
    let args: Vec<&str> = s["args"].as_array().unwrap().iter().filter_map(|a| a.as_str()).collect();
    let at = args.iter().position(|a| *a == "--agent-interface").unwrap();
    assert_eq!(args[at + 1], "tools");
    assert_eq!(s["env"]["SERENA_HOME"], home.to_str().unwrap());
    let sc = fs::read_to_string(&cfg.serena_config).unwrap();
    assert!(sc.contains(&format!("{}/data/$projectFolderName", home.display())));

    // generation leaves the project untouched, and a home inside it is refused
    assert_eq!(fs::read_dir(&project).unwrap().count(), 0);
    assert!(code_lookup_config(&project, &project.join(".serena"), &["rust"]).is_err());
    assert!(code_lookup_config(&project, &home, &[]).is_err());
}

/// Live probe: start the real server on a git project and require an empty
/// `git status --porcelain` plus a tool list free of editing and memory tools.
/// Needs uvx and network: `cargo test -p qq-core --test kb -- --ignored`.
#[test]
#[ignore]
fn serena_live_server_keeps_project_clean() {
    use std::io::Write;
    use std::process::Stdio;
    let root = scratch("live");
    let project = root.join("work");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("a.rs"), "fn main() {}\n").unwrap();
    let git = |a: &[&str]| {
        assert!(Command::new("git").args(a).current_dir(&project).status().unwrap().success())
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "i"]);

    let cfg = code_lookup_config(&project, &root.join("home"), &["rust"]).unwrap();
    let s = &cfg.mcp["mcpServers"]["code-lookup"];
    let args: Vec<&str> = s["args"].as_array().unwrap().iter().filter_map(|a| a.as_str()).collect();
    let mut child = Command::new("uvx")
        .args(&args)
        .env("SERENA_HOME", &cfg.home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"t\",\"version\":\"0\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n"
    ).as_bytes()).unwrap();
    stdin.flush().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(25));
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let list = text.lines().find(|l| l.contains("\"id\":2")).expect("no tools/list reply");
    let v: serde_json::Value = serde_json::from_str(list).unwrap();
    let names: Vec<&str> = v["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains(&"find_symbol"));
    for n in &names {
        assert!(!SERENA_EDIT_TOOLS.contains(n) && !SERENA_MEMORY_TOOLS.contains(n), "{n} enabled");
    }
    let porcelain = Command::new("git").args(["status", "--porcelain", "--ignored"]).current_dir(&project).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&porcelain.stdout), "");
}
