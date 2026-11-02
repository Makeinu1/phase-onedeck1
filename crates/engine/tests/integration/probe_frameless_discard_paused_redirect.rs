//! PROBE (fork-only, not for upstream): does an ordinary, frameless
//! effect-caused discard still count as a discard when its inner
//! hand -> graveyard move pauses for a CR 616.1 replacement-order choice?
//!
//! CR 701.9c: a card discarded but put somewhere other than a graveyard
//! is still discarded. Two Rest in Peace copies make two graveyard -> exile
//! redirects apply at once, so the owner must choose an order (CR 616.1).
//!
//! Cards (verbatim fixture Oracle text):
//!   Mind Rot: "Target player discards two cards."
//!   Rest in Peace: "When this enchantment enters, exile all graveyards. / If a
//!     card or token would be put into a graveyard from anywhere, exile it
//!     instead."
//!   Geth's Grimoire: "Whenever an opponent discards a card, you may draw a card."
//!
//! Control row: one Rest in Peace (no choice). Probe row: two copies (choice).
//! Both assert the rules-correct outcome: two discards recorded, two
//! `Discarded` events, two Grimoire triggers.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

struct Observed {
    discarded_events: usize,
    recorded_discards: u32,
    grimoire_triggers: usize,
    choices_answered: usize,
    hand_cards_in_exile: usize,
}

fn run(rest_in_peace_copies: usize) -> Option<Observed> {
    let db = load_db()?;
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // Mind Rot costs {2}{B}.
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    for seat in [P0, P1] {
        for _ in 0..4 {
            scenario.add_real_card(seat, "Swamp", Zone::Library, db);
        }
    }
    let grimoire = scenario.add_real_card(P0, "Geth's Grimoire", Zone::Battlefield, db);
    for _ in 0..rest_in_peace_copies {
        scenario.add_real_card(P0, "Rest in Peace", Zone::Battlefield, db);
    }
    let mind_rot = scenario.add_real_card(P0, "Mind Rot", Zone::Hand, db);
    let a = scenario.add_real_card(P1, "Grizzly Bears", Zone::Hand, db);
    let b = scenario.add_real_card(P1, "Swamp", Zone::Hand, db);
    let mut runner: GameRunner = scenario.build();

    let mut commit = runner.cast(mind_rot).free_cast().target_player(P1).commit();
    let mut events: Vec<GameEvent> = commit.events().to_vec();
    let mut choices_answered = 0;
    let mut grimoire_triggers = 0;
    for _ in 0..40 {
        let state = commit.state();
        let mind_rot_on_stack = state.stack.iter().any(|e| e.id == mind_rot);
        grimoire_triggers = grimoire_triggers.max(
            state
                .stack
                .iter()
                .filter(|e| e.source_id == grimoire)
                .count(),
        );
        let action = match &state.waiting_for {
            WaitingFor::ReplacementChoice { .. } => {
                choices_answered += 1;
                GameAction::ChooseReplacement { index: 0 }
            }
            WaitingFor::Priority { .. } if mind_rot_on_stack => GameAction::PassPriority,
            WaitingFor::Priority { .. } => break,
            other => panic!("probe: unexpected prompt {other:?}"),
        };
        let result = commit.act(action).expect("probe action accepted");
        events.extend(result.events);
    }
    let state = commit.state();
    let discarded_events = events
        .iter()
        .filter(|e| matches!(e, GameEvent::Discarded { player_id, .. } if *player_id == P1))
        .count();
    let recorded_discards = state
        .cards_discarded_this_turn_by_player
        .get(&P1)
        .copied()
        .unwrap_or(0);
    let hand_cards_in_exile = [a, b]
        .iter()
        .filter(|id: &&ObjectId| state.objects[*id].zone == Zone::Exile)
        .count();
    eprintln!(
        "PROBE rip={rest_in_peace_copies} choices={choices_answered} \
         discarded_events={discarded_events} recorded={recorded_discards} \
         grimoire_triggers={grimoire_triggers} exiled={hand_cards_in_exile}"
    );
    Some(Observed {
        discarded_events,
        recorded_discards,
        grimoire_triggers,
        choices_answered,
        hand_cards_in_exile,
    })
}

fn assert_two_discards(o: &Observed) {
    assert_eq!(
        o.hand_cards_in_exile, 2,
        "reach: both cards left the hand into exile"
    );
    assert_eq!(o.discarded_events, 2, "CR 701.9c: two Discarded events");
    assert_eq!(o.recorded_discards, 2, "two discards recorded this turn");
    assert_eq!(
        o.grimoire_triggers, 2,
        "CR 603.2: Grimoire triggers per discard"
    );
}

#[test]
fn probe_control_single_redirect_counts_both_discards() {
    let Some(o) = run(1) else { return };
    assert_eq!(o.choices_answered, 0, "reach: one redirect needs no choice");
    assert_two_discards(&o);
}

#[test]
fn probe_paused_redirect_counts_both_discards() {
    let Some(o) = run(2) else { return };
    assert!(
        o.choices_answered >= 2,
        "reach: each card's move paused for a CR 616.1 choice"
    );
    assert_two_discards(&o);
}
