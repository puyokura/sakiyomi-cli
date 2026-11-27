//! `sakiyomi mcp`: JSON-RPC 2.0 over stdio, one message per line.
//!
//! Framing and method shapes were taken from an observed session of the MCP
//! client this host actually uses (`server/discover`, `initialize`, then
//! `notifications/initialized`, `tools/list`, `tools/call`), captured in
//! `tmp/mcp-probe/wire.log`. No SDK and no async runtime: a line reader, a
//! dispatcher, one writer thread.
//!
//! Tool failures are reported two ways on purpose, matching what the client
//! distinguishes:
//! * a bad argument or unknown tool is a *protocol* error (`error` object), so a
//!   client sees the tool does not exist or was called wrong;
//! * a tool that ran and failed returns `isError: true` with the message in
//!   `content`. The host turns that into a plain thrown error carrying the text.

use std::io::{BufRead, Write};

use serde_json::{json, Value};

use crate::client::{classify, Api};
use crate::config::Settings;
use crate::session::Session;
use crate::transport::Transport;
use crate::wire::WireRequest;

/// Protocol version strings the client accepts when echoed back.
///
/// Measured against `@modelcontextprotocol/client` 2.2.0: `2026-07-28` and other
/// non-legacy strings are rejected with "Server's protocol version is not
/// supported". The client probes `2026-07-28` via `server/discover` and then
/// offers one of these in `initialize`, so echoing an arbitrary value is unsafe.
const LEGACY_PROTOCOLS: &[&str] = &[
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
    "2024-10-07",
];

const DEFAULT_PROTOCOL: &str = "2025-11-25";

fn negotiate_protocol(requested: Option<&str>) -> &'static str {
    match requested {
        Some(v) => LEGACY_PROTOCOLS
            .iter()
            .find(|p| **p == v)
            .copied()
            .unwrap_or(DEFAULT_PROTOCOL),
        None => DEFAULT_PROTOCOL,
    }
}

/// Serve until stdin closes.
///
/// Concurrency, chosen to fix a measured defect: with tool calls run inline, a
/// 30-second HTTP request blocks `ping` and `tools/list` for 30 seconds. So the
/// main thread only reads stdin and answers the trivial methods inline, each
/// `tools/call` runs on its own thread, and every reply funnels through one
/// channel to a single writer thread. The in-flight count is capped; a saturated
/// server answers immediately with `isError: true` rather than queueing.
pub fn serve(settings: Settings) -> i32 {
    /// Maximum simultaneous tool calls. Each holds an HTTP connection, so this
    /// is a socket budget as much as a thread budget.
    const MAX_IN_FLIGHT: usize = 8;

    let settings = std::sync::Arc::new(settings);
    // The session is snapshotted once. The MCP server does not log in or out, so
    // there is nothing to re-read, and avoiding a shared mutable session keeps
    // the tool threads independent.
    let session =
        std::sync::Arc::new(crate::session::load(&settings.state_path).unwrap_or_default());

    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let writer = std::thread::spawn(move || {
        let mut out = std::io::stdout();
        for msg in rx {
            if writeln!(out, "{}", msg).is_err() {
                break;
            }
            // Flush per message. Without this the client times out: it parses
            // only complete lines, and a buffered line never arrives.
            if out.flush().is_err() {
                break;
            }
        }
    });

    let in_flight = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let stdin = std::io::stdin();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }

        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                // A malformed line has no id to answer to; report on stderr so
                // stdout stays pure protocol.
                eprintln!("mcp: malformed JSON on stdin: {}", e);
                continue;
            }
        };

        let id = match msg.get("id").cloned() {
            // Notifications carry no id and must never be answered, including
            // notifications/cancelled.
            None => continue,
            Some(v) if v.is_null() => continue,
            Some(v) => v,
        };
        let method = msg
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        if method == "tools/call" {
            let n = in_flight.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n >= MAX_IN_FLIGHT {
                in_flight.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                let reply = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": error_result(
                        "server is busy: too many tool calls in flight; retry shortly"
                    ),
                });
                let _ = tx.send(reply.to_string());
                continue;
            }

            let tx = tx.clone();
            let settings = std::sync::Arc::clone(&settings);
            let session = std::sync::Arc::clone(&session);
            let in_flight = std::sync::Arc::clone(&in_flight);
            std::thread::spawn(move || {
                let reply = match call_tool_params(&params, &settings, &session) {
                    Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                    Err(e) => {
                        json!({"jsonrpc": "2.0", "id": id, "error": {"code": e.code, "message": e.message}})
                    }
                };
                in_flight.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                let _ = tx.send(reply.to_string());
            });
            continue;
        }

        let reply = match dispatch(&method, &params, &settings, &session) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(e) => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": e.code, "message": e.message}})
            }
        };
        if tx.send(reply.to_string()).is_err() {
            break;
        }
    }

    // EOF: close the channel so the writer drains and exits, then join it so no
    // reply is lost on the way out.
    drop(tx);
    let _ = writer.join();
    0
}

