//! API surface: one builder per verified endpoint.
//!
//! Each method returns a [`WireRequest`] and performs no I/O. Sending is the
//! transport's job. Keeping the two apart means the dry-run path and the live
//! path describe the *same* request by construction — there is no second
//! code path that could drift from the one that actually goes on the wire.
//!
//! Every path, method, body key and query key traces to a line in
//! `analysis/bundle/index.js`; the mapping is tabulated in
//! `analysis/api-contracts.md`. If an endpoint is not in that table it does not
//! appear here.
//!
//! In particular there is **no** `POST /charge`. `'/charge'` in the bundle is an
//! expo-router screen transition (lines 666475, 685507, 776525), not an HTTP
//! call, so deposit has no restored API path and this module does not invent one.

use crate::transport::HttpResponse;
use crate::wire::{encode_path_segment, id_value, WireRequest};

/// Anything that could stop a call from returning a usable response.
#[derive(Debug)]
pub enum ApiError {
    Transport(crate::transport::TransportError),
    /// Non-2xx. Carries the server's `error` string when it sent one, matching
    /// the client's `ApiError(status, body.error)` (bundle 376591-376596).
    Status {
        status: u16,
        message: String,
        body: serde_json::Value,
    },
    /// 401 on a call that had a token. The bundle invokes its unauthorized
    /// callback here (376569-376607) and then still rejects.
    Unauthorized {
        body: serde_json::Value,
    },
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Transport(e) => write!(f, "{}", e),
            ApiError::Status {
                status, message, ..
            } => {
                write!(f, "HTTP {}: {}", status, message)
            }
            ApiError::Unauthorized { body } => {
                // The server does send a message here (`{"error":"ログインしてください"}`
                // observed live on GET /me), so surface it rather than invent one.
                let detail = body
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("session is not valid");
                write!(f, "HTTP 401: {} (log in again)", detail)
            }
        }
    }
}

impl std::error::Error for ApiError {}

impl From<crate::transport::TransportError> for ApiError {
    fn from(e: crate::transport::TransportError) -> Self {
        ApiError::Transport(e)
    }
}

/// Apply the bundle's success / 401 / error split to one response.
///
/// `had_token` matters: the app only runs the unauthorized callback when the
/// request carried a session (376569-376607). A 401 without a token is just an
/// unauthenticated request and is reported as a status, not as a dead session.
///
/// `is_login` matters too: the bundle explicitly exempts `/login` from the
/// unauthorized path (376575 compares the request path against `'/login'`), so a
/// failed login is reported with the server's own message — 「メールアドレスまたは
/// パスワードが違います」 — instead of being mislabelled as an expired session.
pub fn classify(
    resp: HttpResponse,
    had_token: bool,
    is_login: bool,
) -> Result<HttpResponse, ApiError> {
    let status = resp.status;
    if (200..300).contains(&status) {
        return Ok(resp);
    }
    if status == 401 && had_token && !is_login {
        return Err(ApiError::Unauthorized { body: resp.body });
    }
    // The bundle only uses the server's message when `json.error` is a string;
    // otherwise it falls back to this literal (376583-376596). A 401 body here
    // does carry one in practice, observed live.
    let message = resp
        .body
        .get("error")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "通信に失敗しました".to_string());
    Err(ApiError::Status {
        status,
        message,
        body: resp.body,
    })
}

/// Request builders. Holds only what the app injects into payloads, so it can be
/// constructed without a network client.
pub struct Api {
    pub app_version: String,
}

/// The fields `POST /quizzes` takes (bundle 516118).
///
/// `prob` is `None` when the caller left it out, which the app sends as null
/// rather than as zero: a zero would claim "no chance at all".
#[derive(Debug, Clone, Default)]
pub struct QuizDraft {
    pub title: String,
    pub description: String,
    /// `(label, prob)` in the order the app renders them.
    pub options: Vec<(String, Option<u8>)>,
    pub closes_at: String,
    pub image_url: Option<String>,
    pub news_url: Option<String>,
    pub note: String,
}

// The recovered surface is wider than the exposed CLI: builders listed here are
// derived from the bundle and kept so the contract table stays executable even
// when a subcommand does not use them yet.
#[allow(dead_code)]
impl Api {
    pub fn new(app_version: impl Into<String>) -> Self {
        Self {
            app_version: app_version.into(),
        }
    }

