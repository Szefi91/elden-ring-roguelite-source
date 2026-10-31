//! Card draft logic -- pure, game-independent, unit-testable offline.
//!
//! Spec (Szefi, 2026-10-22): on level-up, draw 5 cards; the player picks 2.
//! Card types: Stat, Armor, Ability, Weapon (+ AshOfWar, Spell, Talisman).
//! The pool CONTENTS (concrete item ids, rarities) are filled in Phase 3 from the
//! game's param tables; this module owns the DRAW + PICK mechanism only.

use std::collections::HashSet;

pub const DRAW_COUNT: usize = 5;
/// Base picks (choose 2). At 4 talisman slots the draft adds one more pick (choose 3) --
/// see the sizing in lib.rs.
pub const PICK_COUNT: usize = 2;

/// Draft hand size scales with talisman slots (Szefi, 2026-10-23): base 3 cards, +1 per
/// unlocked slot, capped at 5 (4-slot max in ER). The actual per-draft size is computed
/// in lib.rs from the live talisman count.
pub const BASE_HAND_SIZE: usize = 3;
pub const MAX_HAND_SIZE: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CardKind {
    Stat,
    Armor,  // breastplate / body armor
    Shield,
    Weapon,
    AshOfWar,
    Spell,
    Talisman,
    Ability,
    Curse,  // a cursed card: a hidden risk/debuff, revealed only when taken
}

/// What a cursed card inflicts when the player takes it (Szefi, 2026-10-24: "átkos
/// dolgok ... mérgezést a vérzést"). Poison/Bleed drive the game's OWN status system
/// (a real SpEffect on the player's ChrIns); the rest use the already-proven
/// player-data write path, so they survive the Grace-suppression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurseEffect {
    Poison,              // poison buildup (SpEffect 20500330, applied many times to proc)
    Bleed,               // hemorrhage proc (SpEffect 1753 self-bleed, procs on one call)
    Madness,             // madness buildup (SpEffect 1782 Vyke self-madness) -- confirmed
    ScarletRot,          // scarlet rot buildup (SpEffect 4054 Lake of Rot) -- confirmed
    DeathBlight,         // deathblight buildup (SpEffect 1794 Eclipse Shotel) -- INSTANT DEATH
    Frostbite,           // frost buildup -- DEFERRED: no working self/environmental id yet
    Teleport,            // warp to a random base-game Site of Grace
    HalveHp,             // set current HP to half (Aurora: game::halve_hp, not saved)
    EmptyFlasks,         // remove all flask charges (Aurora: game::empty_flasks, PERSISTS)
    Frailty(u32),        // lose N Vigor -> lower max HP
    Weakness(usize, u32),// lose N from one attribute (index 0..7)
    RuneForfeit(u32),    // lose this percent (0..100) of currently held runes
}

