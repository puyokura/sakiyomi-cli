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

    let weight = match chosen.get("weight").and_then(|w| w.as_f64()) {
        Some(w) => w,
        None => chosen.get("prob")?.as_f64()? * total_weight,
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
    fn shares_are_the_floor_of_stake_over_probability() {
        // The account's own settled positions confirm this relation: see
        // VERIFICATION.md. A rounding change here would break that agreement.
        let e = buy_estimate(&board(), "106460", 100).unwrap();
        let expected = (100.0 / e.buy_prob).floor() as u64;
        assert_eq!(e.shares, expected);
    }
}
