//! The wave minigame, played without a screen.
//!
//! The app renders the game and reports a list of tap timestamps; the server
//! replays that list against its own copy of the level and decides the outcome.
//! So the only thing needed to play headlessly is a tap list the replay accepts,
//! which means reproducing the level's physics rather than faking a result.
//!
//! Every constant and formula below was read out of the bundle:
//!
//! | what | value | source |
//! |---|---|---|
//! | `PAD` | 120 | 502612 |
//! | `PIER_X` | 180 | 502586 |
//! | `FIELD_W` / `EDGE` | 360 / 18 | 502576, 502592 |
//! | `ROWS` / `LIMIT_MS` | 8 / 30000 | 502578, 502580 |
//! | `JUMP_MS` / `LAND_TOL` | 220 / 6 | 502582, 502588 |
//! | island left edge | `((x0+PAD+dir·speed·t/1000) mod period) − PAD` | 502642 |
//! | landing tolerance | `tier ? 5−tier : LAND_TOL` | 503003 |
//! | ride deadline | `dir>0 ? FIELD_W−EDGE : EDGE`, `t0+ceil(dist·1000/speed)` | 502743 |
//! | stand offset clamp | `clamp(off, min(14,w/2), w−min(14,w/2))` | 502669 |
//!
//! Verified against the live server: a clear pays `free_coins` (20) once per day,
//! and a second clear that day pays nothing.

use serde_json::Value;

/// Padding subtracted after the period wrap (bundle 502612).
const PAD: f64 = 120.0;
/// Where the runner stands before the first jump (bundle 502586).
const PIER_X: f64 = 180.0;
/// Horizontal bound used by the drift calculation (bundle 502576, 502592).
const FIELD_W: f64 = 360.0;
const EDGE: f64 = 18.0;
/// Rows to clear (bundle 502578).
const ROWS: usize = 8;
/// The replay treats a tap past this as too late (bundle 502580).
const LIMIT_MS: f64 = 30_000.0;
/// Airborne time per jump (bundle 502582).
const JUMP_MS: f64 = 220.0;
/// Landing tolerance when the level has no tier (bundle 502588).
const LAND_TOL: f64 = 6.0;

/// One island on one row.
#[derive(Debug, Clone)]
struct Island {
    x0: f64,
    w: f64,
    /// Standable for this long before it crumbles; absent means never.
    crumble_ms: Option<f64>,
}

/// One row of the level.
#[derive(Debug, Clone)]
struct Row {
    dir: f64,
    speed: f64,
    period: f64,
    islands: Vec<Island>,
}

/// The level a `start` call returned.
#[derive(Debug, Clone)]
pub struct Level {
    rows: Vec<Row>,
    tier: Option<f64>,
}

impl Level {
    /// Parse the `level` object out of a `POST /minigame/wave/start` reply.
    ///
    /// Returns `None` when the reply carries no usable level instead of
    /// substituting an empty one: an invented level produces tap times the server
    /// rejects, which then looks like a game defect rather than a parse failure.
    pub fn from_start(body: &Value) -> Option<Self> {
        let level = body.get("level")?;
        let raw = level.get("rows")?.as_array()?;
        let mut rows = Vec::with_capacity(raw.len());
        for r in raw {
            let mut islands = Vec::new();
            for i in r.get("islands")?.as_array()? {
                islands.push(Island {
                    x0: i.get("x0")?.as_f64()?,
                    w: i.get("w")?.as_f64()?,
                    crumble_ms: i.get("crumble_ms").and_then(|c| c.as_f64()),
                });
            }
            if islands.is_empty() {
                return None;
            }
            rows.push(Row {
                dir: r.get("dir")?.as_f64()?,
                speed: r.get("speed")?.as_f64()?,
                period: r.get("period")?.as_f64()?,
                islands,
            });
        }
        if rows.is_empty() {
            return None;
        }
        Some(Self {
            rows,
            tier: level.get("tier").and_then(|t| t.as_f64()),
        })
    }

    fn tolerance(&self) -> f64 {
        match self.tier {
            Some(tier) => 5.0 - tier,
            None => LAND_TOL,
        }
    }

