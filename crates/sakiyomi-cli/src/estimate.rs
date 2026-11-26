//! Buy-time estimates, reconstructed from the bundle.
//!
//! The app shows "予想すると X%" and a share count before the user commits a
//! stake. Both are computed on the client from the event board it already
//! holds; there is no quote endpoint (measured: no `/quote`, `/preview`,
//! `/estimate` or `/simulate` path exists, and `/events?ids=` returns no pricing
//! field). So the CLI has to repeat the same arithmetic.
//!
//! The formula is `afterBuyProb` (bundle 675326-675525):
//!
//! ```text
//! total  = total_weight + stake
//! weight = option.weight, or option.prob * total_weight when weight is absent
//! buy_prob = (weight + stake) / total
//! shares   = floor(stake / buy_prob)
//! ```
//!
//! `shares` is confirmed against the account's own settled positions: for five
//! positions, `floor(stake / buy_prob)` reproduces the recorded `shares`
//! exactly. `buy_prob` itself is only an estimate at *the moment of the call* —
//! the recorded value was fixed when the position was opened, and weights move
//! afterwards, so a later recomputation cannot match it.

use serde_json::Value;

/// The share count and buy probability a stake would produce right now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Estimate {
    /// `(weight + stake) / (total_weight + stake)`.
    pub buy_prob: f64,
    /// `floor(stake / buy_prob)`.
    pub shares: u64,
    /// The weight the server reports for the chosen option.
    pub weight: f64,
    /// The total weight the server reports for the event.
    pub total_weight: f64,
}

/// Compute the estimate for `option_id` at `stake` points.
///
/// Returns `None` when the board does not carry the option or the numbers
/// needed, instead of substituting a placeholder: a fabricated probability is
/// worse than no estimate, because the caller would act on it.
pub fn buy_estimate(event: &Value, option_id: &str, stake: u64) -> Option<Estimate> {
    let total_weight = event.get("total_weight")?.as_f64()?;
    let options = event.get("options")?.as_array()?;

    // `optionWeight()` (675588-675602): use the raw weight when present, else
    // fall back to prob * total_weight. Both branches exist in the bundle.
    let chosen = options.iter().find(|o| {
        o.get("id")
            .and_then(|v| {
                v.as_i64()
                    .map(|n| n.to_string())
                    .or_else(|| v.as_str().map(str::to_string))
            })
            .is_some_and(|id| id == option_id)
    })?;

    // `optionWeight()` (675590-675601) prefers `weight`, then `raw_weight`, then
    // `prob * total`. When the app's `x-prob-display` header is sent, `weight`
    // already carries the seed-floor maximum, so the first branch is the normal
    // path; the others exist because a reply without that header has no
    // `raw_weight` and may have no `weight`.
    let weight = match chosen.get("weight").and_then(|w| w.as_f64()) {
        Some(w) => w,
        None => match chosen.get("raw_weight").and_then(|w| w.as_f64()) {
            Some(w) => w,
            None => chosen.get("prob")?.as_f64()? * total_weight,
        },
    };

    let stake = stake as f64;
    let total = total_weight + stake;
    if total <= 0.0 {
        return None;
    }
    let buy_prob = (weight + stake) / total;
    if !(0.0..=1.0).contains(&buy_prob) || buy_prob <= 0.0 {
        return None;
    }
    Some(Estimate {
        buy_prob,
        shares: (stake / buy_prob).floor() as u64,
        weight,
        total_weight,
    })
}

/// Add `buy_prob` and `shares` to every option of an event, in place.
///
/// Keeps the server's own `prob` untouched: it is the current weight share, and
/// overwriting it would silently change what every other reader sees. The two
/// fields therefore sit next to `prob` and are only meaningful for one stake.
///
/// Returns how many options were annotated. An option whose numbers are
/// missing keeps its original fields rather than gaining a guess.
pub fn annotate_event(event: &mut Value, stake: u64) -> usize {
    let total_weight = match event.get("total_weight").and_then(|v| v.as_f64()) {
        Some(t) if t > 0.0 => t,
        _ => return 0,
    };
    let Some(options) = event.get_mut("options").and_then(|v| v.as_array_mut()) else {
        return 0;
    };
    let stake_f = stake as f64;
    let mut n = 0;
    for option in options.iter_mut() {
        // Same fallback as `optionWeight()`: raw weight, else prob * total.
        let weight = match option.get("weight").and_then(|w| w.as_f64()) {
            Some(w) => w,
            None => match option.get("raw_weight").and_then(|w| w.as_f64()) {
                Some(w) => w,
                None => match option.get("prob").and_then(|p| p.as_f64()) {
                    Some(p) => p * total_weight,
                    None => continue,
                },
            },
        };
        let total = total_weight + stake_f;
        let buy_prob = (weight + stake_f) / total;
        if !(0.0..=1.0).contains(&buy_prob) || buy_prob <= 0.0 {
            continue;
        }
        if let Some(obj) = option.as_object_mut() {
            obj.insert("buy_prob".to_string(), serde_json::json!(buy_prob));
            obj.insert(
                "shares".to_string(),
                serde_json::json!((stake_f / buy_prob).floor() as u64),
            );
            n += 1;
        }
    }
    n
}

