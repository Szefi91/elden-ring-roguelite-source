//! Rune-threshold auto level-up logic.
//!
//! Design (Szefi, 2026-10-22): the base game already absorbs runes on kill and
//! already knows the rune cost of every level (1..=713 total soul levels incl. DLC).
//! We do NOT invent an XP system. We watch the player's held-rune count, and when it
//! reaches the vanilla cost of the next level, we level up *in place* (subtract the
//! runes, bump soul level) and open the card draft -- instead of waiting for a Grace.
//!
//! This module is a stub for Phase 1: the real hooks (read runes / soul level, write
//! them) get wired in Phase 2 via the game-data manager. For now it only carries the
//! cost formula so the rest of the code can compile and be unit-tested offline.

/// Runes required to go from `level` to `level + 1` in Elden Ring.
///
/// This is the exact vanilla soul-level cost (FromSoft's CalcCorrectGraph row 200,
/// "Leveling Cost"), so our auto level-up thresholds match the Grace "Level Up" menu
/// one-for-one. The earlier cubic here was only a rough placeholder and charged far
/// too much (e.g. 1432 vs 829 at level 10), which is what Szefi spotted in-game.
///
///   n    = max(0, level - 11) * 0.02
///   cost = floor( (n + 0.1) * (level + 81)^2 + 1 )
///
/// Verified against the real in-game table: level 1 -> 673, level 10 -> 829.
pub fn rune_cost_for_next_level(level: u32) -> u64 {
    let l = level as f64;
    let n = (l - 11.0).max(0.0) * 0.02;
    let cost = (n + 0.1) * (l + 81.0).powi(2) + 1.0;
    cost.floor().max(0.0) as u64
}

/// Given held runes and current level, how many levels can we step, and the runes
/// left over afterwards. Used by the loop to drip boss payouts one level at a time.
pub fn levels_affordable(mut runes: u64, mut level: u32) -> (u32, u64) {
    let mut gained = 0u32;
    loop {
        let cost = rune_cost_for_next_level(level);
        if cost == 0 || runes < cost {
            break;
        }
        runes -= cost;
        level += 1;
        gained += 1;
    }
    (gained, runes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_is_monotonic() {
        let mut prev = 0;
        for lvl in 1..200 {
            let c = rune_cost_for_next_level(lvl);
            assert!(c >= prev, "cost should not decrease at level {lvl}");
            prev = c;
        }
    }

    #[test]
    fn boss_payout_steps_multiple_levels() {
        // A big payout at a low level should grant several levels at once.
        let (gained, left) = levels_affordable(10_000, 1);
        assert!(gained >= 3, "10k runes at level 1 should be 3+ levels, got {gained}");
        assert!(left < rune_cost_for_next_level(1 + gained));
    }
}