    /// Left edge of island `i` on `row` at time `t` (bundle 502642).
    fn island_left(&self, row: &Row, i: usize, t: f64) -> f64 {
        let p = row.islands[i].x0 + PAD + row.dir * row.speed * t / 1000.0;
        (p % row.period + row.period) % row.period - PAD
    }

    /// Stand offset clamp (bundle 502669): `Math.min(14, w/2)`, so the margin is
    /// half the island width capped at 14 — not 14 halved.
    fn stand_off(w: f64, off: f64) -> f64 {
        let m = (w / 2.0).min(14.0);
        off.max(m).min(w - m)
    }

    /// Where the runner is at time `t` (bundle 502904).
    fn tako_x(&self, s: &State, t: f64) -> f64 {
        match s.mode {
            Mode::Stand => {
                if s.row == 0 || s.isl < 0 {
                    s.off
                } else {
                    self.island_left(&self.rows[s.row - 1], s.isl as usize, t) + s.off
                }
            }
            Mode::Jump if s.hit => {
                let p = ((t - s.t0) / JUMP_MS).clamp(0.0, 1.0);
                let to = self.island_left(&self.rows[s.row - 1], s.isl as usize, t) + s.off;
                s.x0 + (to - s.x0) * p
            }
            _ => s.x0,
        }
    }

    /// Which island a landing at `x` hits, if any (bundle 503003).
    fn landing_island(&self, row_idx: usize, x: f64, t: f64) -> Option<usize> {
        if row_idx >= self.rows.len() {
            return None;
        }
        let row = &self.rows[row_idx];
        let tol = self.tolerance();
        for (i, island) in row.islands.iter().enumerate() {
            let left = self.island_left(row, i, t);
            if x >= left - tol && x <= left + island.w + tol {
                return Some(i);
            }
        }
        None
    }

    /// When the current ride reaches the edge (bundle 502743).
    fn drift_at(&self, r: usize, i: usize, off: f64, t0: f64) -> Option<f64> {
        if r < 1 {
            return None;
        }
        let row = &self.rows[r - 1];
        let x = self.island_left(row, i, t0) + off;
        let bound = if row.dir > 0.0 { FIELD_W - EDGE } else { EDGE };
        let dist = (bound - x) * row.dir;
        if dist <= 0.0 {
            return None;
        }
        Some(t0 + (dist * 1000.0 / row.speed).ceil())
    }

    /// Apply one tap at time `t` (bundle 504133).
    fn tap(&self, s: &State, t: f64) -> State {
        if s.mode != Mode::Stand || t >= LIMIT_MS || s.row >= ROWS {
            return s.clone();
        }
        let x = self.tako_x(s, t);
        let island = self.landing_island(s.row, x, t);
        let mut next = State {
            mode: Mode::Jump,
            t0: t,
            until: t + JUMP_MS,
            x0: x,
            row: s.row,
            isl: s.isl,
            off: s.off,
            hit: island.is_some(),
            best: s.best,
        };
        next.row = s.row + 1;
        match island {
            None => next.isl = -1,
            Some(i) => {
                let row = &self.rows[s.row];
                let left = self.island_left(row, i, t);
                next.off = Self::stand_off(row.islands[i].w, x - left);
                next.isl = i as i32;
            }
        }
        next
    }

    /// Advance the state to time `t` (bundle 503986).
    ///
    /// A miss ends the round. The app instead respawns the runner on a safe spot
    /// and continues; a replay built from a tap list cannot express that, because
    /// the respawn depends on the row and the clock. Stopping at the first miss
    /// keeps the generated list honest: every tap it emits is one the server
    /// also accepts as a landing.
    fn advance(&self, s0: &State, t: f64) -> State {
        let mut s = s0.clone();
        for _ in 0..1000 {
            match s.mode {
                Mode::Clear | Mode::Over => return s,
                Mode::Jump => {
                    if s.until > t {
                        return s;
                    }
                    let at = s.until;
                    if !s.hit {
                        s.mode = Mode::Over;
                        return s;
                    }
                    s.best = s.best.max(s.row);
                    if s.row >= ROWS {
                        s.mode = Mode::Clear;
                        s.t0 = at;
                        return s;
                    }
                    let isl = match s.isl {
                        i if i >= 0 => i as usize,
                        _ => {
                            s.mode = Mode::Over;
                            return s;
                        }
                    };
                    s.mode = Mode::Stand;
                    s.t0 = at;
                    let drift = self.drift_at(s.row, isl, s.off, at);
                    let crumble = self.rows[s.row - 1].islands[isl].crumble_ms;
                    let deadline = match (drift, crumble) {
                        (Some(d), Some(c)) => Some(d.min(at + c)),
                        (Some(d), None) => Some(d),
                        (None, Some(c)) => Some(at + c),
                        (None, None) => None,
                    };
                    match deadline {
                        Some(d) if d > at => s.until = d,
                        _ => {
                            // Already past the edge or crumbled: the ride ends
                            // immediately, before any further tap can land.
                            s.mode = Mode::Over;
                            return s;
                        }
                    }
                }
                Mode::Stand => {
                    if s.until > t {
                        return s;
                    }
                    s.mode = Mode::Over;
                    return s;
                }
            }
        }
        s
    }

