//! Entry point: parse, resolve settings, dispatch.
//!
//! Layering: `cli` parses, `config` resolves settings, `client` builds requests,
//! `transport` performs them, `guard` gates the ones that move money, `output`
//! renders, `mcp` exposes the same set over JSON-RPC. Nothing here does HTTP
//! itself except through `transport`.

mod cli;
mod client;
mod config;
mod doctor;
mod estimate;
mod guard;
mod import;
mod mcp;
mod output;
mod schedule;
mod session;
mod transport;
mod wave;
mod wire;

use std::io::IsTerminal;

use clap::Parser;

use cli::{Cli, Command};
use client::Api;
use config::Settings;
use session::Session;
use transport::Transport;
use wire::WireRequest;

/// Exit codes. Chosen so a wrapper can distinguish causes without parsing text.
///
/// 4 and 5 are the important ones: they separate "the request never left the
/// machine" (safe to retry) from "it may have been processed" (not safe).
mod code {
    pub const OK: i32 = 0;
    /// Expected failure: the server refused, or the data was not usable.
    pub const FAILED: i32 = 1;
    /// Usage error (clap uses 2 as well).
    pub const USAGE: i32 = 2;
    /// The operator declined the confirmation prompt.
    pub const DECLINED: i32 = 3;
    /// Authentication is missing or no longer valid.
    pub const AUTH: i32 = 6;
    /// The request definitely never reached the server; retrying is safe.
    pub const NOT_SENT: i32 = 4;
    /// The request may have been processed; retrying is not safe.
    pub const OUTCOME_UNKNOWN: i32 = 5;
}

fn main() {
    let cli = Cli::parse();
    std::process::exit(run(cli));
}

fn run(cli: Cli) -> i32 {
    let (settings, origins) = match config::resolve(&cli.global.overrides()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("config error: {}", e);
            return code::FAILED;
        }
    };

    if cli.global.verbose {
        eprintln!("base = {}", settings.base);
        for (k, src) in &origins {
            eprintln!("{:<14} <- {}", k, src.label());
        }
    }

    match cli.command {
        Command::Doctor => cmd_doctor(&settings, origins, &cli.global),
        Command::Me => read(&settings, &cli.global, |api| api.me()),
        Command::Config => read(&settings, &cli.global, |api| api.config()),
        Command::Mcp => mcp::serve(settings),
        Command::Auth(c) => cmd_auth(c, &settings, &cli.global),
        Command::Events(c) => cmd_events(c, &settings, &cli.global),
        Command::Positions(c) => cmd_positions(c, &settings, &cli.global),
        Command::Missions(c) => cmd_missions(c, &settings, &cli.global),
        Command::Ledger(c) => cmd_ledger(c, &settings, &cli.global),
        Command::Notices(c) => cmd_notices(c, &settings, &cli.global),
        Command::Rankings(c) => cmd_rankings(c, &settings, &cli.global),
        Command::Users(c) => cmd_users(c, &settings, &cli.global),
        Command::Blocks(c) => cmd_blocks(c, &settings, &cli.global),
        Command::Follow(a) => cmd_follow(a, &settings, &cli.global),
        Command::Block(a) => cmd_block(a, &settings, &cli.global),
        Command::Buy(a) => cmd_buy(a, &settings, &cli.global),
        Command::Sell(a) => cmd_sell(a, &settings, &cli.global),
        Command::Claim(a) => cmd_claim(a, &settings, &cli.global),
        Command::Transfer(a) => cmd_transfer(a, &settings, &cli.global),
        Command::InviteClaim(a) => cmd_invite_claim(a, &settings, &cli.global),
        Command::GiftExchange(a) => cmd_gift_exchange(a, &settings, &cli.global),
        Command::Bonus(c) => cmd_bonus(c, &settings, &cli.global),
        Command::Wave(c) => cmd_wave(c, &settings, &cli.global),
        Command::Quiz(c) => cmd_quiz(c, &settings, &cli.global),
        Command::Schedule(c) => cmd_schedule(c, &settings, &cli.global),
    }
}

// ---- plumbing -------------------------------------------------------------

/// Build the transport for this run. `--token` wins over the stored session:
/// that is the injection path for a token established elsewhere.
/// Build the transport for this run.
///
/// Token precedence is `--token` > `SAKIYOMI_TOKEN`/config file > stored session.
/// The env layer matters: without it, `export SAKIYOMI_TOKEN=...` would be
/// resolved into settings but never actually sent, which is exactly the
/// documented injection path failing silently.
fn make_transport(settings: &Settings, g: &cli::Global, session: &Session) -> Transport {
    let token = g
        .token
        .clone()
        .filter(|t| !t.trim().is_empty())
        .or_else(|| settings.token.clone().filter(|t| !t.trim().is_empty()))
        .or_else(|| session.token().map(|s| s.to_string()));
    Transport::new(
        settings.base.clone(),
        token,
        std::time::Duration::from_secs(settings.timeout_secs),
    )
}

/// Whether any token source is available for this run.
fn has_any_token(settings: &Settings, g: &cli::Global, session: &Session) -> bool {
    g.token.as_deref().is_some_and(|t| !t.trim().is_empty())
        || settings
            .token
            .as_deref()
            .is_some_and(|t| !t.trim().is_empty())
        || session.token().is_some()
}

fn load_session(settings: &Settings) -> Result<Session, i32> {
    match session::load(&settings.state_path) {
        Ok(s) => Ok(s),
        Err(e) => {
            eprintln!("state error: {}", e);
            eprintln!("hint: delete the file or pass --state to use another path");
            Err(code::FAILED)
        }
    }
}

fn emit(g: &cli::Global, body: &serde_json::Value) {
    if g.table {
        print!("{}", output::to_table(body));
    } else {
        println!("{}", output::to_json(body));
    }
}

/// Send one request and print the outcome. Returns the exit code and, when a
/// body came back, the parsed body so a caller can inspect it.
fn perform(
    settings: &Settings,
    g: &cli::Global,
    req: &WireRequest,
) -> (i32, Option<serde_json::Value>) {
    perform_with(settings, g, req, true)
}

/// `perform` with the printing step optional.
///
/// A command that issues several requests and merges them must print once, at
/// the end; if every request printed on its own the caller would receive several
/// JSON documents back to back and could not parse any of them.
fn perform_with(
    settings: &Settings,
    g: &cli::Global,
    req: &WireRequest,
    print: bool,
) -> (i32, Option<serde_json::Value>) {
    let session = match load_session(settings) {
        Ok(s) => s,
        Err(c) => return (c, None),
    };
    let transport = make_transport(settings, g, &session);

    let resp = match transport.send(req) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {}", e);
            return (code::FAILED, None);
        }
    };

    match client::classify(resp, transport.has_token(), req.path == "/login") {
        Ok(r) => {
            if print {
                emit(g, &r.body);
            }
            (code::OK, Some(r.body))
        }
        Err(e) => {
            eprintln!("error: {}", e);
            let c = match &e {
                client::ApiError::Unauthorized { .. } => code::AUTH,
                // A 401 without a token is not a "dead session" internally, but
                // for the caller the cause and the fix are the same, so the exit
                // code is shared.
                client::ApiError::Status {
                    status: 401, body, ..
                } => {
                    if print {
                        emit(g, body);
                    }
                    code::AUTH
                }
                client::ApiError::Status { body, .. } => {
                    if print {
                        emit(g, body);
                    }
                    code::FAILED
                }
                client::ApiError::Transport(_) => code::FAILED,
            };
            (c, None)
        }
    }
}