/// Annotate every event in a `{events: [...]}` list or a `{event: {...}}` reply.
pub fn annotate_events(body: &mut Value, stake: u64) -> usize {
    if let Some(events) = body.get_mut("events").and_then(|v| v.as_array_mut()) {
        return events.iter_mut().map(|e| annotate_event(e, stake)).sum();
    }
    if let Some(event) = body.get_mut("event") {
        return annotate_event(event, stake);
    }
    0
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn board() -> Value {
        json!({
            "id": 88013,
            "total_weight": 134283,
            "reward_multiplier": 1,
            "options": [
                {"id": 106459, "weight": 16114, "prob": 0.12},
                {"id": 106460, "weight": 128862, "prob": 0.9596},
                {"id": 106461, "weight": 5372, "prob": 0.04}
            ]
        })
    }

    #[test]
    fn matches_the_bundle_formula() {
        // (16114 + 100) / (134283 + 100)
        let e = buy_estimate(&board(), "106459", 100).expect("option exists");
        assert!(
            (e.buy_prob - 0.120655).abs() < 0.000001,
            "got {}",
            e.buy_prob
        );
        assert_eq!(e.shares, 828); // floor(100 / 0.120655…)
    }

    #[test]
    fn a_larger_stake_moves_the_probability() {
        let small = buy_estimate(&board(), "106461", 100).unwrap();
        let large = buy_estimate(&board(), "106461", 10_000).unwrap();
        assert!(large.buy_prob > small.buy_prob, "stake raises own share");
        assert!(large.shares > small.shares);
    }

    #[test]
    fn falls_back_to_prob_when_weight_is_absent() {
        // `optionWeight` uses prob * total_weight in that branch (675596-675598).
        let only_prob = json!({
            "total_weight": 1000,
            "options": [{"id": 1, "prob": 0.25}]
        });
        let e = buy_estimate(&only_prob, "1", 100).unwrap();
        // (250 + 100) / (1000 + 100)
        assert!(
            (e.buy_prob - 0.3181818).abs() < 0.000001,
            "got {}",
            e.buy_prob
        );
    }

    #[test]
    fn an_unknown_option_is_not_guessed() {
        assert!(buy_estimate(&board(), "999999", 100).is_none());
    }

    #[test]
    fn a_board_without_weights_is_refused() {
        let empty = json!({"id": 1, "options": []});
        assert!(buy_estimate(&empty, "1", 100).is_none());
        let no_total = json!({"options": [{"id": 1, "weight": 10}]});
        assert!(buy_estimate(&no_total, "1", 100).is_none());
    }

    #[test]
    fn annotating_keeps_the_server_prob_and_adds_the_buy_fields() {
        let mut board = board();
        let n = annotate_event(&mut board, 100);
        assert_eq!(n, 3, "every option with numbers is annotated");
        let first = &board["options"][0];
        // The server's own value must survive: it is the current weight share.
        assert_eq!(first["prob"], 0.12);
        assert!((first["buy_prob"].as_f64().unwrap() - 0.120655).abs() < 0.000001);
        assert_eq!(first["shares"], 828);
    }

    #[test]
    fn annotating_a_list_touches_every_event() {
        let mut body = json!({
            "events": [board(), board()],
            "next_ids": [1, 2]
        });
        let n = annotate_events(&mut body, 100);
        assert_eq!(n, 6, "two events x three options");
        assert_eq!(body["events"][0]["options"][0]["shares"], 828);
        assert_eq!(body["next_ids"][0], 1, "other fields are left alone");
    }

    #[test]
    fn annotating_a_single_event_reply_works() {
        let mut body = json!({"event": board(), "series": {}});
        assert_eq!(annotate_events(&mut body, 100), 3);
    }

    #[test]
    fn a_board_without_weights_is_left_untouched() {
        let mut body = json!({"events": [{"id": 1, "options": []}]});
        assert_eq!(annotate_events(&mut body, 100), 0);
        assert_eq!(body["events"][0]["options"].as_array().unwrap().len(), 0);
    }
}
