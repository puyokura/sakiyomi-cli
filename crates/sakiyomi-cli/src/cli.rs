//! Command-line surface.
//!
//! Shape: hierarchical resource-verb (`events list`, `events show <id>`),
//! because it maps one-to-one onto the REST paths the bundle uses and keeps
//! names from colliding as endpoints are added.
//!
//! Output defaults to JSON because the primary consumer is a machine. `--table`
//! switches to a human table. There is no third format.

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "sakiyomi",
    version,
    about = "Headless client for the saKIYOMI API, reconstructed from the shipped Android bundle",
    long_about = "Every request this tool sends is derived from analysis of the released app bundle.\n\
                  Endpoints that could not be recovered are absent rather than guessed; run\n\
                  `sakiyomi doctor` to see what the server currently accepts.",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: Global,

    #[command(subcommand)]
    pub command: Command,
}

/// Flags accepted both before and after the subcommand.
#[derive(Debug, Args, Clone)]
pub struct Global {
    /// API base URL. Default: the value the app uses.
    #[arg(long, global = true, value_name = "URL")]
    pub base: Option<String>,

    /// Pre-established session token (sent as `Authorization: Bearer`).
    /// Use this instead of a password when the token was obtained elsewhere.
    #[arg(long, global = true, value_name = "TOKEN")]
    pub token: Option<String>,

    /// Config file path. Overrides SAKIYOMI_CONFIG and the OS default.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<std::path::PathBuf>,

    /// Session state file path. Overrides SAKIYOMI_STATE and the OS default.
    #[arg(long, global = true, value_name = "PATH")]
    pub state: Option<std::path::PathBuf>,

    /// Schedule file path. Overrides SAKIYOMI_SCHEDULE and the OS default
    /// (`schedule.json` beside the session file).
    #[arg(long, global = true, value_name = "PATH")]
    pub schedule: Option<std::path::PathBuf>,

    /// Request timeout in seconds.
    #[arg(long, global = true, value_name = "SECS")]
    pub timeout: Option<u64>,

    /// Print the human-readable table instead of JSON.
    #[arg(long, global = true)]
    pub table: bool,

    /// Show what would be sent, then stop. No network I/O.
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Skip the confirmation prompt for state-changing commands.
    #[arg(long, global = true)]
    pub yes: bool,

    /// Increase log detail on stderr. Never prints secrets.
    #[arg(long, global = true)]
    pub verbose: bool,
}