    // ---- unauthenticated ---------------------------------------------------

    /// `GET /config` — verified live: 200 with six feature flags.
    pub fn config(&self) -> WireRequest {
        WireRequest::get("/config")
    }

    // ---- auth --------------------------------------------------------------

    /// `POST /auth/social` — provider plus id_token; `promo_v1: true` is
    /// unconditional in the app (355919-355933). Referral fields arrive as a
    /// pair — `{ref_code, ref_via}` (355940-355946) — so both are taken or
    /// neither. `device_key` is added only when the app resolved one
    /// (355958-355963).
    pub fn auth_social(
        &self,
        provider: &str,
        id_token: &str,
        nickname: Option<&str>,
        referral: Option<(&str, &str)>,
        device_key: Option<&str>,
    ) -> WireRequest {
        let mut body = serde_json::json!({
            "provider": provider,
            "id_token": id_token,
            "promo_v1": true,
        });
        if let Some(n) = nickname {
            body["nickname"] = serde_json::Value::String(n.to_string());
        }
        if let Some((code, via)) = referral {
            body["ref_code"] = serde_json::Value::String(code.to_string());
            body["ref_via"] = serde_json::Value::String(via.to_string());
        }
        if let Some(d) = device_key {
            body["device_key"] = serde_json::Value::String(d.to_string());
        }
        WireRequest::post("/auth/social").body(body)
    }

    /// `POST /logout` — the app sends no body of its own (356615).
    pub fn logout(&self) -> WireRequest {
        WireRequest::post("/logout")
    }

    /// `GET /me?app_version=` (bundle 357115-357125).
    pub fn me(&self) -> WireRequest {
        WireRequest::get("/me").query("app_version", self.app_version.clone())
    }

    // ---- reads -------------------------------------------------------------

    /// `GET /events`. `ids` is comma-joined by the client (bundle 497859-497870).
    /// `trial_ui=1` is appended when the app is in trial mode (500857-500870).
    pub fn events(&self, ids: Option<&[String]>, trial: bool) -> WireRequest {
        let mut req = WireRequest::get("/events");
        if let Some(ids) = ids {
            if !ids.is_empty() {
                req = req.query("ids", ids.join(","));
            }
        }
        Self::with_trial(req, trial)
    }

    /// `GET /home` plus `trial_ui=1` when in trial mode (bundle 498939-498944).
    pub fn home(&self, trial: bool) -> WireRequest {
        Self::with_trial(WireRequest::get("/home"), trial)
    }

    /// `GET /categories` plus `trial_ui=1` (bundle 497274-497280).
    pub fn categories(&self, trial: bool) -> WireRequest {
        Self::with_trial(WireRequest::get("/categories"), trial)
    }

    /// `GET /events?ids=a,b` — the app's batch fetch (bundle 497859-497870).
    pub fn events_by_ids(&self, ids: &[String]) -> WireRequest {
        WireRequest::get("/events").query("ids", ids.join(","))
    }

    /// `GET /events/{id}` — one event plus its series.
    ///
    /// Measured: returns 200 with `{event, series}` for both an open and a
    /// closed event, where `?ids=` returns an empty list for the closed one.
    ///
    /// The bundle builds `/events/` + id + trialParams (497957-497966), but
    /// appending `?trial_ui=1` is measured to return **404** on this endpoint
    /// (`/events/6663` 200 vs `/events/6663?trial_ui=1` 404, both re-checked),
    /// so the parameter is deliberately not sent here.
    pub fn event(&self, id: &str) -> WireRequest {
        WireRequest::get(format!("/events/{}", encode_path_segment(id)))
    }

    /// `trialParams()` — `{trial_ui: 1}` or nothing (bundle 500857-500870).
    ///
    /// The key name is NOT unknown: the bundle's helper returns literally
    /// `{'trial_ui': 1}` and callers append it to `/events`, `/home`,
    /// `/categories` and `/subcategories`.
    fn with_trial(req: WireRequest, trial: bool) -> WireRequest {
        if trial {
            req.query("trial_ui", "1")
        } else {
            req
        }
    }

    /// `GET /events/{id}/history?period=` (bundle 498628-498638).
    pub fn event_history(&self, id: &str, period: &str) -> WireRequest {
        let path = format!("/events/{}/history", encode_path_segment(id));
        WireRequest::get(path).query("period", period)
    }