/// Read-only command: build, honour --dry-run, send, print.
fn read<F>(settings: &Settings, g: &cli::Global, build: F) -> i32
where
    F: FnOnce(&Api) -> WireRequest,
{
    read_body(settings, g, build).0
}

fn read_body<F>(settings: &Settings, g: &cli::Global, build: F) -> (i32, Option<serde_json::Value>)
where
    F: FnOnce(&Api) -> WireRequest,
{
    let api = Api::new(settings.app_version.clone());
    let req = build(&api);
    if g.dry_run {
        let session = load_session(settings).unwrap_or_default();
        let transport = make_transport(settings, g, &session);
        print!(
            "{}",
            guard::describe(&req, transport.base(), session.token())
        );
        return (code::OK, None);
    }
    perform(settings, g, &req)
}

// ---- doctor ---------------------------------------------------------------

fn cmd_doctor(
    settings: &Settings,
    origins: Vec<(&'static str, config::Source)>,
    g: &cli::Global,
) -> i32 {
    let session = match load_session(settings) {
        Ok(s) => s,
        Err(c) => return c,
    };
    // --token injects a token for this run only; doctor must see it.
    let mut effective = session.clone();
    if let Some(t) = g.token.clone().filter(|t| !t.trim().is_empty()) {
        effective.session_token = Some(t);
        effective.acquired_via = Some("injected".into());
    }
    let report = doctor::run(settings, origins, &effective);
    print!("{}", report.to_text());
    if report.all_required_ok() {
        code::OK
    } else {
        code::FAILED
    }
}

// ---- auth -----------------------------------------------------------------

fn cmd_auth(c: cli::AuthCmd, settings: &Settings, g: &cli::Global) -> i32 {
    match c {
        cli::AuthCmd::Status => {
            let session = match load_session(settings) {
                Ok(s) => s,
                Err(c) => return c,
            };
            let view = serde_json::json!({
                "base": settings.base,
                "state_path": settings.state_path.display().to_string(),
                "token_present": session.token().is_some(),
                "token_length": session.token_len(),
                "auth_provider": session.auth_provider,
                "acquired_via": session.acquired_via,
            });
            emit(g, &view);
            code::OK
        }
        cli::AuthCmd::ImportToken(args) => import::import_token(settings, g, &args),
        cli::AuthCmd::Logout(args) => cmd_logout(args, settings, g),
    }
}

fn cmd_logout(args: cli::AuthLogoutArgs, settings: &Settings, g: &cli::Global) -> i32 {
    let session = match load_session(settings) {
        Ok(s) => s,
        Err(c) => return c,
    };
    if session.token().is_none() {
        println!(
            "{}",
            output::to_json(&serde_json::json!({"ok": true, "note": "no session stored"}))
        );
        return code::OK;
    }
    let api = Api::new(settings.app_version.clone());
    let req = api.logout();

    if g.dry_run {
        let transport = make_transport(settings, g, &session);
        print!(
            "{}",
            guard::describe(&req, transport.base(), session.token())
        );
        return code::OK;
    }

    let transport = make_transport(settings, g, &session);
    let server_ok = match transport.send(&req) {
        Ok(r) if (200..300).contains(&r.status) => true,
        Ok(r) => {
            eprintln!("warning: POST /logout returned HTTP {}", r.status);
            false
        }
        Err(e) => {
            eprintln!("warning: POST /logout failed: {}", e);
            false
        }
    };

    if !server_ok && !args.forget {
        eprintln!("local session kept; pass --forget to remove it anyway");
        return code::FAILED;
    }
    match session::clear(&settings.state_path) {
        Ok(()) => {
            emit(
                g,
                &serde_json::json!({
                    "ok": true,
                    "server": if server_ok { "accepted" } else { "call failed" },
                    "local_session": "cleared",
                }),
            );
            code::OK
        }
        Err(e) => {
            eprintln!("error: {}", e);
            code::FAILED
        }
    }
}

// ---- reads ----------------------------------------------------------------

/// How many ids one `GET /events?ids=` accepts.
///
/// Measured on the live server: 50 ids return 200 and 51 return 400 with
/// 「一覧の続きを読み込めませんでした。上に引っぱって更新してください」.
/// The limit is the count, not the URL length — 50 long ids (URL 299 chars)
/// pass while 51 short ids (URL 287 chars) fail. `next_ids` commonly carries
/// several hundred entries, so passing the whole list through unchanged fails
/// every time.
const MAX_IDS_PER_REQUEST: usize = 50;

/// `events list`, with the id batches split to the server's limit and merged.
///
/// The merge is not cosmetic: each batch answers `{events}` and nothing else, so
/// the results are concatenated in the order the ids were asked for and
/// returned under the same shape a single request produces. Anything the server
/// returns beyond `events` (today only `next_ids`, and only on a full-list call)
/// is carried over from the first response so the paging contract is unchanged.
fn cmd_events_list(a: cli::EventsListArgs, settings: &Settings, g: &cli::Global) -> i32 {
    if a.stake == Some(0) {
        eprintln!("error: --stake must be greater than 0");
        return code::USAGE;
    }
    if a.ids.is_empty() {
        return read_annotated(settings, g, a.stake, |api| api.events(None, a.trial));
    }
    if a.ids.len() <= MAX_IDS_PER_REQUEST {
        return read_annotated(settings, g, a.stake, |api| {
            api.events(Some(&a.ids), a.trial)
        });
    }
    if g.dry_run {
        // Show what each batch would send, because a preview of one request
        // would hide the split that the caller is about to trigger.
        let session = load_session(settings).unwrap_or_default();
        let transport = make_transport(settings, g, &session);
        for (i, chunk) in a.ids.chunks(MAX_IDS_PER_REQUEST).enumerate() {
            let api = Api::new(settings.app_version.clone());
            let req = api.events(Some(chunk), a.trial);
            eprintln!(
                "-- batch {}/{} ({} ids)",
                i + 1,
                a.ids.len().div_ceil(MAX_IDS_PER_REQUEST),
                chunk.len()
            );
            print!(
                "{}",
                guard::describe(&req, transport.base(), session.token())
            );
        }
        eprintln!("dry-run: nothing was sent");
        return code::OK;
    }

    let chunks: Vec<&[String]> = a.ids.chunks(MAX_IDS_PER_REQUEST).collect();
    let mut merged: Vec<serde_json::Value> = Vec::new();
    let mut shape: Option<serde_json::Value> = None;
    let mut failed = 0;
    for (i, chunk) in chunks.iter().enumerate() {
        let api = Api::new(settings.app_version.clone());
        let req = api.events(Some(chunk), a.trial);
        let (rc, body) = perform_with(settings, g, &req, false);
        let Some(body) = body else {
            eprintln!(
                "error: batch {} of {} ({} ids) failed; nothing was printed for it",
                i + 1,
                chunks.len(),
                chunk.len()
            );
            failed += 1;
            continue;
        };
        if let Some(list) = body.get("events").and_then(|v| v.as_array()) {
            merged.extend(list.iter().cloned());
        }
        if shape.is_none() {
            shape = Some(body);
        } else if let Some(next) = body.get("next_ids") {
            // Only the first response defines the paging cursor; later batches
            // answer without one, and letting them overwrite it would make the
            // caller believe the list ended.
            let _ = next;
        }
        let _ = rc;
    }

    if failed > 0 {
        eprintln!("error: {} of {} batches failed", failed, chunks.len());
        return code::FAILED;
    }

    let mut out = shape.unwrap_or_else(|| serde_json::json!({ "events": [] }));
    if let Some(obj) = out.as_object_mut() {
        obj.insert("events".to_string(), serde_json::Value::Array(merged));
    }
    if let Some(stake) = a.stake {
        // Annotated after the merge so the split is invisible to the caller:
        // every option gets the same fields no matter how many batches it took.
        estimate::annotate_events(&mut out, stake);
    }
    emit(g, &out);
    code::OK
}

/// `read`, plus the per-option buy estimate when `--stake` was given.
fn read_annotated<F>(settings: &Settings, g: &cli::Global, stake: Option<u64>, build: F) -> i32
where
    F: FnOnce(&Api) -> WireRequest,
{
    // `perform_with(.., false)` keeps the first body from being printed: the
    // caller prints the annotated document once, at the end.
    let api = Api::new(settings.app_version.clone());
    let req = build(&api);
    if g.dry_run {
        let session = load_session(settings).unwrap_or_default();
        let transport = make_transport(settings, g, &session);
        print!(
            "{}",
            guard::describe(&req, transport.base(), session.token())
        );
        return code::OK;
    }
    let (rc, body) = perform_with(settings, g, &req, false);
    if rc != code::OK {
        return rc;
    }
    let Some(mut body) = body else {
        return rc;
    };
    // Printing is suppressed above in every case, so it happens here whether or
    // not an estimate was asked for; returning early would print nothing at all.
    if let Some(stake) = stake {
        estimate::annotate_events(&mut body, stake);
    }
    emit(g, &body);
    rc
}

fn cmd_events(c: cli::EventsCmd, settings: &Settings, g: &cli::Global) -> i32 {
    match c {
        cli::EventsCmd::List(a) => cmd_events_list(a, settings, g),
        cli::EventsCmd::Show(a) => {
            // `/events/{id}` returns `{event, series}` and answers 200 even for a
            // closed event, which `/events?ids=` does not — that is why this is a
            // separate request shape and not a batch lookup.
            //
            // A 404 here is a real "no such event". The success body always carries
            // an `event` key, so its absence is treated as a failure rather than
            // as an empty success.
            let (rc, body) = read_body(settings, g, |api| api.event(&a.id));
            if rc == code::OK {
                let missing = body
                    .as_ref()
                    .map(|b| b.get("event").is_none())
                    .unwrap_or(false);
                if missing {
                    eprintln!("error: response contained no 'event' object");
                    return code::FAILED;
                }
            }
            rc
        }
        cli::EventsCmd::History(a) => read(settings, g, |api| api.event_history(&a.id, &a.period)),
        cli::EventsCmd::Live => read(settings, g, |api| api.live_board()),
        cli::EventsCmd::Categories(a) => read(settings, g, |api| api.categories(a.trial)),
        cli::EventsCmd::Subcategories(a) => read(settings, g, |api| api.subcategories(&a.category)),
        cli::EventsCmd::Updown(a) => {
            // The app does not call this with an event id: the only call site is
            // `useUpDownChart(r15, ...)` where r15 comes from
            // `source === 'updown' ? TABLE[subcategory] : null` (bundle
            // 749599-749622). So the correct argument is unverified, and a 404
            // must not be read as "that event does not exist".
            let (rc, _) = read_body(settings, g, |api| api.updown(&a.id, a.live));
            if rc == code::FAILED {
                eprintln!(
                    "note: the bundle never calls /updown with an event id, so a 404 here \
                     means the argument is unverified — not that the event is missing.\n\
                     See analysis/api-contracts.md §2."
                );
            }
            rc
        }
    }
}

fn cmd_positions(c: cli::PositionsCmd, settings: &Settings, g: &cli::Global) -> i32 {
    match c {
        cli::PositionsCmd::List(a) => read(settings, g, |api| api.positions(&a.tab)),
        cli::PositionsCmd::Claim(a) => cmd_positions_claim(a, settings, g),
    }
}

/// `positions claim` — collect settled payouts.
///
/// One position id claims exactly that one. `all` reads `GET /positions?tab=result`
/// first and claims every `won` row whose `unclaimed` is 1, in id order, because
/// the server has no bulk route (measured: the bundle only builds
/// `/positions/{id}/claim`). Each claim is its own confirmed action so a run that
/// stops part-way leaves an obvious record instead of an ambiguous one.
fn cmd_positions_claim(a: cli::PositionsClaimArgs, settings: &Settings, g: &cli::Global) -> i32 {
    if a.currency != "point" && a.currency != "coin" {
        eprintln!("error: --currency must be 'point' or 'coin'");
        return code::USAGE;
    }

    if a.position == "all" {
        // The list is read only to find candidates; printing it too would put
        // a positions document in front of the claim results.
        let list_req = {
            let api = Api::new(settings.app_version.clone());
            api.positions("result")
        };
        if g.dry_run {
            let session = load_session(settings).unwrap_or_default();
            let transport = make_transport(settings, g, &session);
            print!(
                "{}",
                guard::describe(&list_req, transport.base(), session.token())
            );
            eprintln!("dry-run: nothing was sent");
            return code::OK;
        }
        let (rc, body) = perform_with(settings, g, &list_req, false);
        if rc != code::OK {
            return rc;
        }
        let Some(body) = body else {
            return if g.dry_run { code::OK } else { code::FAILED };
        };
        let pending: Vec<String> = body
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
            // Not an error: "nothing to collect" is the steady state.
            println!("ok: no unclaimed payouts");
            return code::OK;
        }
        eprintln!(
            "{} unclaimed payout(s): {}",
            pending.len(),
            pending.join(", ")
        );

        let mut failures = 0;
        for id in &pending {
            let req = {
                let api = Api::new(settings.app_version.clone());
                api.claim_position(id, &a.currency)
            };
            let rc = money_action(
                settings,
                g,
                req,
                &format!("claim payout for position {}", id),
                &format!("{} of the payout in {}", a.currency, a.currency),
            );
            if rc == code::OK {
                continue;
            }
            if rc == code::DECLINED {
                // Declining is a decision, not a failure: stop quietly.
                eprintln!("stopped at position {}", id);
                return code::DECLINED;
            }
            failures += 1;
            eprintln!("position {} was not collected (exit {})", id, rc);
            if !a.keep_going {
                eprintln!("stopping; pass --keep-going to collect the rest anyway");
                return rc;
            }
        }
        return if failures == 0 {
            code::OK
        } else {
            code::FAILED
        };
    }

    let req = {
        let api = Api::new(settings.app_version.clone());
        api.claim_position(&a.position, &a.currency)
    };
    money_action(
        settings,
        g,
        req,
        &format!("claim payout for position {}", a.position),
        &format!("the payout is credited as {}", a.currency),
    )
}