impl Global {
    pub fn overrides(&self) -> crate::config::Overrides {
        crate::config::Overrides {
            base: self.base.clone(),
            token: self.token.clone(),
            config: self.config.clone(),
            state: self.state.clone(),
            schedule: self.schedule.clone(),
            timeout_secs: self.timeout,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check connectivity, authentication and the public config endpoint.
    Doctor,

    /// Show the signed-in account (GET /me).
    Me,

    /// Public feature flags (GET /config). No authentication needed.
    Config,

    /// Serve MCP over stdio (initialize / tools/list / tools/call).
    Mcp,

    /// Authentication.
    #[command(subcommand)]
    Auth(AuthCmd),

    /// Prediction events.
    #[command(subcommand)]
    Events(EventsCmd),

    /// Your open positions.
    #[command(subcommand)]
    Positions(PositionsCmd),

    /// Missions and their rewards.
    #[command(subcommand)]
    Missions(MissionsCmd),

    /// Point ledger.
    #[command(subcommand)]
    Ledger(LedgerCmd),

    /// Notices.
    #[command(subcommand)]
    Notices(NoticesCmd),

    /// Rankings.
    #[command(subcommand)]
    Rankings(RankingsCmd),

    /// Users.
    #[command(subcommand)]
    Users(UsersCmd),

    /// Buy into an event option. Moves points.
    Buy(BuyArgs),

    /// Sell a position. Moves points.
    Sell(SellArgs),

    /// Claim a mission reward. Moves points.
    Claim(ClaimArgs),

    /// Transfer points to another user. Moves points.
    Transfer(TransferArgs),

    /// Claim an invite code.
    #[command(name = "invite-claim")]
    InviteClaim(InviteClaimArgs),

    /// Exchange a gift batch.
    #[command(name = "gift-exchange")]
    GiftExchange(GiftExchangeArgs),

    /// Blocked users.
    #[command(subcommand)]
    Blocks(BlocksCmd),

    /// Follow or unfollow a user.
    Follow(FollowArgs),

    /// Block or unblock a user.
    Block(BlockArgs),

    /// Daily login bonus and the extra coins offered for watching an ad.
    #[command(subcommand)]
    Bonus(BonusCmd),

    /// The wave minigame: play it headlessly and collect its coins.
    #[command(subcommand)]
    Wave(WaveCmd),

    /// Create a quiz (POST /quizzes) and inspect the ones you created.
    #[command(subcommand)]
    Quiz(QuizCmd),

    /// Queue a purchase to be sent at a specific time, and run the queue.
    #[command(subcommand)]
    Schedule(ScheduleCmd),
}

#[derive(Debug, Subcommand)]
pub enum ScheduleCmd {
    /// Queue one purchase. Saved to a file; nothing is sent until `run`.
    Add(ScheduleAddArgs),
    /// Show the queue.
    List,
    /// Drop an entry from the queue (nothing is sent either way).
    Remove(ScheduleRemoveArgs),
    /// Wait for due entries and send them. This is the process that stays resident.
    Run(ScheduleRunArgs),
    /// Clear the `in_flight`/`not_sent` marks after you have checked by hand.
    Reconcile(ScheduleReconcileArgs),
}

#[derive(Debug, Args)]
pub struct ScheduleAddArgs {
    /// When to buy, e.g. 2026-12-01T12:00+09:00. Without an offset it is read as
    /// local time. Stored as UTC, so the queue is unambiguous.
    #[arg(long, value_name = "TIME")]
    pub at: String,
    /// Event id.
    #[arg(long)]
    pub event: String,
    /// Option id.
    #[arg(long)]
    pub option: String,
    /// Coins to stake. The server debits coins, not points, and the
    /// request carries no currency field (bundle 665770).
    #[arg(long, value_name = "COINS")]
    pub stake: u64,
}

#[derive(Debug, Args)]
pub struct ScheduleRemoveArgs {
    /// Entry id, from `schedule list`.
    pub id: String,
}

#[derive(Debug, Args)]
pub struct ScheduleRunArgs {
    /// Stop once nothing is pending instead of waiting for more.
    #[arg(long)]
    pub once: bool,
    /// Stop after this many seconds even if entries remain. 0 means no limit.
    #[arg(long, default_value = "0", value_name = "SECS")]
    pub max_seconds: u64,
}

#[derive(Debug, Args)]
pub struct ScheduleReconcileArgs {
    /// Entry id to settle.
    pub id: String,
    /// Record it as done (the server did take the points).
    #[arg(long)]
    pub done: bool,
    /// Record it as not sent again, so `run` will retry it.
    #[arg(long)]
    pub retry: bool,
}

#[derive(Debug, Subcommand)]
pub enum QuizCmd {
    /// Create a quiz (POST /quizzes). Points move only if it is adopted.
    Create(QuizArgs),
    /// Your own quizzes and their review status (GET /quizzes/mine).
    List(QuizListArgs),
}

#[derive(Debug, Args)]
pub struct QuizListArgs {
    /// Only show quizzes in this review state: pending, approved or rejected.
    #[arg(long)]
    pub status: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum BlocksCmd {
    /// List blocked users (GET /blocks).
    List,
}

/// `bonus`: the daily login bonus, which is not an ad.
#[derive(Debug, Subcommand)]
pub enum BonusCmd {
    /// Show the daily login bonus state (GET /ads/reward).
    Status,
}

/// `wave`: the endless-runner whose coins are paid out on a clear.
#[derive(Debug, Subcommand)]
pub enum WaveCmd {
    /// Play one round: start, simulate, finish after the taps are in the past.
    Play(WavePlayArgs),
    /// Show the per-day limits and what has been collected (GET /ads/reward).
    Status,
}

#[derive(Debug, Args)]
pub struct WavePlayArgs {
    /// Round rules 1 or 2. The server refuses anything else with 409.
    #[arg(long, default_value = "2")]
    pub rules_version: u32,
    /// Seconds to wait before finishing. The replay treats tap times that are
    /// still in the future as a miss, so the wait must cover the last tap.
    #[arg(long, default_value = "0")]
    pub wait: u64,
}

#[derive(Debug, Args)]
pub struct QuizArgs {
    /// Question title.
    #[arg(long)]
    pub title: String,
    /// Optional description.
    #[arg(long, default_value = "")]
    pub description: String,
    /// Answer label. Repeat for each option (at least two).
    #[arg(long = "option", required = true)]
    pub options: Vec<String>,
    /// Confidence for the matching `--option`, as a percentage. Repeat.
    #[arg(long = "prob")]
    pub probs: Vec<u8>,
    /// Closing time, ISO 8601 in UTC (for example 2026-11-27T00:00:00.000Z).
    #[arg(long)]
    pub closes_at: String,
    /// Optional image URL.
    #[arg(long)]
    pub image_url: Option<String>,
    /// Optional news URL.
    #[arg(long)]
    pub news_url: Option<String>,
    /// Optional note shown with the quiz.
    #[arg(long, default_value = "")]
    pub note: String,
}

#[derive(Debug, Args)]
pub struct FollowArgs {
    /// User id.
    pub user: String,
    /// Reverse the action: unfollow instead of follow.
    #[arg(long)]
    pub off: bool,
}

#[derive(Debug, Args)]
pub struct BlockArgs {
    /// User id.
    pub user: String,
    /// Reverse the action: unblock instead of block.
    #[arg(long)]
    pub off: bool,
}

#[derive(Debug, Subcommand)]
pub enum AuthCmd {
    /// Store a session token that was established elsewhere (the app or an
    /// emulator). This CLI runs no OAuth flow of its own.
    ImportToken(AuthImportArgs),
    /// Report whether a usable session is stored.
    Status,
    /// Call POST /logout and forget the stored session.
    Logout(AuthLogoutArgs),
}

#[derive(Debug, Args)]
pub struct AuthImportArgs {
    /// The token value. Omit to read it from stdin.
    #[arg(long, value_name = "TOKEN")]
    pub token: Option<String>,
    /// Read the token from stdin even when a terminal is attached.
    #[arg(long)]
    pub stdin: bool,
}

#[derive(Debug, Args)]
pub struct AuthCallbackArgs {
    /// The full redirect URI, as the OS passes it.
    #[arg(value_name = "URI")]
    pub uri: String,
    /// Forward the URI to this machine's relay instead of finishing here.
    /// Recorded at `auth register` time so the OS handler carries it.
    #[arg(long)]
    pub relay: Option<String>,
}

#[derive(Debug, Args)]
pub struct AuthLogoutArgs {
    /// Forget the local session even if the server call fails.
    #[arg(long)]
    pub forget: bool,
}

#[derive(Debug, Subcommand)]
pub enum EventsCmd {
    /// List events (GET /events).
    List(EventsListArgs),
    /// Show one event's board (GET /events?ids=).
    Show(EventsShowArgs),
    /// Price history for one event (GET /events/{id}/history).
    History(EventsHistoryArgs),
    /// Live board snapshot (GET /live-board).
    Live,
    /// Categories (GET /categories).
    Categories(EventsCategoriesArgs),
    /// Subcategories of one category (GET /subcategories).
    Subcategories(EventsSubcategoriesArgs),
    /// Up/down movement for one event (GET /updown/{id}).
    Updown(EventsUpdownArgs),
}

#[derive(Debug, Args)]
pub struct EventsListArgs {
    /// Comma-separated event ids to fetch instead of the full list.
    /// The server accepts at most 50 per request (measured: 50 is 200, 51 is
    /// 400), so larger sets are split and merged automatically.
    #[arg(long, value_delimiter = ',')]
    pub ids: Vec<String>,
    /// Send `trial_ui=1`, which is what the app's `trialParams()` helper
    /// appends when the account is in trial mode (bundle 500857-500870).
    #[arg(long)]
    pub trial: bool,
    /// Add a `buy_prob` and `shares` field to every option, computed for this
    /// stake. The listing `prob` is the current weight share; what a purchase
    /// would actually pay also folds the stake itself into the fraction, so the
    /// two differ by design. Comparing candidates needs one fixed stake.
    #[arg(long, value_name = "POINTS")]
    pub stake: Option<u64>,
}

#[derive(Debug, Args)]
pub struct EventsShowArgs {
    /// Event id.
    pub id: String,
}

#[derive(Debug, Args)]
pub struct EventsHistoryArgs {
    /// Event id.
    pub id: String,
    /// Period selector. The app's own set is `1d` / `1w` / `1m` / `all`
    /// (bundle 744613-744622), defaulting to `1w` (683501).
    #[arg(long, default_value = "1w")]
    pub period: String,
}

#[derive(Debug, Args)]
pub struct EventsCategoriesArgs {
    /// Send `trial_ui=1` (bundle 500857-500870).
    #[arg(long)]
    pub trial: bool,
}

#[derive(Debug, Args)]
pub struct EventsSubcategoriesArgs {
    /// Category key, e.g. `soccer`.
    pub category: String,
}

#[derive(Debug, Args)]
pub struct EventsUpdownArgs {
    /// Market symbol, e.g. `btc`. The app looks this up from the subcategory
    /// (bundle 753033 maps `ビットコイン` to `btc`); it never sends an event id.
    /// An event id is also accepted and converted through that same table.
    pub sym: String,
    /// Request the live variant (`?live=1`), which adds the round list and
    /// `server_now` alongside the price history.
    #[arg(long)]
    pub live: bool,
}

#[derive(Debug, Subcommand)]
pub enum PositionsCmd {
    /// List your positions (GET /positions?tab=).
    List(PositionsListArgs),
    /// Collect a settled payout (POST /positions/{id}/claim).
    Claim(PositionsClaimArgs),
}

#[derive(Debug, Args)]
pub struct PositionsClaimArgs {
    /// Position id, or `all` to collect every won position that is still
    /// unclaimed. `all` reads the list first and claims what it finds.
    pub position: String,
    /// Payout currency: `point` or `coin` (bundle 535604-535611). The app
    /// remembers the last choice, but the request itself always carries one.
    #[arg(long, default_value = "point")]
    pub currency: String,
    /// With `all`, act even if a claim fails part-way. Without it the first
    /// failure stops the run so a half-collected state is not hidden.
    #[arg(long)]
    pub keep_going: bool,
}

#[derive(Debug, Args)]
pub struct PositionsListArgs {
    /// Tab selector. The app's own two tabs are `active` (参加中) and `result`
    /// (結果) — bundle 830255, 830269 — defaulting to `active`.
    /// Measured: `active` returns every unsettled position (11 on the test
    /// account) while `result` returns only settled ones (3). Anything the
    /// server does not recognise falls back to the 3-entry default.
    #[arg(long, default_value = "active")]
    pub tab: String,
}

#[derive(Debug, Subcommand)]
pub enum MissionsCmd {
    /// List missions (GET /missions).
    List,
}

#[derive(Debug, Subcommand)]
pub enum LedgerCmd {
    /// List ledger rows (GET /ledger?currency=).
    List(LedgerListArgs),
}

#[derive(Debug, Args)]
pub struct LedgerListArgs {
    /// Currency selector. The app's two tabs are `coin` and `point`
    /// (bundle 776543, 775224), defaulting to `coin`.
    #[arg(long, default_value = "coin")]
    pub currency: String,
}

#[derive(Debug, Subcommand)]
pub enum NoticesCmd {
    /// List notices (GET /notices).
    List,
}

#[derive(Debug, Subcommand)]
pub enum RankingsCmd {
    /// Profit ranking (GET /rankings?period=&kind=profit).
    List(RankingsListArgs),
}

#[derive(Debug, Args)]
pub struct RankingsListArgs {
    /// Period selector. Measured vocabulary is `daily` / `weekly` / `monthly`
    /// / `all`; anything else is coerced to `weekly` by the server. The app's
    /// own default is `weekly` (bundle 801869).
    #[arg(long, default_value = "weekly")]
    pub period: String,
}

#[derive(Debug, Subcommand)]
pub enum UsersCmd {
    /// Show a user's profile (GET /users/{user}/profile).
    Show(UsersShowArgs),
    /// List a user's positions (GET /users/{user}/positions).
    Positions(UsersPositionsArgs),
}

#[derive(Debug, Args)]
pub struct UsersShowArgs {
    /// User handle or id.
    pub user: String,
    /// Period selector.
    #[arg(long, default_value = "1d")]
    pub period: String,
}

#[derive(Debug, Args)]
pub struct UsersPositionsArgs {
    /// User handle or id.
    pub user: String,
    /// Tab selector.
    #[arg(long, default_value = "open")]
    pub tab: String,
}

#[derive(Debug, Args)]
pub struct BuyArgs {
    /// Event id.
    #[arg(long)]
    pub event: String,
    /// Option id.
    #[arg(long)]
    pub option: String,
    /// Coins to stake. Measured: the server debits coins, not points, and
    /// the request carries no currency field (bundle 665770).
    #[arg(long)]
    pub stake: u64,
}

impl BuyArgs {
    /// The board request used to compute the estimate shown by `--dry-run`.
    /// It is a plain read of the event the buy targets.
    pub fn board_request(&self, api: &crate::client::Api) -> crate::wire::WireRequest {
        api.event(&self.event)
    }
}

#[derive(Debug, Args)]
pub struct SellArgs {
    /// Position id.
    pub position: String,
}

#[derive(Debug, Args)]
pub struct ClaimArgs {
    /// Mission id.
    pub mission: String,
}

#[derive(Debug, Args)]
pub struct TransferArgs {
    /// Recipient (as the app sends it: `to`).
    #[arg(long)]
    pub to: String,
    /// Amount.
    #[arg(long)]
    pub amount: u64,
    /// Message.
    #[arg(long, default_value = "")]
    pub message: String,
}

#[derive(Debug, Args)]
pub struct InviteClaimArgs {
    /// Invite code.
    pub code: String,
}

#[derive(Debug, Args)]
pub struct GiftExchangeArgs {
    /// Batch id.
    #[arg(long)]
    pub batch: String,
    /// Items as a JSON array, e.g. '[{"id":"g1","qty":1}]'.
    #[arg(long)]
    pub items: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn global_flag_before_subcommand_parses() {
        let c = Cli::try_parse_from(["sakiyomi", "--token", "abc", "events", "list"]).unwrap();
        assert_eq!(c.global.token.as_deref(), Some("abc"));
    }

    #[test]
    fn global_flag_after_subcommand_parses() {
        let c = Cli::try_parse_from(["sakiyomi", "events", "list", "--table"]).unwrap();
        assert!(c.global.table);
    }

    #[test]
    fn unknown_flag_is_rejected() {
        assert!(Cli::try_parse_from(["sakiyomi", "events", "list", "--bogus"]).is_err());
    }

    #[test]
    fn there_is_no_charge_command() {
        // /charge is an expo-router screen, not an API path. Guard against
        // reintroducing it by accident.
        assert!(Cli::try_parse_from(["sakiyomi", "charge"]).is_err());
    }

    #[test]
    fn buy_requires_all_three_arguments() {
        assert!(Cli::try_parse_from(["sakiyomi", "buy", "--event", "e1"]).is_err());
        let c = Cli::try_parse_from([
            "sakiyomi", "buy", "--event", "e1", "--option", "o1", "--stake", "100",
        ])
        .unwrap();
        match c.command {
            Command::Buy(b) => {
                assert_eq!(b.event, "e1");
                assert_eq!(b.option, "o1");
                assert_eq!(b.stake, 100);
            }
            other => panic!("expected Buy, got {:?}", other),
        }
    }
}
