//! PROBE (fork-only, not for upstream): does a frameless single-card discard
//! (the Sandbox `DebugAction::Discard` entry, which shares
//! `discard_caused_by_effect_with_source` with Learn's rummage and connive)
//! still count as a discard after its move pauses for a replacement choice?
//!
//! CR 701.9c: a card discarded but put somewhere other than its owner's
//! graveyard is still discarded.
//!
//! Cards (verbatim fixture Oracle text):
//!   Rest in Peace: "... If a card or token would be put into a graveyard from
//!     anywhere, exile it instead."
//!   Geth's Grimoire: "Whenever an opponent discards a card, you may draw a card."
//!   Library of Leng: "... If an effect causes you to discard a card, discard
//!     it, but you may put it on top of your library instead of into your
//!     graveyard."

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::{DebugAction, GameAction};
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

struct Observed {
    choices: usize,
    discarded_events: usize,
    recorded: u32,
    grimoire_triggers: usize,
    zone: Zone,
}

/// `rip` copies of Rest in Peace (P1), optional Library of Leng (P0), and an
/// opponent's Geth's Grimoire (P1). Answers every replacement prompt with the
/// candidate `pick` selects.
fn run(rip: usize, leng: bool, pick: fn(&str) -> bool) -> Option<Observed> {
    let db = load_db()?;
    let mut s = GameScenario::new_n_player(2, 42);
    s.at_phase(Phase::PreCombatMain);
    for seat in [P0, P1] {
        for _ in 0..4 {
            s.add_real_card(seat, "Swamp", Zone::Library, db);
        }
    }
    let grimoire = s.add_real_card(P1, "Geth's Grimoire", Zone::Battlefield, db);
    for _ in 0..rip {
        s.add_real_card(P1, "Rest in Peace", Zone::Battlefield, db);
    }
    if leng {
        s.add_real_card(P0, "Library of Leng", Zone::Battlefield, db);
    }
    let card: ObjectId = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let mut runner: GameRunner = s.build();
    runner.state_mut().debug_mode = true;

    let mut events = runner
        .act(GameAction::Debug(DebugAction::Discard { object_id: card }))
        .expect("debug discard accepted")
        .events;
    let mut choices = 0;
    while let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    {
        choices += 1;
        assert!(choices <= 4, "probe: runaway prompts");
        let index = candidates
            .iter()
            .position(|c| pick(&c.description))
            .unwrap_or(0);
        events.extend(
            runner
                .act(GameAction::ChooseReplacement { index })
                .expect("answer accepted")
                .events,
        );
    }
    let state = runner.state();
    let o = Observed {
        choices,
        discarded_events: events
            .iter()
            .filter(|e| matches!(e, GameEvent::Discarded { player_id, .. } if *player_id == P0))
            .count(),
        recorded: state
            .cards_discarded_this_turn_by_player
            .get(&P0)
            .copied()
            .unwrap_or(0),
        grimoire_triggers: state
            .stack
            .iter()
            .filter(|e| e.source_id == grimoire)
            .count(),
        zone: state.objects[&card].zone,
    };
    eprintln!(
        "PROBE rip={rip} leng={leng} choices={} discarded={} recorded={} grimoire={} zone={:?}",
        o.choices, o.discarded_events, o.recorded, o.grimoire_triggers, o.zone
    );
    Some(o)
}

fn assert_one_discard(o: &Observed) {
    assert_eq!(o.discarded_events, 1, "CR 701.9c: one Discarded event");
    assert_eq!(o.recorded, 1, "one discard recorded this turn");
    assert_eq!(o.grimoire_triggers, 1, "CR 603.2: Grimoire triggers once");
}

#[test]
fn probe_control_one_redirect() {
    let Some(o) = run(1, false, |_| true) else {
        return;
    };
    assert_eq!(o.choices, 0);
    assert_eq!(o.zone, Zone::Exile);
    assert_one_discard(&o);
}

#[test]
fn probe_two_redirects_pause_the_move() {
    let Some(o) = run(2, false, |_| true) else {
        return;
    };
    assert!(o.choices >= 1, "reach: CR 616.1 order prompt");
    assert_eq!(o.zone, Zone::Exile);
    assert_one_discard(&o);
}

#[test]
fn probe_library_of_leng_accept() {
    let Some(o) = run(0, true, |d| d != "Decline") else {
        return;
    };
    assert!(o.choices >= 1, "reach: Library of Leng prompt");
    assert_eq!(o.zone, Zone::Library);
    assert_one_discard(&o);
}

#[test]
fn probe_library_of_leng_decline() {
    let Some(o) = run(0, true, |d| d == "Decline") else {
        return;
    };
    assert!(o.choices >= 1, "reach: Library of Leng prompt");
    assert_eq!(o.zone, Zone::Graveyard);
    assert_one_discard(&o);
}