fn cmd_missions(_c: cli::MissionsCmd, settings: &Settings, g: &cli::Global) -> i32 {
    read(settings, g, |api| api.missions())
}

fn cmd_ledger(c: cli::LedgerCmd, settings: &Settings, g: &cli::Global) -> i32 {
    match c {
        cli::LedgerCmd::List(a) => read(settings, g, |api| api.ledger(&a.currency)),
    }
}

fn cmd_notices(_c: cli::NoticesCmd, settings: &Settings, g: &cli::Global) -> i32 {
    read(settings, g, |api| api.notices())
}

fn cmd_rankings(c: cli::RankingsCmd, settings: &Settings, g: &cli::Global) -> i32 {
    match c {
        cli::RankingsCmd::List(a) => read(settings, g, |api| api.rankings(&a.period)),
    }
}

fn cmd_users(c: cli::UsersCmd, settings: &Settings, g: &cli::Global) -> i32 {
    match c {
        cli::UsersCmd::Show(a) => read(settings, g, |api| api.user_profile(&a.user, &a.period)),
        cli::UsersCmd::Positions(a) => read(settings, g, |api| api.user_positions(&a.user, &a.tab)),
    }
}

fn cmd_blocks(c: cli::BlocksCmd, settings: &Settings, g: &cli::Global) -> i32 {
    match c {
        cli::BlocksCmd::List => read(settings, g, |api| api.blocks()),
    }
}

