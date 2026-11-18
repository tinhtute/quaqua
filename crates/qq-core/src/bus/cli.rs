//! `quaqua mail send --to <agent> --kind <kind> [--body-file -|<path>]`.
//!
//! A client of `mail.send`. The sender is the caller the RPC layer names
//! (named by the daemon from the socket peer; see [`crate::cli::rpc_call`]).

use std::io::Read;

use serde_json::json;

use crate::cli::{rpc_call, CliError, Command, Env};

pub fn command() -> Command {
    Command {
        name: "mail",
        help: "mail send --to <agent> --kind <kind> [--body-file -|<path>] [--body <text>] [--client-id <key>]",
        run: |env, args| mail(env, args, &mut std::io::stdin()),
    }
}

/// `mail <verb> ...` with the body's stdin passed in, so a caller can supply it.
pub fn mail(env: &Env, args: &[String], stdin: &mut dyn Read) -> Result<String, CliError> {
    match args.split_first() {
        Some((v, rest)) if v == "send" => send(env, rest, stdin),
        _ => Err(usage(
            "usage: quaqua mail send --to <agent> --kind <kind> [--body-file -|<path>]",
        )),
    }
}

fn usage(msg: impl Into<String>) -> CliError {
    CliError::new(2, msg)
}

fn send(env: &Env, args: &[String], stdin: &mut dyn Read) -> Result<String, CliError> {
    let (mut to, mut kind, mut file, mut body, mut client_id) = (None, None, None, None, None);
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let slot = match flag.as_str() {
            "--to" => &mut to,
            "--kind" => &mut kind,
            "--body-file" => &mut file,
            "--body" => &mut body,
            "--client-id" => &mut client_id,
            "--from" => {
                return Err(usage(
                    "--from is not permitted; the sender is the calling agent",
                ))
            }
            other => return Err(usage(format!("unknown option for mail send: {other}"))),
        };
        *slot = Some(
            it.next()
                .ok_or_else(|| usage(format!("{flag} needs a value")))?
                .clone(),
        );
    }
    let to = to.ok_or_else(|| usage("missing required flag: --to <name>"))?;
    let kind = kind.ok_or_else(|| usage("missing required flag: --kind <kind>"))?;
    let body = match (body, file) {
        (Some(b), _) => b,
        (None, Some(f)) if f != "-" => std::fs::read_to_string(&f)
            .map_err(|e| CliError::new(1, format!("failed to read body file '{f}': {e}")))?,
        // `-`, or no body flag at all: the body is stdin.
        (None, _) => {
            let mut s = String::new();
            stdin
                .read_to_string(&mut s)
                .map_err(|e| CliError::new(1, format!("failed to read stdin: {e}")))?;
            s
        }
    };
    // A retry of the same send is the same mail: the key is the caller's, else
    // the addressee, kind and body's, so the same words to someone else (or as
    // another kind) are their own mail.
    let key = client_id.unwrap_or_else(|| content_key(&to, &kind, &body));
    let sent = rpc_call(
        env,
        "mail.send",
        json!({ "to": to, "kind": kind, "body": body, "dedupe_key": key }),
    )?;
    let state = if sent["created"].as_bool() == Some(false) {
        "duplicate"
    } else {
        "created"
    };
    Ok(format!("mail id={} {state}", sent["id"]))
}

/// FNV-1a over `to|kind|body`: stable across runs, so a retry finds its first send.
fn content_key(to: &str, kind: &str, body: &str) -> String {
    let h = [to, "|", kind, "|", body]
        .iter()
        .flat_map(|p| p.bytes())
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
    format!("mail:{h:016x}")
}
