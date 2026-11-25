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
}

#[derive(Debug, Subcommand)]
pub enum BlocksCmd {
    /// List blocked users (GET /blocks).
    List,
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
    /// Log in with Google. Opens the browser and waits for the OS to hand the
    /// redirect back (this is the only sign-in the app actually offers).
    Login,
    /// Receives the OAuth redirect. The OS runs this; you normally never type it.
    Callback(AuthCallbackArgs),
    /// Register this binary as the `app.sakiyomi.mobile` handler.
    Register,
    /// Remove the handler registration.
    Unregister,
    /// Report whether a usable session is stored.
    Status,
    /// Call POST /logout and forget the stored session.
    Logout(AuthLogoutArgs),
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
    #[arg(long, value_delimiter = ',')]
    pub ids: Vec<String>,
    /// Send `trial_ui=1`, which is what the app's `trialParams()` helper
    /// appends when the account is in trial mode (bundle 500857-500870).
    #[arg(long)]
    pub trial: bool,
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
    /// Event id.
    pub id: String,
    /// Request the live variant (`?live=1`).
    #[arg(long)]
    pub live: bool,
}

#[derive(Debug, Subcommand)]
pub enum PositionsCmd {
    /// List your positions (GET /positions?tab=).
    List(PositionsListArgs),
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
    /// Points to stake.
    #[arg(long)]
    pub stake: u64,
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