    /// Find a tap list that clears the level, or `None`.
    ///
    /// Greedy rather than exhaustive: at each row, scan forward from the current
    /// time for the earliest tap that lands. The geometry is deterministic, so
    /// the same level always yields the same list.
    pub fn solve(&self) -> Option<Vec<u64>> {
        let mut s = State::default();
        let mut taps: Vec<u64> = Vec::new();
        let mut from = 0.0f64;
        while s.mode == Mode::Stand && s.row < ROWS && from < LIMIT_MS {
            let mut tap_t = from + 1.0;
            let mut landed = None;
            while tap_t < LIMIT_MS {
                let cand = self.tap(&s, tap_t);
                if cand.hit {
                    landed = Some((tap_t, cand));
                    break;
                }
                tap_t += 2.0;
            }
            let (t, cand) = landed?;
            taps.push(t as u64);
            s = self.advance(&cand, cand.until);
            from = cand.until;
            if s.mode == Mode::Clear {
                break;
            }
            if s.mode != Mode::Stand {
                return None;
            }
        }
        if s.mode == Mode::Clear || s.row >= ROWS {
            Some(taps)
        } else {
            None
        }
    }

    /// Time of the last tap a solved list depends on. Finishing before it has
    /// passed in real time makes the replay treat the remaining taps as future
    /// and record no clear — the failure mode this value exists to prevent.
    pub fn settle_ms(taps: &[u64]) -> u64 {
        let last = taps.last().copied().unwrap_or(0);
        last + JUMP_MS as u64 + 1
    }
}

/// The replay's states (bundle 502600-502609).
///
/// The app also has a fall state (code 2). It is absent here on purpose: a
/// respawn position depends on the row and the clock, which a replayed tap list
/// cannot express, so a miss ends the round instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Stand,
    Jump,
    Clear,
    Over,
}

// The codes are part of the recovered contract and are asserted by the tests;
// nothing in the solver branches on them.
#[cfg_attr(not(test), allow(dead_code))]
impl Mode {
    /// The integer the app stores in `mode` (bundle 502600-502609).
    ///
    /// Explicit rather than `as u8`: the variant order no longer matches the
    /// app's numbering once the unused fall state is dropped.
    pub fn code(self) -> u8 {
        match self {
            Mode::Stand => 0,
            Mode::Jump => 1,
            Mode::Clear => 3,
            Mode::Over => 4,
        }
    }

    /// The app's fall state, kept for the contract table.
    pub const FALL_CODE: u8 = 2;
}

/// The replay's mutable state.
#[derive(Debug, Clone)]
pub struct State {
    mode: Mode,
    row: usize,
    isl: i32,
    off: f64,
    t0: f64,
    /// For a jump this is the landing time; for a stand it is the deadline the
    /// runner can stay on the island.
    until: f64,
    x0: f64,
    hit: bool,
    best: usize,
}

impl Default for State {
    fn default() -> Self {
        Self {
            mode: Mode::Stand,
            row: 0,
            isl: -1,
            off: PIER_X,
            t0: 0.0,
            until: 0.0,
            x0: PIER_X,
            hit: false,
            best: 0,
        }
    }
}
#[cfg(test)]
mod tests {
    // Fixtures for the wave solver. The level is a real `POST /minigame/wave/start`
    // reply (tier 3, rules_version 1). The assertions check the properties the
    // replay guarantees (ordering, bounds) rather than one exact number list: the
    // greedy scan has no single correct answer, and any list the server accepts is
    // valid.

