//! `plonix users` and `plonix access`: saved users and the Access check.

use anyhow::{Result, bail};
use clap::Args;
use serde_json::{Value, json};

use crate::Ctx;
use crate::client::Client;
use crate::render::{clip, safe};

/// Prints the saved users: name, id and which credentials each carries.
/// Values stay in the project; add or edit users on the Users screen.
pub fn users_cmd(ctx: &Ctx) -> Result<()> {
    let v = ctx.client()?.get("/api/users/summary")?;
    if ctx.json {
        return ctx.print_json(&v);
    }
    let users = v["users"].as_array().cloned().unwrap_or_default();
    if users.is_empty() {
        println!("No saved users yet. Add them on the Users screen (`plonix ui`), or pick one found in traffic there.");
        return Ok(());
    }
    let acting = v["acting"].as_str().unwrap_or("");
    for u in &users {
        let id = u["id"].as_str().unwrap_or("");
        let names = |k: &str| u[k].as_array().into_iter().flatten().filter_map(Value::as_str).map(safe).collect::<Vec<_>>();
        let mut carries = vec![];
        let (headers, scoped, cookies) = (names("headers"), names("scoped"), names("cookies"));
        if !headers.is_empty() {
            carries.push(format!("headers {}", headers.join(", ")));
        }
        if !scoped.is_empty() {
            carries.push(format!("for parts of the app {}", scoped.join(", ")));
        }
        if !cookies.is_empty() {
            carries.push(format!("cookies {}", cookies.join(", ")));
        }
        println!(
            "{:<16} {}{}",
            safe(id),
            safe(u["name"].as_str().unwrap_or("")),
            if id == acting { "  (acting as)" } else { "" }
        );
        if !carries.is_empty() {
            println!("{:<16} {}", "", carries.join("; "));
        }
    }
    println!("\nReplay a request as one with `plonix replay <id> --as <user>`; compare them all with `plonix access`.");
    Ok(())
}

#[derive(Args)]
pub struct AccessArgs {
    /// Request ids to check (from `plonix search`)
    ids: Vec<i64>,
    /// Or check a whole branch: every endpoint seen on this host
    #[arg(long, conflicts_with = "ids")]
    host: Option<String>,
    /// With --host: only paths under this prefix, e.g. /api/admin
    #[arg(long, requires = "host")]
    prefix: Option<String>,
    /// Only these saved users (names or ids, comma-separated); all by default
    #[arg(long, value_delimiter = ',')]
    users: Vec<String>,
    /// Leave out the signed-out replay
    #[arg(long)]
    no_signed_out: bool,
    /// With --host: also replay requests that change data (POST, PUT, DELETE…)
    #[arg(long, requires = "host")]
    include_unsafe: bool,
    /// Wait this long between requests
    #[arg(long, value_name = "MS")]
    delay: Option<u64>,
}

/// Replays requests as every saved user (and signed out) and prints who got
/// what, the endpoints worth a look first.
pub fn access_cmd(ctx: &Ctx, a: AccessArgs) -> Result<()> {
    if a.ids.is_empty() && a.host.is_none() {
        bail!("give request ids (`plonix access 12 15`) or a branch (`plonix access --host api.example.com --prefix /api`)");
    }
    let c = ctx.client()?;
    let user_ids = a.users.iter().map(|u| user_id(&c, u)).collect::<Result<Vec<_>>>()?;
    let v = c.post(
        "/api/access-check",
        json!({
            "targets": a.ids,
            "host": a.host,
            "prefix": a.prefix,
            "user_ids": user_ids,
            "include_anon": !a.no_signed_out,
            "include_unsafe": a.include_unsafe,
            "delay_ms": a.delay,
        }),
    )?;
    if ctx.json {
        return ctx.print_json(&v);
    }
    print!("{}", access_report(&v));
    Ok(())
}