    /// `GET /live-board` (bundle 498117-498120).
    pub fn live_board(&self) -> WireRequest {
        WireRequest::get("/live-board")
    }

    /// `GET /categories` (bundle 497279).
    pub fn categories_plain(&self) -> WireRequest {
        WireRequest::get("/categories")
    }

    /// `GET /subcategories?category=&groups=1` (bundle 498728-498744).
    pub fn subcategories(&self, category: &str) -> WireRequest {
        WireRequest::get("/subcategories")
            .query("category", category)
            .query("groups", "1")
    }

    /// `GET /updown/{id}`. `live` selects the variant that appends `?live=1`
    /// (bundle 498490-498495).
    pub fn updown(&self, id: &str, live: bool) -> WireRequest {
        let path = format!("/updown/{}", encode_path_segment(id));
        if live {
            WireRequest::get(path).query("live", "1")
        } else {
            WireRequest::get(path)
        }
    }

    /// `GET /rankings?period=&kind=profit` (bundle 499308-499315).
    pub fn rankings(&self, period: &str) -> WireRequest {
        WireRequest::get("/rankings")
            .query("period", period)
            .query("kind", "profit")
    }

    /// `GET /users/{user}/profile?period=` (bundle 499439-499456).
    pub fn user_profile(&self, user: &str, period: &str) -> WireRequest {
        let path = format!("/users/{}/profile", encode_path_segment(user));
        WireRequest::get(path).query("period", period)
    }

    /// `GET /positions?tab=` (bundle 499220-499231).
    pub fn positions(&self, tab: &str) -> WireRequest {
        WireRequest::get("/positions").query("tab", tab)
    }

    /// `GET /users/{user}/positions?tab=` (bundle 496704-496721).
    pub fn user_positions(&self, user: &str, tab: &str) -> WireRequest {
        let path = format!("/users/{}/positions", encode_path_segment(user));
        WireRequest::get(path).query("tab", tab)
    }

    /// `GET /missions` (bundle 495574-495588).
    pub fn missions(&self) -> WireRequest {
        WireRequest::get("/missions")
    }

    /// `GET /ledger?currency=` (bundle 499650-499657).
    pub fn ledger(&self, currency: &str) -> WireRequest {
        WireRequest::get("/ledger").query("currency", currency)
    }

    /// `GET /notices` (bundle 500028-500039).
    pub fn notices(&self) -> WireRequest {
        WireRequest::get("/notices")
    }

    /// `GET /blocks` (bundle 495501-495519).
    pub fn blocks(&self) -> WireRequest {
        WireRequest::get("/blocks")
    }

    // ---- money / state changing -------------------------------------------

    /// `POST /events/{id}/buy {option_id, stake}` (bundle 665762-665777).
    /// `option_id` is a JSON number in the app (copied from the event's option
    /// id), so it is coerced rather than sent as a string.
    pub fn buy(&self, event_id: &str, option_id: &str, stake: u64) -> WireRequest {
        let path = format!("/events/{}/buy", encode_path_segment(event_id));
        WireRequest::post(path)
            .body(serde_json::json!({
                "option_id": id_value(option_id),
                "stake": stake,
            }))
            .money()
    }

    /// `POST /positions/{id}/sell` with no body (bundle 730858-730864).
    pub fn sell(&self, position_id: &str) -> WireRequest {
        let path = format!("/positions/{}/sell", encode_path_segment(position_id));
        WireRequest::post(path).money()
    }

    /// `POST /positions/{id}/claim {currency}` — collect a settled payout
    /// (bundle 535418-535532). The path is built as `/positions/` + id + `/claim`
    /// and the body is exactly `{"currency": "point" | "coin"}` (535526-535509).
    ///
    /// Measured: calling it on an already-claimed position answers
    /// `409 受け取り済みか、受け取れる報酬がありません`, so the server both
    /// records the claim and refuses a repeat.
    pub fn claim_position(&self, position_id: &str, currency: &str) -> WireRequest {
        let path = format!("/positions/{}/claim", encode_path_segment(position_id));
        WireRequest::post(path)
            .body(serde_json::json!({ "currency": currency }))
            .money()
    }