    use super::*;

    fn fixture() -> Level {
        let body: Value = serde_json::from_str(
        r#"{"play_id":1,"rules_version":1,"limit_ms":30000,"rows":8,
        "level":{"v":1,"day":"2026-11-26","tier":3,"rows":[
        {"dir":1,"speed":47,"period":841,"islands":[{"x0":561,"w":98},{"x0":2,"w":99},{"x0":275,"w":84}]},
        {"dir":-1,"speed":45,"period":746,"islands":[{"x0":32,"w":82},{"x0":247,"w":76},{"x0":449,"w":78}]},
        {"dir":1,"speed":62,"period":837,"islands":[{"x0":494,"w":87},{"x0":-48,"w":77},{"x0":204,"w":85}]},
        {"dir":-1,"speed":62,"period":781,"islands":[{"x0":213,"w":77},{"x0":500,"w":90},{"x0":-43,"w":78}]},
        {"dir":1,"speed":62,"period":777,"islands":[{"x0":-119,"w":81},{"x0":157,"w":82},{"x0":404,"w":86}]},
        {"dir":-1,"speed":71,"period":819,"islands":[{"x0":-27,"w":82},{"x0":216,"w":77},{"x0":555,"w":74}]},
        {"dir":1,"speed":75,"period":736,"islands":[{"x0":-80,"w":69},{"x0":182,"w":80},{"x0":462,"w":80}]},
        {"dir":-1,"speed":87,"period":821,"islands":[{"x0":324,"w":75},{"x0":542,"w":62},{"x0":-12,"w":71}]}]}}"#,
    )
    .expect("fixture is valid JSON");
        Level::from_start(&body).expect("fixture has a usable level")
    }

    #[test]
    fn parses_the_rows_and_islands_of_a_real_reply() {
        let level = fixture();
        assert_eq!(level.rows.len(), ROWS);
        assert!(level.tier.is_some());
        for row in &level.rows {
            assert_eq!(row.islands.len(), 3);
            assert!(row.period > 0.0);
            assert!(row.speed > 0.0);
        }
    }

    #[test]
    fn a_reply_without_a_level_is_not_invented() {
        // No `level` key, or empty rows: both must be None rather than an empty
        // level, because tap times from an invented level are rejected.
        assert!(Level::from_start(&serde_json::json!({"play_id": 1})).is_none());
        assert!(Level::from_start(&serde_json::json!({"level": {"rows": []}})).is_none());
        assert!(Level::from_start(&serde_json::json!({"level": {"rows": [{"dir": 1}]}})).is_none());
    }

    #[test]
    fn the_solver_clears_the_captured_level() {
        let taps = fixture().solve().expect("this level is clearable");
        assert_eq!(taps.len(), ROWS, "one tap per row");
        assert!(
            taps.windows(2).all(|w| w[1] > w[0]),
            "taps must be strictly increasing: {:?}",
            taps
        );
        let last = *taps.last().unwrap();
        assert!(
            last < LIMIT_MS as u64,
            "the last tap must be inside the limit, got {}",
            last
        );
    }

    #[test]
    fn settle_time_covers_the_last_tap() {
        // Finishing before the taps have happened in real time makes the replay
        // record no clear, which is easy to mistake for a broken solver.
        let taps = [100u64, 500];
        assert!(Level::settle_ms(&taps) > 500, "must pass the last tap");
        assert_eq!(Level::settle_ms(&[]), JUMP_MS as u64 + 1);
    }

    #[test]
    fn mode_codes_match_the_bundle() {
        // 502600-502609: the app stores these as integers, so the codes are part
        // of the contract even though this solver never uses the fall state
        // (a respawn cannot be expressed in a replay, so a miss ends the round).
        // Removing the unused fall state renumbered the enum, so the codes are
        // declared explicitly instead of being implied by the variant order.
        assert_eq!(Mode::Stand.code(), 0);
        assert_eq!(Mode::Jump.code(), 1);
        assert_eq!(Mode::Clear.code(), 3);
        assert_eq!(Mode::Over.code(), 4);
        assert_eq!(Mode::FALL_CODE, 2);
    }
}