// ---- state changes that are not money -------------------------------------

fn cmd_follow(a: cli::FollowArgs, settings: &Settings, g: &cli::Global) -> i32 {
    let verb = if a.off { "unfollow" } else { "follow" };
    let req = {
        let api = Api::new(settings.app_version.clone());
        // The builder takes the CURRENT state: already following + --off means
        // "undo" (DELETE). Without --off the user is not following, so POST.
        api.follow(&a.user, a.off)
    };
    state_action(settings, g, req, &format!("{} {}", verb, a.user), verb)
}

fn cmd_block(a: cli::BlockArgs, settings: &Settings, g: &cli::Global) -> i32 {
    let verb = if a.off { "unblock" } else { "block" };
    let req = {
        let api = Api::new(settings.app_version.clone());
        // Same rule as follow. `--off` means "they are already blocked, undo it".
        api.block(&a.user, a.off)
    };
    state_action(settings, g, req, &format!("{} {}", verb, a.user), verb)
}

fn state_action(
    settings: &Settings,
    g: &cli::Global,
    req: WireRequest,
    what: &str,
    detail: &str,
) -> i32 {
    let session = match load_session(settings) {
        Ok(s) => s,
        Err(c) => return c,
    };
    let transport = make_transport(settings, g, &session);

    if g.dry_run {
        print!(
            "{}",
            guard::describe(&req, transport.base(), session.token())
        );
        eprintln!("dry-run: nothing was sent");
        return code::OK;
    }
    if !has_any_token(settings, g, &session) {
        eprintln!(
            "error: this action needs a session. Run `sakiyomi auth import-token` or pass --token."
        );
        return code::AUTH;
    }
    if !g.yes && !ask(what, detail) {
        return code::DECLINED;
    }
    perform(settings, g, &req).0
}

// ---- bonus / minigame / quiz ----------------------------------------------]

/// `bonus status`: read the ad and bonus offer the app shows on the missions
/// screen. `GET /ads/reward` also reports whether the daily login bonus is still
/// available, which is why it doubles as the status view for `bonus`.
fn cmd_bonus(c: cli::BonusCmd, settings: &Settings, g: &cli::Global) -> i32 {
    match c {
        cli::BonusCmd::Status => {
            // `perform_with(.., false)` keeps the raw reply from being printed:
            // the caller prints the stripped document once, at the end. Printing
            // the original first would show the ad fields anyway.
            let req = {
                let api = Api::new(settings.app_version.clone());
                api.ads_reward()
            };
            if g.dry_run {
                let session = load_session(settings).unwrap_or_default();
                let transport = make_transport(settings, g, &session);
                print!(
                    "{}",
                    guard::describe(&req, transport.base(), session.token())
                );
                eprintln!("dry-run: nothing was sent");
                return code::OK;
            }
            let (rc, body) = perform_with(settings, g, &req, false);
            if rc != code::OK {
                return rc;
            }
            let Some(mut body) = body else {
                return rc;
            };
            strip_ad_fields(&mut body);
            emit(g, &body);
            rc
        }
    }
}

/// Drop the fields that only describe the rewarded-video offer.
///
/// The CLI does not show ads and cannot: the grant is gated on Google's SSV
/// signature, which only Google can produce, so a count of how many videos were
/// watched, the ad unit and the publisher-side user id are all noise here. The
/// login and minigame bonuses happen to travel in the same reply, so the request
/// stays; only the ad-shaped keys are removed.
///
/// Removed: `enabled` (whether the offer is on), `reward_coins`, `daily_limit`,
/// `used_today`, `remaining`, `unit_id`, `user_ref`, `result` (the outcome of the
/// last watched video), and `win_bonus` / `win_bonus_coins` (the same rewarded
/// video offered for a just-settled win).
pub(crate) fn strip_ad_fields(body: &mut serde_json::Value) {
    const AD_KEYS: [&str; 12] = [
        "enabled",
        "reward_coins",
        "daily_limit",
        "used_today",
        "remaining",
        "unit_id",
        "user_ref",
        "result",
        // The win bonus is the same rewarded video, offered when a settled
        // position won: `win_bonus.event_id` is the event it applies to.
        "win_bonus",
        "win_bonus_coins",
        // The minigame bonus is the video offered after a clear, so it is the
        // same ad again rather than a minigame figure.
        "minigame_bonus",
        "minigame_bonus_coins",
    ];
    if let Some(obj) = body.as_object_mut() {
        for key in AD_KEYS {
            obj.remove(key);
        }
        obj.remove("ads");
    }
}

/// `wave status`: the same offer, without a play id.
fn cmd_wave(c: cli::WaveCmd, settings: &Settings, g: &cli::Global) -> i32 {
    match c {
        cli::WaveCmd::Status => cmd_wave_status(settings, g),
        cli::WaveCmd::Play(a) => cmd_wave_play(a, settings, g),
    }
}