    /// `POST /missions/{id}/claim` with no body (bundle 445130-445136).
    pub fn claim(&self, mission_id: &str) -> WireRequest {
        let path = format!("/missions/{}/claim", encode_path_segment(mission_id));
        WireRequest::post(path).money()
    }

    /// `POST /transfers {to, amount, message}` (bundle 813286-813309).
    /// The app trims `to` (813288) and `message` (813304) before sending, and
    /// coerces a non-numeric amount to 0 (813296-813300). The CLI requires a
    /// numeric amount already, and trims both text fields the same way so a
    /// pasted handle with a trailing space behaves identically.
    pub fn transfer(&self, to: &str, amount: u64, message: &str) -> WireRequest {
        WireRequest::post("/transfers")
            .body(serde_json::json!({
                "to": to.trim(),
                "amount": amount,
                "message": message.trim(),
            }))
            .money()
    }

    /// `POST /invite/claim {code, invite_v2:true, promo_v1:true}`
    /// (bundle 774248-774256). The app trims the code (774250).
    pub fn invite_claim(&self, code: &str) -> WireRequest {
        WireRequest::post("/invite/claim")
            .body(serde_json::json!({
                "code": code.trim(),
                "invite_v2": true,
                "promo_v1": true,
            }))
            .money()
    }

    /// `POST /gifts/exchange {batch_id, items}` (bundle 765689-765699).
    pub fn gifts_exchange(&self, batch_id: &str, items: serde_json::Value) -> WireRequest {
        WireRequest::post("/gifts/exchange")
            .body(serde_json::json!({ "batch_id": batch_id, "items": items }))
            .money()
    }

    /// `POST /events/{id}/result-request` (bundle 803615-803620). The body is
    /// whatever the form produced, so it passes through rather than being
    /// reconstructed.
    pub fn result_request(&self, event_id: &str, body: serde_json::Value) -> WireRequest {
        let path = format!("/events/{}/result-request", encode_path_segment(event_id));
        WireRequest::post(path).body(body).money()
    }

    /// `GET /ads/reward?device=android&nonce=&win_event=&mg_play=`
    /// (bundle 455655-455762). The app maps a 404 to a default "ads unavailable"
    /// state; that mapping belongs to the caller, not to the builder.
    pub fn ads_reward(&self, nonce: &str, win_event: &str, mg_play: &str) -> WireRequest {
        WireRequest::get("/ads/reward")
            .query("device", "android")
            .query("nonce", nonce)
            .query("win_event", win_event)
            .query("mg_play", mg_play)
    }

    /// `POST /minigame/wave/start {platform, app_version, rules_version}`
    /// (bundle 502203-502213).
    ///
    /// `rules_version` is a **number** in the bundle (`WAVE_RULES_VERSION` is read
    /// out of a constants module at 502208), and the server validates it against
    /// the current rule set: sending it as a string was measured to be rejected
    /// with 409 「アプリを最新の版にしてから遊んでください」. Values 1 and 2 are
    /// accepted today; 3 and above are refused as "not released yet".
    pub fn minigame_start(&self, rules_version: u32) -> WireRequest {
        WireRequest::post("/minigame/wave/start")
            .body(serde_json::json!({
                "platform": "android",
                "app_version": self.app_version,
                "rules_version": rules_version,
            }))
            .timeout_ms(15_000)
            .money()
    }

    /// `POST /minigame/wave/finish {play_id, taps}` (bundle 502220-502229).
    ///
    /// `taps` is the array of tap times in milliseconds, exactly as the app
    /// reports them (501927-501947, 787938): the server replays the list, so
    /// sending a count instead of the times is a different request entirely.
    pub fn minigame_finish(&self, play_id: &str, taps: &[u64]) -> WireRequest {
        WireRequest::post("/minigame/wave/finish")
            .body(serde_json::json!({ "play_id": play_id, "taps": taps }))
            .timeout_ms(20_000)
            .money()
    }