impl CurseEffect {
    /// Short human name shown on the Confirm-reveal screen when the hidden price is paid.
    pub fn label(&self) -> &'static str {
        match self {
            CurseEffect::Poison => "Poison",
            CurseEffect::Bleed => "Bleeding",
            CurseEffect::Madness => "Madness",
            CurseEffect::ScarletRot => "Scarlet Rot",
            CurseEffect::DeathBlight => "Death Blight",
            CurseEffect::Frostbite => "Frostbite",
            CurseEffect::Teleport => "Teleported away",
            CurseEffect::HalveHp => "HP halved",
            CurseEffect::EmptyFlasks => "Flasks emptied",
            CurseEffect::Frailty(_) => "Frailty (Vigor drained)",
            CurseEffect::Weakness(_, _) => "Weakness (stat drained)",
            CurseEffect::RuneForfeit(_) => "Runes lost",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    pub kind: CardKind,
    /// Param row id of the concrete reward (weapon/armor/goods/...). 0 = placeholder.
    pub item_id: u32,
    pub name: String,
}

/// A weighted entry in the draft pool.
pub struct PoolEntry {
    pub card: Card,
    pub weight: u32,
}

/// Draw `DRAW_COUNT` distinct cards from a weighted pool using the supplied RNG
/// (a small injectable fn so the draw is deterministic in tests). Distinct by
/// item_id so the same reward never appears twice in one draft.
pub fn draw(pool: &[PoolEntry], mut rng_next: impl FnMut(u32) -> u32) -> Vec<Card> {
    let mut chosen: Vec<Card> = Vec::with_capacity(DRAW_COUNT);
    let mut used: HashSet<u32> = HashSet::new();
    // Guard against a pool too small to satisfy a distinct draw.
    let target = DRAW_COUNT.min(pool.iter().filter(|e| e.weight > 0).count());

    let mut guard = 0u32;
    while chosen.len() < target && guard < 10_000 {
        guard += 1;
        let total: u32 = pool
            .iter()
            .filter(|e| e.weight > 0 && !used.contains(&e.card.item_id))
            .map(|e| e.weight)
            .sum();
        if total == 0 {
            break;
        }
        let mut roll = rng_next(total);
        for e in pool.iter().filter(|e| e.weight > 0 && !used.contains(&e.card.item_id)) {
            if roll < e.weight {
                used.insert(e.card.item_id);
                chosen.push(e.card.clone());
                break;
            }
            roll -= e.weight;
        }
    }
    chosen
}

/// Validate a player's pick against the drawn hand: exactly PICK_COUNT, all from
/// the hand, no duplicates. Returns the picked cards or an error reason.
pub fn validate_pick(hand: &[Card], picks: &[usize]) -> Result<Vec<Card>, &'static str> {
    if picks.len() != PICK_COUNT {
        return Err("must pick exactly 2 cards");
    }
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(PICK_COUNT);
    for &i in picks {
        if i >= hand.len() {
            return Err("pick index out of range");
        }
        if !seen.insert(i) {
            return Err("cannot pick the same card twice");
        }
        out.push(hand[i].clone());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(kind: CardKind, id: u32) -> Card {
        Card { kind, item_id: id, name: format!("item{id}") }
    }

    fn sample_pool() -> Vec<PoolEntry> {
        (1..=12)
            .map(|i| PoolEntry { card: card(CardKind::Weapon, i), weight: 10 })
            .collect()
    }

    #[test]
    fn draws_five_distinct() {
        let pool = sample_pool();
        // deterministic "rng": cycle through a fixed sequence
        let seq = [0u32, 9, 3, 25, 7, 1, 50, 2];
        let mut k = 0;
        let hand = draw(&pool, |n| {
            let v = seq[k % seq.len()] % n;
            k += 1;
            v
        });
        assert_eq!(hand.len(), DRAW_COUNT);
        let ids: HashSet<u32> = hand.iter().map(|c| c.item_id).collect();
        assert_eq!(ids.len(), DRAW_COUNT, "all drawn cards must be distinct");
    }

    #[test]
    fn small_pool_does_not_loop_forever() {
        let pool: Vec<PoolEntry> = (1..=3)
            .map(|i| PoolEntry { card: card(CardKind::Stat, i), weight: 5 })
            .collect();
        let hand = draw(&pool, |n| n - 1);
        assert_eq!(hand.len(), 3, "draw caps at pool size when smaller than 5");
    }

    #[test]
    fn pick_must_be_exactly_two_distinct() {
        let hand: Vec<Card> = (0..5).map(|i| card(CardKind::Weapon, i)).collect();
        assert!(validate_pick(&hand, &[0, 1]).is_ok());
        assert!(validate_pick(&hand, &[0]).is_err());
        assert!(validate_pick(&hand, &[0, 1, 2]).is_err());
        assert!(validate_pick(&hand, &[0, 0]).is_err());
        assert!(validate_pick(&hand, &[0, 9]).is_err());
    }
}