/// `wave status` — two separate counters, reported separately.
///
/// The response of a finished round carries `today`, but `/ads/reward` does not:
/// its `used_today` counts *ads*, not minigame clears. Reporting only that value
/// made a second clear of the day look like a bug, because the round says
/// `free_done: true` while the ad counter still reads 0. This joins the two
/// sources so each number is labelled with what it counts.
fn cmd_wave_status(settings: &Settings, g: &cli::Global) -> i32 {
    // Both reads are collected and printed once, at the end; letting each print
    // would put two JSON documents in front of the caller.
    let ads_req = {
        let api = Api::new(settings.app_version.clone());
        api.ads_reward()
    };
    if g.dry_run {
        let session = load_session(settings).unwrap_or_default();
        let transport = make_transport(settings, g, &session);
        print!(
            "{}",
            guard::describe(&ads_req, transport.base(), session.token())
        );
        eprintln!("dry-run: nothing was sent");
        return code::OK;
    }
    let (rc, body) = perform_with(settings, g, &ads_req, false);
    if rc != code::OK {
        return rc;
    }
    let Some(ads) = body else {
        return code::FAILED;
    };

    // The minigame's own count is only visible through the ledger, so read it
    // rather than infer it from the ad counter.
    let ledger_req = {
        let api = Api::new(settings.app_version.clone());
        api.ledger("coin")
    };
    let (lrc, ledger) = perform_with(settings, g, &ledger_req, false);
    let minigame_coins = if lrc == code::OK {
        ledger
            .as_ref()
            .map(|l| crate::schedule::sum_today(l, "minigame"))
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    // The ad-shaped keys are dropped for the same reason `bonus status` drops
    // them: the CLI cannot watch an ad, so reporting how many were watched only
    // invites the reading that it could.
    let mut out = ads.clone();
    strip_ad_fields(&mut out);
    if let Some(obj) = out.as_object_mut() {
        obj.insert(
            "minigame".to_string(),
            serde_json::json!({
                "coins_today": minigame_coins.iter().map(|e| e.0).sum::<i64>(),
                "entries": minigame_coins.iter().map(|(_, at)| at.clone()).collect::<Vec<_>>(),

            }),
        );
    }
    emit(g, &out);
    code::OK
}

/// `wave play`: one round, start to finish, without a screen.
///
/// The order matters and is not cosmetic:
///
/// 1. `start` returns the level and a `play_id`. The level is the only input.
/// 2. The tap list is computed locally from that level (see [`wave`]).
/// 3. The server replays the list when `finish` arrives, and treats a tap whose
///    time has not yet passed as a miss. So the finish call waits until the last
///    tap is in the past; `--wait` overrides the computed value.
///
/// Skipping step 3 was measured to produce `cleared: false` with `best_row: 8`
/// — it looks like a solver bug but is the server refusing to rewind the clock.
fn cmd_wave_play(a: cli::WavePlayArgs, settings: &Settings, g: &cli::Global) -> i32 {
    let session = match load_session(settings) {
        Ok(s) => s,
        Err(c) => return c,
    };
    let transport = make_transport(settings, g, &session);

    let start = {
        let api = Api::new(settings.app_version.clone());
        api.minigame_start(a.rules_version)
    };
    if g.dry_run {
        print!(
            "{}",
            guard::describe(&start, transport.base(), session.token())
        );
        eprintln!("dry-run: nothing was sent");
        return code::OK;
    }
    if !has_any_token(settings, g, &session) {
        eprintln!(
            "error: this action needs a session. Pass --token or run `sakiyomi auth import-token`."
        );
        return code::AUTH;
    }

    let started = match transport.send(&start) {
        Ok(r) if (200..300).contains(&r.status) => r,
        Ok(r) => {
            eprintln!(
                "error: POST /minigame/wave/start returned HTTP {}",
                r.status
            );
            let msg = crate::output::to_json(&r.body);
            eprintln!("{}", msg);
            return code::FAILED;
        }
        Err(e) => {
            eprintln!("error: could not start a round: {}", e);
            return code::NOT_SENT;
        }
    };

    let play_id = match started.body.get("play_id").and_then(|v| v.as_u64()) {
        Some(id) => id,
        None => {
            eprintln!("error: the start reply has no play_id");
            return code::FAILED;
        }
    };

    let level = match wave::Level::from_start(&started.body) {
        Some(l) => l,
        None => {
            eprintln!("error: the start reply carries no usable level; refusing to invent one");
            return code::FAILED;
        }
    };

    let taps = match level.solve() {
        Some(t) => t,
        None => {
            eprintln!("error: this level could not be solved; nothing was finished");
            eprintln!("note: the round is left unfinished on the server, which pays nothing");
            return code::FAILED;
        }
    };

    let wait_secs = if a.wait > 0 {
        a.wait
    } else {
        wave::Level::settle_ms(&taps).div_ceil(1000)
    };
    eprintln!(
        "round {}: {} taps, waiting {}s so the replay sees them in the past",
        play_id,
        taps.len(),
        wait_secs
    );
    std::thread::sleep(std::time::Duration::from_secs(wait_secs));

    let finish = {
        let api = Api::new(settings.app_version.clone());
        api.minigame_finish(&play_id.to_string(), &taps)
    };
    let outcome = guard::MoneyGuard::new(&transport, finish, "finish one wave round").run();
    let (rc, msg) = outcome.report();
    println!("{}", msg);
    // A cleared round that pays nothing is the normal second clear of the day.
    // Say why, so it does not read as a failure. The text after "ok: " is the
    // response body, which is the only place the round's own day counter lives.
    if rc == 0 {
        if let Some(note) = no_payout_note(&msg) {
            eprintln!("note: {}", note);
        }
    }

    rc
}

/// `quiz`: create one (POST /quizzes).
///
/// Options and their probabilities come in as repeated flags, so they are paired
/// here rather than in the CLI layer: a probability without a matching option is
/// a usage error, not something to guess at.
fn cmd_quiz(c: cli::QuizCmd, settings: &Settings, g: &cli::Global) -> i32 {
    match c {
        cli::QuizCmd::List(a) => cmd_quiz_list(a, settings, g),
        cli::QuizCmd::Create(a) => cmd_quiz_create(a, settings, g),
    }
}

/// `quiz list` — the caller's own quizzes with their review state.
///
/// The server returns every quiz the account has submitted, including the
/// reviewer's note and whether it was adopted (which is what decides the bonus).
/// It does no filtering, so `--status` filters here rather than in the request.
fn cmd_quiz_list(a: cli::QuizListArgs, settings: &Settings, g: &cli::Global) -> i32 {
    if let Some(s) = a.status.as_deref() {
        if !matches!(s, "pending" | "approved" | "rejected") {
            eprintln!("error: --status must be pending, approved or rejected");
            return code::USAGE;
        }
    }
    let list_req = {
        let api = Api::new(settings.app_version.clone());
        api.quizzes_mine()
    };
    if g.dry_run {
        let session = load_session(settings).unwrap_or_default();
        let transport = make_transport(settings, g, &session);
        print!(
            "{}",
            guard::describe(&list_req, transport.base(), session.token())
        );
        eprintln!("dry-run: nothing was sent");
        return code::OK;
    }
    // The filtered result is printed once, at the end, so an unfiltered body
    // never appears in front of the filtered one.
    let (rc, body) = perform_with(settings, g, &list_req, false);
    if rc != code::OK {
        return rc;
    }
    let Some(mut body) = body else {
        return code::FAILED;
    };
    if let Some(want) = a.status.as_deref() {
        let kept: Vec<serde_json::Value> = body
            .get("quizzes")
            .and_then(|v| v.as_array())
            .map(|rows| {
                rows.iter()
                    .filter(|q| q.get("status").and_then(|s| s.as_str()) == Some(want))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if let Some(obj) = body.as_object_mut() {
            obj.insert("quizzes".to_string(), serde_json::Value::Array(kept));
        }
    }
    emit(g, &body);
    code::OK
}

fn cmd_quiz_create(a: cli::QuizArgs, settings: &Settings, g: &cli::Global) -> i32 {
    if a.options.len() < 2 {
        eprintln!("error: at least two --option values are required");
        return code::USAGE;
    }
    if !a.probs.is_empty() && a.probs.len() != a.options.len() {
        eprintln!(
            "error: {} --prob values for {} --option values; give one per option or none",
            a.probs.len(),
            a.options.len()
        );
        return code::USAGE;
    }
    let options: Vec<(String, Option<u8>)> = a
        .options
        .iter()
        .enumerate()
        .map(|(i, label)| (label.clone(), a.probs.get(i).copied()))
        .collect();
    let draft = client::QuizDraft {
        title: a.title.clone(),
        description: a.description.clone(),
        options,
        closes_at: a.closes_at.clone(),
        image_url: a.image_url.clone(),
        news_url: a.news_url.clone(),
        note: a.note.clone(),
    };
    let req = {
        let api = Api::new(settings.app_version.clone());
        api.create_quiz(&draft)
    };
    let detail = format!(
        "create quiz {:?} with {} options, closing {}",
        draft.title,
        draft.options.len(),
        draft.closes_at
    );
    money_action(
        settings,
        g,
        req,
        &format!("create quiz {:?}", draft.title),
        &detail,
    )
}

// ---- schedule -------------------------------------------------------------

/// `schedule`: queue purchases for a later time and run the queue.
///
/// Nothing here makes a purchase itself except `run`, which goes through the
/// same [`guard::MoneyGuard`] a manual `buy` does. The queue file is the durable
/// part; `run` is the process that stays resident, so stopping and restarting it
/// loses nothing that was already recorded.
fn cmd_schedule(c: cli::ScheduleCmd, settings: &Settings, g: &cli::Global) -> i32 {
    match c {
        cli::ScheduleCmd::Add(a) => cmd_schedule_add(a, settings),
        cli::ScheduleCmd::List => cmd_schedule_list(settings, g),
        cli::ScheduleCmd::Remove(a) => cmd_schedule_remove(a, settings),
        cli::ScheduleCmd::Reconcile(a) => cmd_schedule_reconcile(a, settings, g),
        cli::ScheduleCmd::Run(a) => cmd_schedule_run(a, settings, g),
    }
}

fn cmd_schedule_add(a: cli::ScheduleAddArgs, settings: &Settings) -> i32 {
    // The server rejects anything outside 10..=100_000 with
    // 「コインは10〜100,000の範囲で入力してください」 (measured), so a queue that
    // accepts 1 would only fail later, after the operator has left.
    if !(10..=100_000).contains(&a.stake) {
        eprintln!(
            "error: --stake must be between 10 and 100000 (the server refuses anything else)"
        );
        return code::USAGE;
    }
    // A bare `YYYY-MM-DD HH:MM` has no offset, and the operator means their own
    // clock, not UTC. `parse_at` treats a missing offset as UTC, so the local
    // offset is appended here rather than guessed inside the parser.
    let at = match schedule::parse_at(&a.at) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: --at: {}", e);
            return code::USAGE;
        }
    };
    // An explicit offset (or `Z`) means the operator already chose a zone; a
    // bare time means their own clock. Only the time part is inspected, because
    // the date part always contains `-`.
    let explicit_offset =
        a.at.split_once('T')
            .or_else(|| a.at.split_once(' '))
            .map(|(_, t)| t.contains('+') || t.contains('-') || t.contains('Z'))
            .unwrap_or(false);
    let at = if explicit_offset {
        at
    } else {
        match local_offset_minutes() {
            Some(off) => match schedule::normalise_with_offset(&a.at, off) {
                Ok(v) => v,
                Err(_) => at,
            },
            None => {
                eprintln!(
                    "error: --at has no UTC offset and this platform does not report one; write it explicitly (e.g. 2026-12-01T12:00+09:00)"
                );
                return code::USAGE;
            }
        }
    };

    let path = settings.schedule_path.clone();
    let mut q = match schedule::Schedule::load(&path) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {}", e);
            return code::FAILED;
        }
    };
    let id = q.new_id();
    q.entries.push(schedule::Entry {
        id: id.clone(),
        at: at.clone(),
        event: a.event.clone(),
        option: a.option.clone(),
        stake: a.stake,
        state: schedule::State::Pending,
        note: None,
        updated_at: None,
    });
    if let Err(e) = q.save(&path) {
        eprintln!("error: {}", e);
        return code::FAILED;
    }
    eprintln!(
        "queued {} at {} ({} coins on event {}",
        id, at, a.stake, a.event
    );
    let entry = q.entries.last().cloned();
    if let Some(e) = entry {
        emit_entry(&e);
    }
    code::OK
}