    /// `POST /quizzes {title, description, options, image_url, closes_at,
    /// news_url, note}` (bundle 516118-516208). Labels, descriptions and notes
    /// are trimmed by the app (516120-516206); each option is
    /// `{label, prob, image_url}` with `prob` parsed as a base-10 integer or
    /// null when the field was left blank (516134-516164).
    ///
    /// Marks the request as money-moving: adoption pays `quiz_adopt_bonus`
    /// points, and the app treats the same call as a state change.
    pub fn create_quiz(&self, quiz: &QuizDraft) -> WireRequest {
        let options: Vec<serde_json::Value> = quiz
            .options
            .iter()
            .map(|(label, prob)| {
                serde_json::json!({
                    "label": label.trim(),
                    "prob": prob,
                    "image_url": quiz.image_url.as_deref().map(str::trim),
                })
            })
            .collect();
        let closes = quiz.closes_at.trim();
        let note = quiz.note.trim();
        WireRequest::post("/quizzes")
            .body(serde_json::json!({
                "title": quiz.title.trim(),
                "description": quiz.description.trim(),
                "options": options,
                "image_url": quiz.image_url.as_deref().map(str::trim),
                "closes_at": closes,
                "news_url": quiz.news_url.as_deref().map(str::trim),
                "note": if note.is_empty() { serde_json::Value::Null }
                else { serde_json::Value::String(note.to_string()) },
            }))
            .money()
    }

    /// `GET /quizzes/mine` — the caller's own quizzes (bundle 495732).
    pub fn quizzes_mine(&self) -> WireRequest {
        WireRequest::get("/quizzes/mine")
    }

    // ---- profile / social toggles -----------------------------------------

    /// `POST /profile/privacy {positions_private}` (bundle 818559-818566).
    pub fn profile_privacy(&self, positions_private: bool) -> WireRequest {
        WireRequest::post("/profile/privacy")
            .body(serde_json::json!({ "positions_private": positions_private }))
    }

    /// `POST /users/{id}/follow/notify {on}` (bundle 830751-830760).
    pub fn follow_notify(&self, user_id: &str, on: bool) -> WireRequest {
        let path = format!("/users/{}/follow/notify", encode_path_segment(user_id));
        WireRequest::post(path).body(serde_json::json!({ "on": on }))
    }

    /// `POST|DELETE /users/{id}/follow`.
    ///
    /// `on` is the **current** state, not the desired one. The bundle sets
    /// `method = 'POST'` and switches to `'DELETE'` when `on` is truthy
    /// (830777-830781), and its success handler sets `is_following = !on`
    /// (828039-828040). So true ⇒ DELETE (unfollow), false ⇒ POST (follow).
    pub fn follow(&self, user_id: &str, on: bool) -> WireRequest {
        let path = format!("/users/{}/follow", encode_path_segment(user_id));
        if on {
            WireRequest::delete(path)
        } else {
            WireRequest::post(path)
        }
    }

    /// `POST|DELETE /users/{id}/block`.
    ///
    /// Same rule as [`Api::follow`] — `on` is the current state and true means
    /// DELETE (830729-830733). The block success handler prints
    /// 「ブロックしました」 when `!on` (828575-828579), which confirms the
    /// polarity. There is no inversion between the two endpoints.
    pub fn block(&self, user_id: &str, on: bool) -> WireRequest {
        let path = format!("/users/{}/block", encode_path_segment(user_id));
        if on {
            WireRequest::delete(path)
        } else {
            WireRequest::post(path)
        }
    }

    /// `POST /support/inquiries {category, body, email, meta}` (bundle 715306-715363).
    pub fn support_inquiry(&self, category: &str, body_text: &str, email: &str) -> WireRequest {
        WireRequest::post("/support/inquiries").body(serde_json::json!({
            "category": category,
            "body": body_text,
            "email": email,
            "meta": {
                "app_version": self.app_version,
                "build": serde_json::Value::Null,
                "os": "android",
                "os_version": serde_json::Value::Null,
                "device": serde_json::Value::Null,
            },
        }))
    }
}