struct RpcError {
    code: i64,
    message: String,
}

impl std::fmt::Debug for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RpcError({}): {}", self.code, self.message)
    }
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    fn unknown_tool(name: &str) -> Self {
        Self::new(-32602, format!("Unknown tool: {}", name))
    }
    fn bad_params(message: impl Into<String>) -> Self {
        Self::new(-32602, message)
    }
}

/// Methods answered on the read loop, so they never wait behind a tool call.
fn dispatch(
    method: &str,
    params: &Value,
    settings: &Settings,
    session: &Session,
) -> Result<Value, RpcError> {
    match method {
        "initialize" => {
            let requested = params.get("protocolVersion").and_then(|v| v.as_str());
            Ok(json!({
                "protocolVersion": negotiate_protocol(requested),
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": {
                    "name": "sakiyomi",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => call_tool_params(params, settings, session),
        // The host probes `server/discover` before initialize. Answering -32601
        // lets it fall back immediately; silence costs a 60-second timeout.
        _ => Err(RpcError::new(
            -32601,
            format!("Method not found: {}", method),
        )),
    }
}

fn call_tool_params(
    params: &Value,
    settings: &Settings,
    session: &Session,
) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::bad_params("tools/call requires params.name"))?;
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    call_tool(name, &args, settings, session)
}

fn tool_definitions() -> Vec<Value> {
    vec![
        tool(
            "doctor",
            "Check connectivity, authentication and the public config endpoint.",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "config",
            "Read the public feature flags (GET /config). No auth required.",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "me",
            "Read the signed-in account (GET /me). Requires a session token.",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "events_list",
            "List prediction events (GET /events).",
            json!({"type":"object","properties":{
                "ids": {"type":"array","items":{"type":"string"},
                        "description":"Optional event ids to fetch instead of the full list."},
                "stake": {"type":"integer",
                          "description":"Add buy_prob and shares to every option, for this stake."}
            }}),
        ),
        tool(
            "event_history",
            "Price history for one event (GET /events/{id}/history).",
            json!({"type":"object","properties":{
                "id": {"type":"string"},
                "period": {"type":"string","default":"1d"}
            },"required":["id"]}),
        ),
        tool(
            "live_board",
            "Live board snapshot (GET /live-board).",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "positions",
            "List your positions (GET /positions).",
            json!({"type":"object","properties":{
                "tab": {"type":"string","default":"open"}
            }}),
        ),
        tool(
            "position_claim",
            "Collect a settled payout (POST /positions/{id}/claim). MOVES POINTS. Requires confirm=true.",
            json!({"type":"object","properties":{
                "position": {"type":"string",
                             "description":"Position id, or 'all' to collect every won position that is still unclaimed."},
                "currency": {"type":"string","default":"point","enum":["point","coin"]},

                "keep_going": {"type":"boolean","default":false,
                               "description":"With 'all', continue after a failure."},
                "confirm": {"type":"boolean"},
                "dry_run": {"type":"boolean","default":false}
            },"required":["position","confirm"]}),
        ),
        tool(
            "missions",
            "List missions (GET /missions).",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "ledger",
            "Point ledger rows (GET /ledger).",
            json!({"type":"object","properties":{
                "currency": {"type":"string","default":"yen"}
            }}),
        ),
        tool(
            "rankings",
            "Profit ranking (GET /rankings?kind=profit).",
            json!({"type":"object","properties":{
                "period": {"type":"string","default":"1d"}
            }}),
        ),
        tool(
            "notices",
            "Notices (GET /notices).",
            json!({"type":"object","properties":{}}),
        ),
        // Money-moving tools are exposed but require an explicit confirmation
        // argument, so an automated caller cannot spend points by accident.
        tool(
            "buy",
            "Buy into an event option (POST /events/{id}/buy). MOVES POINTS. Requires confirm=true.",
            json!({"type":"object","properties":{
                "event": {"type":"string"},
                "option": {"type":"string"},
                "stake": {"type":"integer"},
                "confirm": {"type":"boolean","description":"Must be true. Guards against accidental spend."},
                "dry_run": {"type":"boolean","default":false}
            },"required":["event","option","stake","confirm"]}),
        ),
        tool(
            "sell",
            "Sell a position (POST /positions/{id}/sell). MOVES POINTS. Requires confirm=true.",
            json!({"type":"object","properties":{
                "position": {"type":"string"},
                "confirm": {"type":"boolean"},
                "dry_run": {"type":"boolean","default":false}
            },"required":["position","confirm"]}),
        ),
        tool(
            "claim",
            "Claim a mission reward (POST /missions/{id}/claim). MOVES POINTS. Requires confirm=true.",
            json!({"type":"object","properties":{
                "mission": {"type":"string"},
                "confirm": {"type":"boolean"},
                "dry_run": {"type":"boolean","default":false}
            },"required":["mission","confirm"]}),
        ),
        tool(
            "bonus_status",
            "The daily login bonus state (GET /ads/reward). Read-only.",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "wave_play",
            "Play one wave round headlessly and collect its coins (POST /minigame/wave/start then /finish). MOVES POINTS. Requires confirm=true.",
            json!({"type":"object","properties":{
                "rules_version": {"type":"integer","default":2,
                                  "description":"1 or 2; the server refuses anything else with 409."},
                "wait_secs": {"type":"integer",
                              "description":"Seconds to wait before finishing. Defaults to when the last tap has passed."},
                "confirm": {"type":"boolean","description":"Must be true."},
                "dry_run": {"type":"boolean","default":false}
            },"required":["confirm"]}),
        ),
        tool(
            "quiz_create",
            "Create a quiz (POST /quizzes). MOVES POINTS when it is adopted. Requires confirm=true.",
            json!({"type":"object","properties":{
                "title": {"type":"string"},
                "description": {"type":"string","default":""},
                "options": {"type":"array","items":{"type":"object","properties":{
                    "label": {"type":"string"},
                    "prob": {"type":"integer"}
                },"required":["label"]},
                    "description":"At least two."},
                "closes_at": {"type":"string","description":"ISO 8601 UTC."},
                "image_url": {"type":"string"},
                "news_url": {"type":"string"},
                "note": {"type":"string","default":""},
                "confirm": {"type":"boolean"},
                "dry_run": {"type":"boolean","default":false}
            },"required":["title","options","closes_at","confirm"]}),
        ),
        tool(
            "schedule_list",
            "Show the scheduled purchase queue. Read-only.",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "schedule_add",
            "Queue a purchase for a later time. Nothing is sent until `schedule run` is started outside MCP.",
            json!({"type":"object","properties":{
                "at": {"type":"string","description":"When to buy, e.g. 2026-12-01T12:00+09:00."},
                "event": {"type":"string"},
                "option": {"type":"string"},
                "stake": {"type":"integer","description":"Coins to stake, 10..100000."},
                "confirm": {"type":"boolean"}
            },"required":["at","event","option","stake","confirm"]}),
        ),
        tool(
            "schedule_remove",
            "Drop an entry from the queue. Nothing is sent.",
            json!({"type":"object","properties":{
                "id": {"type":"string"}
            },"required":["id"]}),
        ),
        tool(
            "quizzes_mine",
            "Your own quizzes (GET /quizzes/mine). Read-only.",
            json!({"type":"object","properties":{}}),
        ),
    ]
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({"name": name, "description": description, "inputSchema": input_schema})
}

fn text_result(text: impl Into<String>) -> Value {
    json!({"content": [{"type": "text", "text": text.into()}]})
}

fn error_result(text: impl Into<String>) -> Value {
    json!({
        "content": [{"type": "text", "text": text.into()}],
        "isError": true
    })
}

/// One HTTP exchange for a tool. `isError: true` for anything the call surfaced,
/// because the tool did run; protocol errors are reserved for bad arguments and
/// unknown tools, which never reach here.
fn fetch(transport: &Transport, req: &WireRequest) -> Value {
    let had_token = transport.has_token();
    match transport.send(req) {
        Ok(r) => match classify(r, had_token, false) {
            Ok(r) => text_result(crate::output::to_json(&r.body)),
            Err(e) => error_result(e.to_string()),
        },
        Err(e) => error_result(e.to_string()),
    }
}

fn call_tool(
    name: &str,
    args: &Value,
    settings: &Settings,
    session: &Session,
) -> Result<Value, RpcError> {
    let token = session.token().map(|s| s.to_string());
    let transport = Transport::new(
        settings.base.clone(),
        token,
        std::time::Duration::from_secs(settings.timeout_secs),
    );
    let api = Api::new(settings.app_version.clone());

    match name {
        "doctor" => {
            let (s, origins) = crate::config::resolve(&crate::config::Overrides::default())
                .unwrap_or_else(|_| (settings.clone(), Vec::new()));
            let report = crate::doctor::run(&s, origins, session);
            Ok(text_result(report.to_text()))
        }
        "config" => Ok(fetch(&transport, &api.config())),
        "me" => Ok(fetch(&transport, &api.me())),
        "events_list" => {
            let ids: Option<Vec<String>> = args.get("ids").and_then(|v| v.as_array()).map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            });
            let stake = args.get("stake").and_then(|v| v.as_u64());

            // The server takes at most 50 ids per request, so a longer list is
            // split the same way the CLI splits it; only the merged answer is
            // returned, and it is annotated once at the end.
            let mut merged: Vec<serde_json::Value> = Vec::new();
            let mut shape: Option<serde_json::Value> = None;
            let mut batches: Vec<&[String]> = Vec::new();
            match ids.as_deref() {
                None => batches.push(&[]),
                Some(list) if list.len() <= 50 => batches.push(list),
                Some(list) => batches.extend(list.chunks(50)),
            }
            for chunk in batches {
                let req = if chunk.is_empty() {
                    api.events(None, false)
                } else {
                    api.events(Some(chunk), false)
                };
                match transport.send(&req) {
                    Ok(r) if (200..300).contains(&r.status) => {
                        if let Some(list) = r.body.get("events").and_then(|v| v.as_array()) {
                            merged.extend(list.iter().cloned());
                        }
                        if shape.is_none() {
                            shape = Some(r.body);
                        }
                    }
                    Ok(r) => {
                        return Ok(error_result(format!(
                            "batch failed: HTTP {}\n{}",
                            r.status,
                            crate::output::to_json(&r.body)
                        )))
                    }
                    Err(e) => return Ok(error_result(format!("batch failed: {}", e))),
                }
            }
            let mut body = shape.unwrap_or_else(|| serde_json::json!({ "events": [] }));
            if let Some(obj) = body.as_object_mut() {
                obj.insert("events".to_string(), serde_json::Value::Array(merged));
            }
            if let Some(stake) = stake {
                crate::estimate::annotate_events(&mut body, stake);
            }
            Ok(text_result(crate::output::to_json(&body)))
        }
        "event_history" => {
            let id = str_arg(args, "id")?;
            let period = args.get("period").and_then(|v| v.as_str()).unwrap_or("1d");
            Ok(fetch(&transport, &api.event_history(&id, period)))
        }
        "live_board" => Ok(fetch(&transport, &api.live_board())),
        "positions" => {
            let tab = args.get("tab").and_then(|v| v.as_str()).unwrap_or("open");
            Ok(fetch(&transport, &api.positions(tab)))
        }
        "position_claim" => {
            require_confirm(args)?;
            let position = str_arg(args, "position")?;
            let currency = args
                .get("currency")
                .and_then(|v| v.as_str())
                .unwrap_or("point");
            if currency != "point" && currency != "coin" {
                return Err(RpcError::bad_params("currency must be 'point' or 'coin'"));
            }

            if position != "all" {
                let req = api.claim_position(&position, currency);
                if flag(args, "dry_run") {
                    return Ok(text_result(crate::guard::describe(
                        &req,
                        transport.base(),
                        session.token(),
                    )));
                }
                let (code, msg) = crate::guard::MoneyGuard::new(
                    &transport,
                    req,
                    format!("claim payout for position {}", position),
                )
                .run()
                .report();
                return Ok(if code == 0 {
                    text_result(msg)
                } else {
                    error_result(msg)
                });
            }

            // 'all': read the list, then claim what it shows. The list reply is
            // not printed; only the claim outcomes are.
            let list = match transport.send(&api.positions("result")) {
                Ok(r) if (200..300).contains(&r.status) => r,
                Ok(r) => {
                    return Ok(error_result(format!(
                        "could not read positions: HTTP {}\n{}",
                        r.status,
                        crate::output::to_json(&r.body)
                    )))
                }
                Err(e) => return Ok(error_result(format!("could not read positions: {}", e))),
            };
            let pending: Vec<String> = list
                .body
                .get("positions")
                .and_then(|v| v.as_array())
                .map(|rows| {
                    rows.iter()
                        .filter(|p| {
                            p.get("status").and_then(|s| s.as_str()) == Some("won")
                                && p.get("unclaimed").and_then(|u| u.as_i64()) == Some(1)
                        })
                        .filter_map(|p| {
                            p.get("id")
                                .and_then(|id| id.as_i64())
                                .map(|n| n.to_string())
                        })
                        .collect()
                })
                .unwrap_or_default();

            if pending.is_empty() {
                return Ok(text_result("ok: no unclaimed payouts"));
            }
            if flag(args, "dry_run") {
                return Ok(text_result(format!(
                    "would claim {} payout(s): {}",
                    pending.len(),
                    pending.join(", ")
                )));
            }

            let keep_going = flag(args, "keep_going");
            let mut done = Vec::new();
            let mut failed = Vec::new();
            for id in &pending {
                let req = api.claim_position(id, currency);
                let (code, msg) = crate::guard::MoneyGuard::new(
                    &transport,
                    req,
                    format!("claim payout for position {}", id),
                )
                .run()
                .report();
                if code == 0 {
                    done.push(format!("{}: {}", id, msg));
                } else {
                    failed.push(format!("{}: {}", id, msg));
                    if !keep_going {
                        break;
                    }
                }
            }
            let mut out = String::new();
            if !done.is_empty() {
                out.push_str(&format!("claimed {}:\n{}\n", done.len(), done.join("\n")));
            }
            if !failed.is_empty() {
                out.push_str(&format!(
                    "failed {}:\n{}\n",
                    failed.len(),
                    failed.join("\n")
                ));
            }
            Ok(if failed.is_empty() {
                text_result(out.trim_end().to_string())
            } else {
                error_result(out.trim_end().to_string())
            })
        }
        "missions" => Ok(fetch(&transport, &api.missions())),
        "ledger" => {
            let currency = args
                .get("currency")
                .and_then(|v| v.as_str())
                .unwrap_or("yen");
            Ok(fetch(&transport, &api.ledger(currency)))
        }
        "rankings" => {
            let period = args.get("period").and_then(|v| v.as_str()).unwrap_or("1d");
            Ok(fetch(&transport, &api.rankings(period)))
        }
        "notices" => Ok(fetch(&transport, &api.notices())),

        // ---- bonus / minigame / quiz ----------------------------------------
        "bonus_status" => {
            // No `mg_play`: the minigame video bonus is an ad, so there is
            // nothing here to ask about it.
            let had_token = transport.has_token();
            match transport.send(&api.ads_reward()) {
                Ok(r) => match classify(r, had_token, false) {
                    Ok(r) => {
                        // Same removal as the CLI: the ad-shaped keys are not
                        // something this client can act on.
                        let mut body = r.body;
                        crate::strip_ad_fields(&mut body);
                        Ok(text_result(crate::output::to_json(&body)))
                    }
                    Err(e) => Ok(error_result(e.to_string())),
                },
                Err(e) => Ok(error_result(e.to_string())),
            }
        }
        "wave_play" => {
            require_confirm(args)?;
            let rules = args
                .get("rules_version")
                .and_then(|v| v.as_u64())
                .unwrap_or(2) as u32;
            let start = api.minigame_start(rules);
            if flag(args, "dry_run") {
                return Ok(text_result(crate::guard::describe(
                    &start,
                    transport.base(),
                    session.token(),
                )));
            }
            let started = match transport.send(&start) {
                Ok(r) if (200..300).contains(&r.status) => r,
                Ok(r) => {
                    return Ok(error_result(format!(
                        "start failed: HTTP {}\n{}",
                        r.status,
                        crate::output::to_json(&r.body)
                    )))
                }
                Err(e) => return Ok(error_result(format!("start failed: {}", e))),
            };
            let play_id = match started.body.get("play_id").and_then(|v| v.as_u64()) {
                Some(id) => id,
                None => return Ok(error_result("the start reply has no play_id")),
            };
            let level = match crate::wave::Level::from_start(&started.body) {
                Some(l) => l,
                None => {
                    return Ok(error_result(
                        "the start reply carries no usable level; refusing to invent one",
                    ))
                }
            };
            let taps = match level.solve() {
                Some(t) => t,
                None => return Ok(error_result("this level could not be solved")),
            };
            // The replay treats taps still in the future as misses, so the finish
            // has to wait until the last one is in the past.
            let wait = args
                .get("wait_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or_else(|| crate::wave::Level::settle_ms(&taps).div_ceil(1000));
            std::thread::sleep(std::time::Duration::from_secs(wait));
            let finish = api.minigame_finish(&play_id.to_string(), &taps);
            let (code, msg) = crate::guard::MoneyGuard::new(
                &transport,
                finish,
                format!("finish wave round {}", play_id),
            )
            .run()
            .report();
            Ok(if code == 0 {
                text_result(crate::strip_video_from_report(&msg))
            } else {
                error_result(msg)
            })
        }
        "quiz_create" => {
            require_confirm(args)?;
            let draft = crate::client::QuizDraft {
                title: str_arg(args, "title")?,
                description: args
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                options: args
                    .get("options")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|o| {
                                let label = o.get("label")?.as_str()?.to_string();
                                let prob = o.get("prob").and_then(|p| p.as_u64()).map(|p| p as u8);
                                Some((label, prob))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                closes_at: str_arg(args, "closes_at")?,
                image_url: args
                    .get("image_url")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                news_url: args
                    .get("news_url")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                note: args
                    .get("note")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            };
            if draft.options.len() < 2 {
                return Err(RpcError::bad_params(
                    "quiz_create needs at least two options",
                ));
            }
            let req = api.create_quiz(&draft);
            let (code, msg) = crate::guard::MoneyGuard::new(
                &transport,
                req,
                format!("create quiz {:?}", draft.title),
            )
            .run()
            .report();
            Ok(if code == 0 {
                text_result(msg)
            } else {
                error_result(msg)
            })
        }
        "quizzes_mine" => Ok(fetch(&transport, &api.quizzes_mine())),

        // ---- schedule ---------------------------------------------------------
        "schedule_list" => {
            let path = settings.schedule_path.clone();
            match crate::schedule::Schedule::load(&path) {
                Ok(q) => {
                    let body = serde_json::to_value(&q)
                        .unwrap_or_else(|_| serde_json::json!({"entries": []}));
                    Ok(text_result(crate::output::to_json(&body)))
                }
                Err(e) => Ok(error_result(format!("could not read the queue: {}", e))),
            }
        }
        "schedule_add" => {
            require_confirm(args)?;
            let at = str_arg(args, "at")?;
            let event = str_arg(args, "event")?;
            let option = str_arg(args, "option")?;
            let stake = u64_arg(args, "stake")?;
            if !(10..=100_000).contains(&stake) {
                return Err(RpcError::bad_params(
                    "stake must be between 10 and 100000 (the server refuses anything else)",
                ));
            }
            let at = match crate::schedule::parse_at(&at) {
                Ok(v) => v,
                Err(e) => return Err(RpcError::bad_params(format!("at: {}", e))),
            };
            let path = settings.schedule_path.clone();
            let mut q = match crate::schedule::Schedule::load(&path) {
                Ok(q) => q,
                Err(e) => return Ok(error_result(format!("could not read the queue: {}", e))),
            };
            let id = q.new_id();
            q.entries.push(crate::schedule::Entry {
                id: id.clone(),
                at,
                event,
                option,
                stake,
                state: crate::schedule::State::Pending,
                note: None,
                updated_at: None,
            });
            if let Err(e) = q.save(&path) {
                return Ok(error_result(format!("could not write the queue: {}", e)));
            }
            Ok(text_result(format!(
                "queued {}. Nothing is sent until `sakiyomi schedule run` is started.",
                id
            )))
        }
        "schedule_remove" => {
            let id = str_arg(args, "id")?;
            let path = settings.schedule_path.clone();
            let mut q = match crate::schedule::Schedule::load(&path) {
                Ok(q) => q,
                Err(e) => return Ok(error_result(format!("could not read the queue: {}", e))),
            };
            let before = q.entries.len();
            q.entries.retain(|e| e.id != id);
            if q.entries.len() == before {
                return Ok(error_result(format!("no entry with id {}", id)));
            }
            if let Err(e) = q.save(&path) {
                return Ok(error_result(format!("could not write the queue: {}", e)));
            }
            Ok(text_result(format!("removed {}", id)))
        }

        // ---- money ---------------------------------------------------------
        "buy" => {
            require_confirm(args)?;
            let event = str_arg(args, "event")?;
            let option = str_arg(args, "option")?;
            let stake = u64_arg(args, "stake")?;
            let req = api.buy(&event, &option, stake);
            if flag(args, "dry_run") {
                return Ok(text_result(crate::guard::describe(
                    &req,
                    transport.base(),
                    session.token(),
                )));
            }
            let (code, msg) = crate::guard::MoneyGuard::new(
                &transport,
                req,
                format!("buy {} x{} on {}", option, stake, event),
            )
            .run()
            .report();
            Ok(if code == 0 {
                text_result(msg)
            } else {
                error_result(msg)
            })
        }
        "sell" => {
            require_confirm(args)?;
            let position = str_arg(args, "position")?;
            let req = api.sell(&position);
            if flag(args, "dry_run") {
                return Ok(text_result(crate::guard::describe(
                    &req,
                    transport.base(),
                    session.token(),
                )));
            }
            let (code, msg) = crate::guard::MoneyGuard::new(
                &transport,
                req,
                format!("sell position {}", position),
            )
            .run()
            .report();
            Ok(if code == 0 {
                text_result(msg)
            } else {
                error_result(msg)
            })
        }
        "claim" => {
            require_confirm(args)?;
            let mission = str_arg(args, "mission")?;
            let req = api.claim(&mission);
            if flag(args, "dry_run") {
                return Ok(text_result(crate::guard::describe(
                    &req,
                    transport.base(),
                    session.token(),
                )));
            }
            let (code, msg) = crate::guard::MoneyGuard::new(
                &transport,
                req,
                format!("claim mission {}", mission),
            )
            .run()
            .report();
            Ok(if code == 0 {
                text_result(msg)
            } else {
                error_result(msg)
            })
        }
        other => Err(RpcError::unknown_tool(other)),
    }
}

fn require_confirm(args: &Value) -> Result<(), RpcError> {
    if flag(args, "confirm") {
        Ok(())
    } else {
        Err(RpcError::bad_params(
            "this tool moves points; pass confirm=true to proceed, or dry_run=true to preview",
        ))
    }
}

fn flag(args: &Value, key: &str) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn str_arg(args: &Value, key: &str) -> Result<String, RpcError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| RpcError::bad_params(format!("missing string argument '{}'", key)))
}