/// The machine's UTC offset in minutes, read from the system clock rather than
/// assumed. Returns None when the platform does not expose it.
fn local_offset_minutes() -> Option<i32> {
    Some((local_utc_offset_seconds()? / 60) as i32)
}

/// Seconds the local clock is ahead of UTC, measured by comparing the system
/// clock with the same instant formatted without an offset. Reading it beats
/// assuming one: the scheduler must fire at the operator's wall clock, not at a
/// hard-coded zone.
#[cfg(windows)]
fn local_utc_offset_seconds() -> Option<i64> {
    // Windows: GetTimeZoneInformation is not reachable without a dependency, so
    // read the offset the OS exposes through TZ via the standard library's
    // local-time support instead: `SystemTime` and a civil conversion cannot
    // expose it, and a silent zero would be a wrong answer. Report unknown and
    // let `--at` require an explicit offset on Windows.
    None
}

#[cfg(not(windows))]
fn local_utc_offset_seconds() -> Option<i64> {
    // `date +%z` prints the offset as ±HHMM; parsing it is enough and needs no
    // crate. If the command or its output is not what we expect, report unknown
    // rather than invent a value.
    let out = std::process::Command::new("date")
        .arg("+%z")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let text = text.trim();
    let (sign, rest) = match text.split_at(1) {
        (s, r) if s == "+" => (1i64, r),
        (s, r) if s == "-" => (-1i64, r),
        _ => return None,
    };
    if rest.len() != 4 {
        return None;
    }
    let hh: i64 = rest[..2].parse().ok()?;
    let mm: i64 = rest[2..].parse().ok()?;
    Some(sign * (hh * 3600 + mm * 60))
}

fn cmd_schedule_list(settings: &Settings, g: &cli::Global) -> i32 {
    let path = settings.schedule_path.clone();
    match schedule::Schedule::load(&path) {
        Ok(q) => {
            let body =
                serde_json::to_value(&q).unwrap_or_else(|_| serde_json::json!({"entries": []}));
            emit(g, &body);
            code::OK
        }
        Err(e) => {
            eprintln!("error: {}", e);
            code::FAILED
        }
    }
}

fn cmd_schedule_remove(a: cli::ScheduleRemoveArgs, settings: &Settings) -> i32 {
    let path = settings.schedule_path.clone();
    let mut q = match schedule::Schedule::load(&path) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {}", e);
            return code::FAILED;
        }
    };
    let before = q.entries.len();
    q.entries.retain(|e| e.id != a.id);
    if q.entries.len() == before {
        eprintln!("error: no entry with id {}", a.id);
        return code::FAILED;
    }
    match q.save(&path) {
        Ok(()) => {
            eprintln!("removed {}", a.id);
            code::OK
        }
        Err(e) => {
            eprintln!("error: {}", e);
            code::FAILED
        }
    }
}