/// Pull the session token out of a login response.
///
/// The bundle reads `.token` off the resolved body (356164-356166 for `/login`,
/// 355809 for `/auth/social`), so one extractor serves both. Returns an error
/// rather than a default when the field is missing: a login that returns no
/// token did not succeed.
///
/// The CLI no longer runs either login, so only the tests call this now.
#[allow(dead_code)]
pub fn extract_token(resp: &HttpResponse) -> Result<String, String> {
    match resp.body.get("token") {
        Some(serde_json::Value::String(t)) if !t.trim().is_empty() => Ok(t.clone()),
        Some(serde_json::Value::String(_)) => Err("login returned an empty token".into()),
        Some(other) => Err(format!("login returned a non-string token: {}", other)),
        None => {
            let keys: Vec<&str> = resp
                .body
                .as_object()
                .map(|o| o.keys().map(|k| k.as_str()).collect())
                .unwrap_or_default();
            Err(format!(
                "login response has no 'token' field; top-level keys were {:?}",
                keys
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(status: u16, body: serde_json::Value) -> HttpResponse {
        HttpResponse {
            status,
            body,
            raw: String::new(),
        }
    }

    #[test]
    fn token_extraction_reads_the_documented_field() {
        let r = resp(200, serde_json::json!({"token": "abc", "user": {"id": 1}}));
        assert_eq!(extract_token(&r).unwrap(), "abc");
    }

    #[test]
    fn token_extraction_rejects_missing_field() {
        let r = resp(200, serde_json::json!({"ok": true}));
        let e = extract_token(&r).unwrap_err();
        assert!(e.contains("no 'token' field"), "{}", e);
        assert!(
            e.contains("ok"),
            "error should list the keys that were present"
        );
    }

    #[test]
    fn token_extraction_rejects_empty_string() {
        assert!(extract_token(&resp(200, serde_json::json!({"token": "   "}))).is_err());
    }

    #[test]
    fn classify_passes_2xx_through() {
        let r = classify(resp(200, serde_json::json!({"ok": true})), true, false).unwrap();
        assert_eq!(r.status, 200);
    }

    #[test]
    fn classify_treats_401_as_dead_session_only_with_a_token() {
        let e = classify(
            resp(401, serde_json::json!({"error": "ログインしてください"})),
            true,
            false,
        );
        assert!(matches!(e, Err(ApiError::Unauthorized { .. })));
        // Without a token the same 401 is an ordinary status error.
        let e = classify(
            resp(401, serde_json::json!({"error": "ログインしてください"})),
            false,
            false,
        );
        assert!(matches!(e, Err(ApiError::Status { status: 401, .. })));
    }

    #[test]
    fn classify_uses_the_server_message_when_present() {
        let e = classify(
            resp(404, serde_json::json!({"error": "見つかりません"})),
            false,
            false,
        );
        match e {
            Err(ApiError::Status { message, .. }) => assert_eq!(message, "見つかりません"),
            other => panic!("expected status error, got {:?}", other),
        }
    }

    #[test]
    fn classify_falls_back_when_the_body_has_no_message() {
        let e = classify(resp(500, serde_json::json!({})), false, false);
        match e {
            Err(ApiError::Status { message, .. }) => assert_eq!(message, "通信に失敗しました"),
            other => panic!("expected status error, got {:?}", other),
        }
    }

    #[test]
    fn classify_does_not_treat_a_failed_login_as_an_expired_session() {
        // The bundle exempts '/login' from the unauthorized callback (376575),
        // so a 401 there must carry the server's message rather than being
        // reported as "log in again" — the operator is already trying to.
        let e = classify(
            resp(
                401,
                serde_json::json!({"error": "メールアドレスまたはパスワードが違います"}),
            ),
            true,
            true,
        );
        match e {
            Err(ApiError::Status {
                status, message, ..
            }) => {
                assert_eq!(status, 401);
                assert_eq!(message, "メールアドレスまたはパスワードが違います");
            }
            other => panic!("a /login 401 must be a status error, got {:?}", other),
        }
    }

    #[test]
    fn invite_claim_trims_the_code_like_the_app() {
        let api = Api::new("1.0.42");
        let body = api.invite_claim("  ABC123 \n").body.unwrap();
        assert_eq!(body["code"], "ABC123");
    }

    #[test]
    fn transfer_trims_recipient_and_message() {
        let api = Api::new("1.0.42");
        let body = api.transfer("  bob  ", 100, " hi ").body.unwrap();
        assert_eq!(body["to"], "bob");
        assert_eq!(body["message"], "hi");
        assert_eq!(body["amount"], 100);
    }

    #[test]
    fn social_login_sends_referral_fields_as_a_pair() {
        let api = Api::new("1.0.42");
        let body = api
            .auth_social(
                "google",
                "id-token",
                None,
                Some(("CODE", "instagram")),
                None,
            )
            .body
            .unwrap();
        assert_eq!(body["ref_code"], "CODE");
        assert_eq!(body["ref_via"], "instagram", "ref_via must not be dropped");
        assert_eq!(body["promo_v1"], true);
    }

    #[test]
    fn social_login_omits_referral_when_absent() {
        let api = Api::new("1.0.42");
        let body = api
            .auth_social("apple", "t", Some("nick"), None, None)
            .body
            .unwrap();
        assert!(body.get("ref_code").is_none());
        assert!(body.get("ref_via").is_none());
        assert_eq!(body["nickname"], "nick");
    }

    #[test]
    fn follow_and_block_share_the_same_polarity() {
        // `on` is the CURRENT state, and the bundle sends DELETE when it is
        // truthy for BOTH endpoints (830777-830781, 830729-830733). There is no
        // inversion between them; an earlier version of this file wrongly
        // assumed one and would have turned `block` into `unblock`.
        let api = Api::new("1.0.42");
        for (name, on_true, on_false) in [
            ("follow", api.follow("u1", true), api.follow("u1", false)),
            ("block", api.block("u1", true), api.block("u1", false)),
        ] {
            assert_eq!(
                on_true.method,
                crate::wire::Method::Delete,
                "{} with on=true must send DELETE (undo the current state)",
                name
            );
            assert_eq!(
                on_false.method,
                crate::wire::Method::Post,
                "{} with on=false must send POST (apply the state)",
                name
            );
        }
    }

    #[test]
    fn buy_sends_a_numeric_option_id() {
        let api = Api::new("1.0.42");
        let req = api.buy("44775", "59383", 100);
        assert_eq!(req.path, "/events/44775/buy");
        let body = req.body.unwrap();
        assert!(
            body["option_id"].is_number(),
            "option_id must be a number: {}",
            body
        );
        assert!(body["stake"].is_number());
        assert!(req.money, "buy must be flagged as money-moving");
    }

    #[test]
    fn position_claim_sends_the_currency_in_the_body() {
        // The app builds `/positions/` + id + `/claim` and sends exactly
        // `{currency}` (535418-535532). Anything else is a different request.
        let api = Api::new("1.0.43");
        let req = api.claim_position("145291", "point");
        assert_eq!(req.path, "/positions/145291/claim");
        let body = req.body.unwrap();
        assert_eq!(body["currency"], "point");
        assert!(req.money, "a payout claim moves points");
    }

    #[test]
    fn every_money_builder_sets_the_money_flag() {
        let api = Api::new("1.0.42");
        for req in [
            api.buy("e", "1", 1),
            api.sell("p"),
            api.claim("m"),
            api.transfer("u", 1, ""),
            api.invite_claim("c"),
            api.gifts_exchange("b", serde_json::json!([])),
            api.result_request("e", serde_json::json!({})),
            api.minigame_start(2),
            api.minigame_finish("p", &[1, 2]),
            api.claim_position("p", "point"),
            api.create_quiz(&QuizDraft::default()),
        ] {
            assert!(req.money, "{} must be flagged money", req.path);
        }
    }

    #[test]
    fn read_builders_are_not_flagged_as_money() {
        let api = Api::new("1.0.42");
        for req in [
            api.config(),
            api.me(),
            api.events(None, false),
            api.positions("open"),
            api.missions(),
            api.ledger("yen"),
            api.notices(),
            api.blocks(),
        ] {
            assert!(!req.money, "{} must not be flagged money", req.path);
        }
    }

    #[test]
    fn minigame_start_sends_a_numeric_rules_version() {
        // A string was measured to be refused with 409: the server validates the
        // value as a number, and the bundle reads it as one (502208).
        let api = Api::new("1.0.42");
        let body = api.minigame_start(2).body.unwrap();
        assert_eq!(body["rules_version"], 2);
        assert!(body["rules_version"].is_number(), "must be numeric");
        assert_eq!(body["platform"], "android");
        assert_eq!(body["app_version"], "1.0.42");
    }

    #[test]
    fn invite_claim_sends_both_version_flags() {
        let api = Api::new("1.0.42");
        let body = api.invite_claim("ABC").body.unwrap();
        assert_eq!(body["invite_v2"], true);
        assert_eq!(body["promo_v1"], true);
        assert_eq!(body["code"], "ABC");
    }

    #[test]
    fn events_ids_are_comma_joined() {
        let api = Api::new("1.0.42");
        let req = api.events(Some(&["1".to_string(), "2".to_string()]), false);
        assert_eq!(req.query_string(), "?ids=1%2C2");
    }
}