fn u64_arg(args: &Value, key: &str) -> Result<u64, RpcError> {
    args.get(key)
        .and_then(|v| v.as_u64())
        .ok_or_else(|| RpcError::bad_params(format!("missing integer argument '{}'", key)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;
    use std::path::PathBuf;

    fn settings() -> Settings {
        Settings {
            base: "http://127.0.0.1:9/api".into(),
            timeout_secs: 1,
            token: None,
            state_path: PathBuf::from("unused.json"),
            schedule_path: PathBuf::from("unused-schedule.json"),
            config_path: None,
            app_version: "1.0.42".into(),
        }
    }

    #[test]
    fn initialize_echoes_a_supported_version() {
        let s = Session::default();
        let r = dispatch(
            "initialize",
            &json!({"protocolVersion": "2025-06-18"}),
            &settings(),
            &s,
        )
        .unwrap();
        assert_eq!(r["protocolVersion"], "2025-06-18");
        assert_eq!(r["serverInfo"]["name"], "sakiyomi");
        assert!(r["capabilities"]["tools"].is_object());
    }

    #[test]
    fn initialize_refuses_to_echo_an_unsupported_version() {
        let s = Session::default();
        let r = dispatch(
            "initialize",
            &json!({"protocolVersion": "2026-07-28"}),
            &settings(),
            &s,
        )
        .unwrap();
        assert_eq!(r["protocolVersion"], DEFAULT_PROTOCOL);
    }

    #[test]
    fn initialize_defaults_when_version_absent() {
        let s = Session::default();
        let r = dispatch("initialize", &json!({}), &settings(), &s).unwrap();
        assert_eq!(r["protocolVersion"], DEFAULT_PROTOCOL);
    }

    #[test]
    fn negotiation_only_returns_legacy_strings() {
        for offered in [
            Some("2025-11-25"),
            Some("2025-06-18"),
            Some("2025-03-26"),
            Some("2024-11-05"),
            Some("2024-10-07"),
            Some("2026-07-28"),
            Some("garbage"),
            None,
        ] {
            let got = negotiate_protocol(offered);
            assert!(
                LEGACY_PROTOCOLS.contains(&got),
                "offered {:?} produced non-legacy {:?}",
                offered,
                got
            );
        }
    }

    #[test]
    fn ping_returns_empty_result() {
        let s = Session::default();
        let r = dispatch("ping", &json!({}), &settings(), &s).unwrap();
        assert_eq!(r, json!({}));
    }

    #[test]
    fn tools_list_has_schema_for_every_tool() {
        let s = Session::default();
        let r = dispatch("tools/list", &json!({}), &settings(), &s).unwrap();
        let tools = r["tools"].as_array().unwrap();
        assert!(tools.len() >= 10);
        for t in tools {
            assert!(t["name"].is_string(), "{:?}", t);
            assert!(t["description"].is_string(), "{:?}", t);
            assert_eq!(t["inputSchema"]["type"], "object", "{:?}", t);
        }
    }

    #[test]
    fn money_tools_document_their_confirm_requirement() {
        let s = Session::default();
        let r = dispatch("tools/list", &json!({}), &settings(), &s).unwrap();
        for t in r["tools"].as_array().unwrap() {
            let name = t["name"].as_str().unwrap();
            if ["buy", "sell", "claim"].contains(&name) {
                let required = t["inputSchema"]["required"].as_array().unwrap();
                assert!(
                    required.iter().any(|v| v == "confirm"),
                    "{} must require confirm",
                    name
                );
                assert!(
                    t["description"].as_str().unwrap().contains("MOVES POINTS"),
                    "{} description must warn",
                    name
                );
            }
        }
    }

    #[test]
    fn unknown_tool_is_a_protocol_error() {
        let s = Session::default();
        let e = dispatch(
            "tools/call",
            &json!({"name": "definitely_not_a_tool", "arguments": {}}),
            &settings(),
            &s,
        )
        .unwrap_err();
        assert_eq!(e.code, -32602);
        assert!(e.message.contains("Unknown tool"));
    }

    #[test]
    fn unknown_method_is_method_not_found() {
        let s = Session::default();
        let e = dispatch("server/discover", &json!({}), &settings(), &s).unwrap_err();
        assert_eq!(
            e.code, -32601,
            "the host probes this and needs a fast refusal"
        );
    }

    #[test]
    fn money_tools_refuse_without_confirm() {
        let s = Session::default();
        for (tool, args) in [
            ("buy", json!({"event":"e","option":"o","stake":1})),
            ("sell", json!({"position":"p"})),
            ("claim", json!({"mission":"m"})),
        ] {
            let e = dispatch(
                "tools/call",
                &json!({"name": tool, "arguments": args}),
                &settings(),
                &s,
            )
            .unwrap_err();
            assert!(
                e.message.contains("confirm=true"),
                "{} must refuse without confirm: {}",
                tool,
                e.message
            );
        }
    }

    #[test]
    fn money_tool_dry_run_does_not_touch_the_network() {
        let s = Session::default();
        // Base URL points at a closed port; a successful dry_run proves no I/O.
        let r = dispatch(
            "tools/call",
            &json!({
                "name": "buy",
                "arguments": {"event":"e1","option":"o1","stake":100,"confirm":true,"dry_run":true}
            }),
            &settings(),
            &s,
        )
        .unwrap();
        let text = r["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("POST"));
        assert!(text.contains("/events/e1/buy"));
        assert!(text.contains("stake"));
        assert!(r.get("isError").is_none());
    }

    #[test]
    fn tool_failure_is_reported_as_iserror_not_protocol_error() {
        let s = Session::default();
        // Reachable failure: nothing listens on the port.
        let r = dispatch(
            "tools/call",
            &json!({"name": "missions", "arguments": {}}),
            &settings(),
            &s,
        )
        .unwrap();
        assert_eq!(r["isError"], true, "a ran-and-failed tool must set isError");
        assert!(r["content"][0]["text"].is_string());
    }
}