/// `schedule run` — wait for due entries and send them.
///
/// The loop is deliberately dumb: look at the file, sleep until the next due
/// entry (or a short poll when nothing is due), then claim it. A claimed entry is
/// written back as `in_flight` **before** the request goes out, which is what
/// makes a crash mid-flight recoverable rather than a possible double purchase.
fn cmd_schedule_run(a: cli::ScheduleRunArgs, settings: &Settings, g: &cli::Global) -> i32 {
    let path = settings.schedule_path.clone();
    let session = match load_session(settings) {
        Ok(s) => s,
        Err(c) => return c,
    };
    if !has_any_token(settings, g, &session) {
        eprintln!(
            "error: the queue needs a session. Run `sakiyomi auth import-token` or pass --token."
        );
        return code::AUTH;
    }
    let transport = make_transport(settings, g, &session);
    let started = std::time::Instant::now();

    loop {
        if a.max_seconds > 0 && started.elapsed().as_secs() >= a.max_seconds {
            eprintln!("stopping: --max-seconds elapsed");
            return code::OK;
        }
        let mut q = match schedule::Schedule::load(&path) {
            Ok(q) => q,
            Err(e) => {
                eprintln!("error: {}", e);
                return code::FAILED;
            }
        };
        let Some(due) = q.next_due().cloned() else {
            if a.once {
                eprintln!("nothing pending");
                return code::OK;
            }
            // Nothing to do: poll the file rather than spin, so a `add` from
            // another shell is picked up within a few seconds.
            std::thread::sleep(std::time::Duration::from_secs(5));
            continue;
        };

        let wait = seconds_until(&due.at);
        if wait > 0 {
            if a.once && a.max_seconds == 0 {
                // Without a deadline, `--once` would sit here forever waiting for
                // an entry that is not due yet. Say so and stop instead.
                eprintln!(
                    "next entry {} is due in {}s at {}; use `run` without --once, or wait",
                    due.id, wait, due.at
                );
                return code::OK;
            }
            let sleep = (wait.min(30) as u64).max(1);
            std::thread::sleep(std::time::Duration::from_secs(sleep));
            continue;
        }

        // Claim it: write `in_flight` first, so a crash here leaves a record that
        // says "this may have been sent" rather than one that invites a retry.
        if let Some(e) = q.find_mut(&due.id) {
            e.state = schedule::State::InFlight;
            e.updated_at = Some(schedule::now_rfc3339());
            e.note = None;
        }
        if let Err(e) = q.save(&path) {
            eprintln!("error: could not mark {} in flight: {}", due.id, e);
            return code::FAILED;
        }

        let req = {
            let api = Api::new(settings.app_version.clone());
            api.buy(&due.event, &due.option, due.stake)
        };
        eprintln!(
            "sending {}: {} coins on event {} option {} (scheduled for {})",
            due.id, due.stake, due.event, due.option, due.at
        );
        let outcome =
            guard::MoneyGuard::new(&transport, req, format!("scheduled buy {}", due.id)).run();
        let (rc, msg) = outcome.report();
        let (state, note) = match rc {
            0 => (schedule::State::Done, shorten(&msg)),
            code::DECLINED => (
                schedule::State::Pending,
                "declined at the prompt".to_string(),
            ),
            code::NOT_SENT => (schedule::State::NotSent, shorten(&msg)),
            code::OUTCOME_UNKNOWN => (schedule::State::InFlight, shorten(&msg)),
            _ => (schedule::State::Rejected, shorten(&msg)),
        };
        println!("[{}] {}", due.id, msg);

        // Re-read before writing: the file may have been edited while the
        // request was in flight, and overwriting that with a stale copy would
        // silently drop the other change.
        let mut fresh = match schedule::Schedule::load(&path) {
            Ok(q) => q,
            Err(e) => {
                eprintln!("error: {}", e);
                return code::FAILED;
            }
        };
        if let Some(e) = fresh.find_mut(&due.id) {
            e.state = state;
            e.note = Some(note);
            e.updated_at = Some(schedule::now_rfc3339());
        }
        if let Err(e) = fresh.save(&path) {
            eprintln!("error: could not record the outcome of {}: {}", due.id, e);
            return code::FAILED;
        }

        if rc != 0 && rc != code::DECLINED {
            // A refusal is recorded and not retried. Continue with the rest so
            // one bad entry does not stall the queue, but report the failure
            // through the exit code once the loop ends.
            eprintln!("entry {} ended as {:?} (exit {})", due.id, state, rc);
        }
        if a.once {
            return rc;
        }
    }
}

/// Why a successful round paid nothing, when that is what happened.
///
/// The server answers `reward: null` with `today.free_done: true` once the daily
/// clear has already been paid. Without saying so the whole thing reads like the
/// coins went missing, which is exactly how it was reported.
fn no_payout_note(msg: &str) -> Option<String> {
    let body = msg.strip_prefix("ok: ")?;
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let cleared = v.get("cleared").and_then(|c| c.as_bool()).unwrap_or(false);
    let paid = v.get("reward").map(|r| !r.is_null()).unwrap_or(false);
    if !cleared || paid {
        return None;
    }
    let free_done = v
        .get("today")
        .and_then(|t| t.get("free_done"))
        .and_then(|f| f.as_bool())
        .unwrap_or(false);
    let coins = v
        .get("today")
        .and_then(|t| t.get("coins"))
        .and_then(|c| c.as_u64())
        .unwrap_or(0);
    let free_coins = v.get("free_coins").and_then(|c| c.as_u64()).unwrap_or(0);
    if free_done {
        Some(format!(
            "cleared, but today's free clear is already paid ({} coins today, {} per day). The next clear that pays is tomorrow (JST).",
            coins, free_coins
        ))
    } else {
        Some(format!(
            "cleared, but the server paid nothing this round (free_coins {} was not granted)",
            free_coins
        ))
    }
}

fn shorten(s: &str) -> String {
    // Keep the whole message: a truncated failure reason is worse than a long
    // line, and the "no truncation" rule applies to what is written for a human.
    s.replace('\n', " ")
}

fn seconds_until(at: &str) -> i64 {
    match schedule::unix_from_rfc(at) {
        Ok(t) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            t - now
        }
        Err(_) => 0,
    }
}

fn cmd_schedule_reconcile(
    a: cli::ScheduleReconcileArgs,
    settings: &Settings,
    g: &cli::Global,
) -> i32 {
    if a.done == a.retry {
        eprintln!("error: pass exactly one of --done or --retry");
        return code::USAGE;
    }
    let path = settings.schedule_path.clone();
    let mut q = match schedule::Schedule::load(&path) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {}", e);
            return code::FAILED;
        }
    };
    let now = schedule::now_rfc3339();
    let Some(entry) = q.find_mut(&a.id) else {
        eprintln!("error: no entry with id {}", a.id);
        return code::FAILED;
    };
    entry.state = if a.done {
        schedule::State::Done
    } else {
        schedule::State::NotSent
    };
    entry.note = Some(if a.done {
        "settled by hand as done".to_string()
    } else {
        "settled by hand as retryable".to_string()
    });
    entry.updated_at = Some(now);
    match q.save(&path) {
        Ok(()) => {
            let body = serde_json::to_value(&q).unwrap_or_else(|_| serde_json::json!({}));
            emit(g, &body);
            code::OK
        }
        Err(e) => {
            eprintln!("error: {}", e);
            code::FAILED
        }
    }
}

fn emit_entry(e: &schedule::Entry) {
    let body = serde_json::to_value(e).unwrap_or_else(|_| serde_json::json!({}));
    println!("{}", output::to_json(&body));
}

/// Shared gate for every money-moving command.
fn money_action(
    settings: &Settings,
    g: &cli::Global,
    req: WireRequest,
    what: &str,
    detail: &str,
) -> i32 {
    let session = match load_session(settings) {
        Ok(s) => s,
        Err(c) => return c,
    };
    let transport = make_transport(settings, g, &session);

    // Dry-run comes first: previewing a purchase must work without a session.
    if g.dry_run {
        print!(
            "{}",
            guard::describe(&req, transport.base(), session.token())
        );
        if session.token().is_none() {
            eprintln!("note: no session token; this request would be rejected with 401");
        }
        eprintln!("dry-run: nothing was sent");
        return code::OK;
    }

    if !has_any_token(settings, g, &session) {
        eprintln!(
            "error: this action needs a session. Run `sakiyomi auth import-token` or pass --token."
        );
        return code::AUTH;
    }

    if !g.yes && !ask(what, detail) {
        return code::DECLINED;
    }

    let outcome = guard::MoneyGuard::new(&transport, req, what).run();
    let (exit, msg) = outcome.report();
    if exit == code::OK {
        println!("{}", msg);
    } else {
        eprintln!("{}", msg);
    }
    exit
}

