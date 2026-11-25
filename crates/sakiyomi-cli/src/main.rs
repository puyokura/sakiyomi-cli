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
mod flow;
mod guard;
mod mcp;
mod oauth;
mod output;
mod scheme;
mod session;
mod transport;
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
            emit(g, &r.body);
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
                    emit(g, body);
                    code::AUTH
                }
                client::ApiError::Status { body, .. } => {
                    emit(g, body);
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
        cli::AuthCmd::Login => flow::login(settings, g),
        cli::AuthCmd::Callback(args) => flow::callback(settings, &args.uri, args.relay.as_deref()),
        cli::AuthCmd::Register => cmd_register(),
        cli::AuthCmd::Unregister => cmd_unregister(),
        cli::AuthCmd::Logout(args) => cmd_logout(args, settings, g),
    }
}

fn cmd_register() -> i32 {
    match scheme::register() {
        Ok(msg) => {
            println!("{}", msg);
            code::OK
        }
        Err(e) => {
            eprintln!("error: {}", e);
            code::FAILED
        }
    }
}

fn cmd_unregister() -> i32 {
    match scheme::unregister() {
        Ok(msg) => {
            println!("{}", msg);
            code::OK
        }
        Err(e) => {
            eprintln!("error: {}", e);
            code::FAILED
        }
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

fn cmd_events(c: cli::EventsCmd, settings: &Settings, g: &cli::Global) -> i32 {
    match c {
        cli::EventsCmd::List(a) => read(settings, g, |api| {
            api.events(if a.ids.is_empty() { None } else { Some(&a.ids) }, a.trial)
        }),
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
    }
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
        eprintln!("error: this action needs a session. Run `sakiyomi auth login` or pass --token.");
        return code::AUTH;
    }
    if !g.yes && !ask(what, detail) {
        return code::DECLINED;
    }
    perform(settings, g, &req).0
}

// ---- money ----------------------------------------------------------------

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
        eprintln!("error: this action needs a session. Run `sakiyomi auth login` or pass --token.");
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
    let req = {
        let api = Api::new(settings.app_version.clone());
        api.buy(&a.event, &a.option, a.stake)
    };
    let detail = format!("{} x {} points on event {}", a.option, a.stake, a.event);
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