/// A saved user's id from its id or name (case-insensitive).
pub fn user_id(c: &Client, who: &str) -> Result<String> {
    let who = who.trim();
    let v = c.get("/api/users/summary")?;
    let users = v["users"].as_array().cloned().unwrap_or_default();
    let found = users
        .iter()
        .find(|u| u["id"].as_str() == Some(who))
        .or_else(|| users.iter().find(|u| u["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(who))));
    match found.and_then(|u| u["id"].as_str()) {
        Some(id) => Ok(id.to_string()),
        None => {
            let known: Vec<&str> = users.iter().filter_map(|u| u["name"].as_str()).collect();
            if known.is_empty() {
                bail!("there are no saved users yet; add them on the Users screen")
            }
            bail!("there is no saved user '{}'; saved users: {}", safe(who), known.join(", "))
        }
    }
}

/// An Access check report as a table: one row per request, one column per
/// identity (status and size), with Plonix's notes underneath.
pub fn access_report(v: &Value) -> String {
    let ids = v["identities"].as_array().cloned().unwrap_or_default();
    let rows = v["rows"].as_array().cloned().unwrap_or_default();
    let mut out = String::new();
    let label = |i: &Value| clip(&safe(i["label"].as_str().unwrap_or("")), 14);
    out.push_str(&format!("{:<44}", "REQUEST"));
    for i in &ids {
        out.push_str(&format!(" {:<14}", label(i)));
    }
    out.push('\n');
    let mut looks = 0;
    for r in &rows {
        let line = clip(&format!("{} {}", r["method"].as_str().unwrap_or(""), safe(r["path"].as_str().unwrap_or(""))), 43);
        out.push_str(&format!("{line:<44}"));
        for i in &ids {
            let cell = r["cells"].as_array().into_iter().flatten().find(|c| c["identity"] == i["id"]);
            let text = match cell {
                Some(c) if c["error"].is_string() => "error".to_string(),
                Some(c) => format!("{} {}", c["status"].as_u64().map(|s| s.to_string()).unwrap_or("-".into()), crate::render::human_size(c["len"].as_i64().unwrap_or(0))),
                None => "-".into(),
            };
            out.push_str(&format!(" {text:<14}"));
        }
        out.push('\n');
        let public = r["public"] == true;
        for n in r["notes"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            out.push_str(&format!("  {} {}\n", if public { " " } else { "!" }, safe(n)));
        }
        if !public && r["notes"].as_array().is_some_and(|n| !n.is_empty()) {
            looks += 1;
        }
    }
    let skipped: Vec<&str> = v["skipped"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if !skipped.is_empty() {
        out.push_str(&format!("\nLeft out {} that change data (add --include-unsafe to replay them): {}\n", skipped.len(), skipped.join(", ")));
    }
    out.push_str(&format!(
        "\n{} request(s), {} sent{}. {}\n",
        rows.len(),
        v["sent"].as_u64().unwrap_or(0),
        if v["truncated"] == true { ", stopped at the limit" } else { "" },
        match looks {
            0 => "Nothing stands out.".to_string(),
            1 => "1 is worth a look (marked !).".to_string(),
            n => format!("{n} are worth a look (marked !)."),
        }
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_marks_rows_to_look_at() {
        let v = json!({
            "identities": [{ "id": "alice", "label": "Alice" }, { "id": "anon", "label": "Signed out", "anon": true }],
            "rows": [
                { "method": "GET", "path": "/api/admin/users", "public": false, "notes": ["Signed out got the same page as Alice."],
                  "cells": [{ "identity": "alice", "status": 200, "len": 120 }, { "identity": "anon", "status": 200, "len": 120 }] },
                { "method": "GET", "path": "/", "public": true, "notes": ["Public: everyone got the same page, signed out too."],
                  "cells": [{ "identity": "alice", "status": 200, "len": 50 }] }
            ],
            "sent": 3, "truncated": false, "skipped": ["POST /api/orders"]
        });
        let out = access_report(&v);
        assert!(out.contains("GET /api/admin/users") && out.contains("! Signed out got"), "{out}");
        assert!(out.contains("1 is worth a look") && out.contains("Left out 1"), "{out}");
    }
}