/// Confirmation prompt. Returns true only when the operator typed `yes`.
fn ask(what: &str, detail: &str) -> bool {
    let stdin = std::io::stdin();
    let interactive = stdin.is_terminal();
    let mut input = stdin.lock();
    let mut out = std::io::stderr();
    let shown = if interactive {
        detail.to_string()
    } else {
        format!(
            "{}\n(no terminal attached; pass --yes to proceed non-interactively)",
            detail
        )
    };
    match guard::confirm(what, &shown, &mut input, &mut out) {
        Ok(true) => true,
        Ok(false) => {
            eprintln!("aborted");
            false
        }
        Err(e) => {
            eprintln!("error reading confirmation: {}", e);
            false
        }
    }
}

fn cmd_buy(a: cli::BuyArgs, settings: &Settings, g: &cli::Global) -> i32 {
    // The server accepts 10..=100_000 and refuses the rest with
    // 「コインは10〜100,000の範囲で入力してください」(measured on 2026-11-27).
    if !(10..=100_000).contains(&a.stake) {
        eprintln!(
            "error: --stake must be between 10 and 100000 (the server refuses anything else)"
        );
        return code::USAGE;
    }
    let req = {
        let api = Api::new(settings.app_version.clone());
        api.buy(&a.event, &a.option, a.stake)
    };
    if g.dry_run {
        // The estimate has to be shown *before* the purchase, which is exactly
        // what the issue asked for: the listing `prob` is not the buy price.
        // It is a separate read, so a dry-run now performs one GET (a read,
        // never a purchase) and prints the estimate next to the request.
        let session = load_session(settings).unwrap_or_default();
        let transport = make_transport(settings, g, &session);
        let board_req = a.board_request(&Api::new(settings.app_version.clone()));
        print!(
            "{}",
            guard::describe(&req, transport.base(), session.token())
        );
        if session.token().is_none() {
            eprintln!("note: no session token; this request would be rejected with 401");
        }
        // Printed after the request so the two blocks never interleave, and on
        // stdout so `--dry-run | jq` style pipes keep it.
        match transport.send(&board_req) {
            Ok(resp) if (200..300).contains(&resp.status) => match resp.body.get("event") {
                Some(event) => match estimate::buy_estimate(event, &a.option, a.stake) {
                    Some(e) => {
                        println!("estimate:");
                        println!("  buy_prob: {:.4}", e.buy_prob);
                        println!("  shares:   {}", e.shares);
                        println!("  weight:   {} / total {}", e.weight, e.total_weight);
                    }
                    None => eprintln!(
                        "warning: no estimate; option {} was not found on event {} or the board lacks weights",
                        a.option, a.event
                    ),
                },
                None => eprintln!("warning: no estimate; the event reply carried no 'event'"),
            },
            Ok(resp) => eprintln!(
                "warning: no estimate; GET /events/{} returned HTTP {}",
                a.event, resp.status
            ),
            Err(e) => eprintln!("warning: no estimate; {}", e),
        }
        eprintln!("dry-run: nothing was sent");
        return code::OK;
    }
    let detail = format!("{} x {} coins on event {}", a.option, a.stake, a.event);
    money_action(settings, g, req, &format!("buy into {}", a.event), &detail)
}

fn cmd_sell(a: cli::SellArgs, settings: &Settings, g: &cli::Global) -> i32 {
    let req = {
        let api = Api::new(settings.app_version.clone());
        api.sell(&a.position)
    };
    money_action(
        settings,
        g,
        req,
        &format!("sell position {}", a.position),
        "the whole position is sold; the app sends no amount",
    )
}

fn cmd_claim(a: cli::ClaimArgs, settings: &Settings, g: &cli::Global) -> i32 {
    let req = {
        let api = Api::new(settings.app_version.clone());
        api.claim(&a.mission)
    };
    money_action(
        settings,
        g,
        req,
        &format!("claim mission {}", a.mission),
        "claims the mission reward for this account",
    )
}

fn cmd_transfer(a: cli::TransferArgs, settings: &Settings, g: &cli::Global) -> i32 {
    let req = {
        let api = Api::new(settings.app_version.clone());
        api.transfer(&a.to, a.amount, &a.message)
    };
    let detail = format!(
        "send {} points to {} (message: {:?})",
        a.amount, a.to, a.message
    );
    money_action(settings, g, req, &format!("transfer to {}", a.to), &detail)
}

fn cmd_invite_claim(a: cli::InviteClaimArgs, settings: &Settings, g: &cli::Global) -> i32 {
    let req = {
        let api = Api::new(settings.app_version.clone());
        api.invite_claim(&a.code)
    };
    money_action(
        settings,
        g,
        req,
        &format!("claim invite code {}", a.code),
        "invite codes can usually be claimed only once per account",
    )
}

fn cmd_gift_exchange(a: cli::GiftExchangeArgs, settings: &Settings, g: &cli::Global) -> i32 {
    let items: serde_json::Value = match serde_json::from_str(&a.items) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: --items is not valid JSON: {}", e);
            return code::USAGE;
        }
    };
    let req = {
        let api = Api::new(settings.app_version.clone());
        api.gifts_exchange(&a.batch, items)
    };
    let detail = format!("exchange batch {} with items {}", a.batch, a.items);
    money_action(
        settings,
        g,
        req,
        &format!("exchange gift batch {}", a.batch),
        &detail,
    )
}

// ---- helpers --------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> serde_json::Value {
        // The shape measured from GET /ads/reward on 2026-11-27.
        serde_json::json!({
            "enabled": true,
            "reward_coins": 10,
            "daily_limit": 5,
            "used_today": 5,
            "remaining": 0,
            "unit_id": "ca-app-pub-example/example",
            "user_ref": "u000",
            "result": null,
            "win_bonus_coins": 10,
            "win_bonus": null,
            "login_bonus_coins": 30,
            "login_bonus": {"day": "2026-11-27", "available": true, "received": false},
            "minigame_bonus_coins": 10,
            "minigame_bonus": null
        })
    }

    #[test]
    fn the_ad_shaped_keys_are_removed() {
        let mut body = sample();
        strip_ad_fields(&mut body);
        let obj = body.as_object().unwrap();
        for gone in [
            "enabled",
            "reward_coins",
            "daily_limit",
            "used_today",
            "remaining",
            "unit_id",
            "user_ref",
            "result",
            "win_bonus",
            "win_bonus_coins",
            "minigame_bonus",
            "minigame_bonus_coins",
        ] {
            assert!(!obj.contains_key(gone), "{} should be gone", gone);
        }
    }

    #[test]
    fn the_login_bonus_survives() {
        let mut body = sample();
        strip_ad_fields(&mut body);
        let obj = body.as_object().unwrap();
        assert_eq!(obj["login_bonus_coins"], 30);
        assert_eq!(obj["login_bonus"]["available"], true);
        // The login bonus is the only non-ad bonus in this reply.
        assert_eq!(obj.len(), 2, "only the login fields remain");
    }

    #[test]
    fn stripping_a_non_object_is_a_no_op() {
        // A reply that is not an object must not be turned into one.
        let mut body = serde_json::json!([1, 2]);
        strip_ad_fields(&mut body);
        assert_eq!(body, serde_json::json!([1, 2]));
    }
}
