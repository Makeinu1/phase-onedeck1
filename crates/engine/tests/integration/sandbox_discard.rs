//! Sandbox `DebugAction::Discard`: discard one hand card through the real
//! discard authority, as an effect-caused discard with no source object.
//!
//! Every row drives `GameRunner::act(GameAction::Debug(..))` (or `apply()` for
//! a non-priority seat) from `WaitingFor::Priority` in `PreCombatMain` with
//! `debug_mode` on, using `GameScenarioDbExt::add_real_card`.
//!
//! Verbatim Oracle text, as the committed fixture stores it:
//!   Geth's Grimoire: "Whenever an opponent discards a card, you may draw a card."
//!   Waste Not: "Whenever an opponent discards a creature card, create a 2/2
//!     black Zombie creature token. / Whenever an opponent discards a land
//!     card, add {B}{B}. / Whenever an opponent discards a noncreature,
//!     nonland card, draw a card."
//!   Rest in Peace: "When this enchantment enters, exile all graveyards. / If a
//!     card or token would be put into a graveyard from anywhere, exile it
//!     instead."
//!   Library of Leng: "You have no maximum hand size. / If an effect causes you
//!     to discard a card, discard it, but you may put it on top of your library
//!     instead of into your graveyard."
//!   Violent Eruption: "Violent Eruption deals 4 damage divided as you choose
//!     among any number of targets. / Madness {1}{R}{R} (If you discard this
//!     card, discard it into exile. When you do, cast it for its madness cost or
//!     put it into your graveyard.)"
//!   Loxodon Smiter: "This spell can't be countered. / If a spell or ability an
//!     opponent controls causes you to discard this card, put it onto the
//!     battlefield instead of putting it into your graveyard."

use std::collections::BTreeSet;

use engine::database::card_db::CardDatabase;
use engine::game::engine::apply;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::{DebugAction, GameAction};
use engine::types::events::GameEvent;
use engine::types::game_state::{CastOfferKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Library cards per seat, so no row can end on a draw from an empty library.
const LIBRARY_PER_SEAT: usize = 4;

fn table(db: &CardDatabase, setup: impl FnOnce(&mut GameScenario, &CardDatabase)) -> GameRunner {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for seat in [P0, P1] {
        for _ in 0..LIBRARY_PER_SEAT {
            scenario.add_real_card(seat, "Swamp", Zone::Library, db);
        }
    }
    setup(&mut scenario, db);
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    runner
}

fn discard(object_id: ObjectId) -> GameAction {
    GameAction::Debug(DebugAction::Discard { object_id })
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn discarded_events(events: &[GameEvent]) -> Vec<(PlayerId, ObjectId)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::Discarded {
                player_id,
                object_id,
                ..
            } => Some((*player_id, *object_id)),
            _ => None,
        })
        .collect()
}

fn discarded_with_sources(events: &[GameEvent]) -> Vec<(PlayerId, ObjectId, Option<ObjectId>)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::Discarded {
                player_id,
                object_id,
                source_id,
                ..
            } => Some((*player_id, *object_id, *source_id)),
            _ => None,
        })
        .collect()
}

fn discard_count(runner: &GameRunner, player: PlayerId) -> u32 {
    runner
        .state()
        .cards_discarded_this_turn_by_player
        .get(&player)
        .copied()
        .unwrap_or(0)
}

fn has_debug_used(events: &[GameEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, GameEvent::DebugActionUsed { .. }))
}

fn is_priority(runner: &GameRunner) -> bool {
    matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
}

/// What a rejected action must leave exactly as it was: every zone's contents,
/// life totals, the prompt, the stack and the state revision. The whole
/// `GameState` is not compared, because the action boundary also records the
/// rejected attempt itself (the same rollback-set approach as `engine_debug`'s
/// rejection tests).
fn digest(runner: &GameRunner) -> serde_json::Value {
    let state = runner.state();
    let players: Vec<_> = state
        .players
        .iter()
        .map(|p| (p.life, &p.hand, &p.library, &p.graveyard))
        .collect();
    serde_json::json!({
        "players": players,
        "battlefield": &state.battlefield,
        "exile": &state.exile,
        "stack_len": state.stack.len(),
        "waiting_for": &state.waiting_for,
        "state_revision": state.state_revision,
        "pending_replacement": &state.pending_replacement,
        "resolution_stack": &state.resolution_stack,
        "discard_counts": &state.cards_discarded_this_turn_by_player,
    })
}

fn count_named_on_battlefield(runner: &GameRunner, player: PlayerId, name: &str) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.controller == player && o.name == name)
        })
        .count()
}

/// R1: a basic discard goes hand -> graveyard as exactly one discard.
#[test]
fn r1_basic_discard_goes_to_graveyard() {
    let Some(db) = load_db() else { return };
    let mut card = None;
    let mut runner = table(db, |s, db| {
        card = Some(s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db));
    });
    let card = card.unwrap();

    let result = runner.act(discard(card)).expect("discard succeeds");

    assert!(has_debug_used(&result.events));
    assert_eq!(discarded_events(&result.events), vec![(P0, card)]);
    assert_eq!(zone_of(&runner, card), Zone::Graveyard);
    assert!(runner.state().stack.is_empty());
    assert!(is_priority(&runner));
}

/// R2: an opponent's discard trigger fires exactly once; a player's own
/// discard does not trigger it.
#[test]
fn r2_opponent_discard_trigger_fires_once() {
    let Some(db) = load_db() else { return };
    let mut ids = None;
    let mut runner = table(db, |s, db| {
        let grimoire = s.add_real_card(P1, "Geth's Grimoire", Zone::Battlefield, db);
        let card = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
        ids = Some((grimoire, card));
    });
    let (grimoire, card) = ids.unwrap();

    let result = runner.act(discard(card)).expect("discard succeeds");

    assert_eq!(discarded_events(&result.events), vec![(P0, card)]);
    assert_eq!(runner.state().stack.len(), 1, "one Grimoire trigger");
    assert_eq!(runner.state().stack[0].source_id, grimoire);
    assert!(is_priority(&runner));
}

#[test]
fn r2_own_discard_does_not_trigger_opponents_only_card() {
    let Some(db) = load_db() else { return };
    let mut card = None;
    let mut runner = table(db, |s, db| {
        s.add_real_card(P1, "Geth's Grimoire", Zone::Battlefield, db);
        card = Some(s.add_real_card(P1, "Grizzly Bears", Zone::Hand, db));
    });
    let card = card.unwrap();

    let result = runner.act(discard(card)).expect("discard succeeds");

    // Reach guard: the discard happened, by P1.
    assert_eq!(discarded_events(&result.events), vec![(P1, card)]);
    assert!(runner.state().stack.is_empty());
}

/// R3: the typed opponent trigger reads the discarded card.
#[test]
fn r3_waste_not_zombie_for_creature_card_only() {
    let Some(db) = load_db() else { return };
    let mut card = None;
    let mut runner = table(db, |s, db| {
        s.add_real_card(P1, "Waste Not", Zone::Battlefield, db);
        card = Some(s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db));
    });
    let card = card.unwrap();
    assert_eq!(count_named_on_battlefield(&runner, P1, "Zombie"), 0);

    runner.act(discard(card)).expect("discard succeeds");
    assert_eq!(runner.state().stack.len(), 1, "one Waste Not trigger");
    runner.resolve_top();

    assert_eq!(count_named_on_battlefield(&runner, P1, "Zombie"), 1);
}

#[test]
fn r3_waste_not_no_zombie_for_land_card() {
    let Some(db) = load_db() else { return };
    let mut card = None;
    let mut runner = table(db, |s, db| {
        s.add_real_card(P1, "Waste Not", Zone::Battlefield, db);
        card = Some(s.add_real_card(P0, "Swamp", Zone::Hand, db));
    });
    let card = card.unwrap();

    runner.act(discard(card)).expect("discard succeeds");
    // Reach guard: the land-card trigger (not the creature one) is on the stack.
    assert_eq!(runner.state().stack.len(), 1, "one Waste Not trigger");
    runner.resolve_top();

    assert_eq!(count_named_on_battlefield(&runner, P1, "Zombie"), 0);
}

/// R4: a redirected discard is still a discard (CR 701.9c per discard.rs).
#[test]
fn r4_rest_in_peace_redirect_is_still_a_discard() {
    let Some(db) = load_db() else { return };
    let mut card = None;
    let mut runner = table(db, |s, db| {
        s.add_real_card(P1, "Rest in Peace", Zone::Battlefield, db);
        s.add_real_card(P1, "Geth's Grimoire", Zone::Battlefield, db);
        card = Some(s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db));
    });
    let card = card.unwrap();

    let result = runner.act(discard(card)).expect("discard succeeds");

    assert_eq!(zone_of(&runner, card), Zone::Exile);
    assert_eq!(discarded_events(&result.events), vec![(P0, card)]);
    assert_eq!(runner.state().stack.len(), 1, "one Grimoire trigger");
}

/// R5: Madness offers its cast after the Madness trigger resolves.
#[test]
fn r5_madness_offers_cast_then_decline_goes_to_graveyard() {
    let Some(db) = load_db() else { return };
    let mut card = None;
    let mut runner = table(db, |s, db| {
        card = Some(s.add_real_card(P0, "Violent Eruption", Zone::Hand, db));
    });
    let card = card.unwrap();

    runner.act(discard(card)).expect("discard succeeds");
    assert_eq!(zone_of(&runner, card), Zone::Exile);
    assert_eq!(runner.state().stack.len(), 1, "one Madness trigger");

    runner.resolve_top();
    match &runner.state().waiting_for {
        WaitingFor::CastOffer {
            player,
            kind: CastOfferKind::Madness { object_id, .. },
        } => {
            assert_eq!(*player, P0);
            assert_eq!(*object_id, card);
        }
        other => panic!("expected the Madness cast offer, got {other:?}"),
    }

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the Madness cast succeeds");
    assert_eq!(zone_of(&runner, card), Zone::Graveyard);
}

/// R6: the cause is Effect, so Library of Leng's replacement applies.
#[test]
fn r6_library_of_leng_applies_to_sandbox_discard() {
    let db = load_db().expect("Library discard regression requires the committed card fixture");
    let mut ids = None;
    let mut runner = table(db, |s, db| {
        s.add_real_card(P0, "Library of Leng", Zone::Battlefield, db);
        let grimoire = s.add_real_card(P1, "Geth's Grimoire", Zone::Battlefield, db);
        let card = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
        ids = Some((grimoire, card));
    });
    let (grimoire, card) = ids.unwrap();
    let before = discard_count(&runner, P0);

    let parked = runner.act(discard(card)).expect("discard succeeds");
    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { player, .. } => assert_eq!(*player, P0),
        other => panic!("expected the Library of Leng prompt, got {other:?}"),
    }
    assert_eq!(zone_of(&runner, card), Zone::Hand);
    assert!(discarded_with_sources(&parked.events).is_empty());
    assert_eq!(discard_count(&runner, P0), before);
    assert!(runner.state().stack.is_empty());

    // Persist the actual producer-owned prompt before publicly resuming it.
    assert!(runner
        .state()
        .pending_replacement
        .as_ref()
        .unwrap()
        .discard_provenance
        .is_some());
    let original = runner
        .state()
        .pending_replacement
        .as_ref()
        .unwrap()
        .discard_provenance
        .as_deref()
        .unwrap()
        .observation
        .as_deref()
        .unwrap()
        .subject
        .identity;
    let encoded = serde_json::to_value(runner.state()).expect("serialize Library prompt");
    *runner.state_mut() = serde_json::from_value(encoded).expect("restore Library prompt");
    let result = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("choosing the library option succeeds");
    let state = runner.state();
    let top = state
        .players
        .iter()
        .find(|p| p.id == P0)
        .and_then(|p| p.library.front().copied());
    assert_eq!(top, Some(card));
    assert_eq!(zone_of(&runner, card), Zone::Library);
    // CR 701.9c + CR 603.2c: a redirected discard is one occurrence.
    assert_eq!(
        discarded_with_sources(&result.events),
        vec![(P0, card, None)]
    );
    assert_eq!(discard_count(&runner, P0), before + 1);
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].source_id, grimoire);
    assert!(runner.state().pending_replacement.is_none());
    assert_discard_receipt(
        &runner,
        &result.events,
        card,
        original,
        Zone::Library,
        false,
        grimoire,
    );
    assert!(is_priority(&runner));
}

/// R7: a parked choice refuses a second debug discard; the graveyard option
/// completes the first.
#[test]
fn r7_parked_choice_refuses_second_debug_discard() {
    let db = load_db().expect("Library discard regression requires the committed card fixture");
    let mut ids = None;
    let mut runner = table(db, |s, db| {
        s.add_real_card(P0, "Library of Leng", Zone::Battlefield, db);
        let grimoire = s.add_real_card(P1, "Geth's Grimoire", Zone::Battlefield, db);
        let first = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
        let second = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
        ids = Some((grimoire, first, second));
    });
    let (grimoire, first, second) = ids.unwrap();
    let count_before = discard_count(&runner, P0);

    let parked = runner.act(discard(first)).expect("discard parks a choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(discarded_with_sources(&parked.events).is_empty());
    assert_eq!(zone_of(&runner, first), Zone::Hand);
    assert_eq!(discard_count(&runner, P0), count_before);

    let before = digest(&runner);
    assert!(runner.act(discard(second)).is_err());
    assert_eq!(digest(&runner), before, "a refused action changes nothing");

    let original = runner
        .state()
        .pending_replacement
        .as_ref()
        .unwrap()
        .discard_provenance
        .as_deref()
        .unwrap()
        .observation
        .as_deref()
        .unwrap()
        .subject
        .identity;
    let result = runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("choosing the graveyard option succeeds");
    assert_eq!(zone_of(&runner, first), Zone::Graveyard);
    assert_eq!(zone_of(&runner, second), Zone::Hand);
    // CR 701.9a + CR 603.2c: declining the redirect still discards once.
    assert_eq!(
        discarded_with_sources(&result.events),
        vec![(P0, first, None)]
    );
    assert_eq!(discard_count(&runner, P0), count_before + 1);
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].source_id, grimoire);
    assert!(runner.state().pending_replacement.is_none());
    assert_discard_receipt(
        &runner,
        &result.events,
        first,
        original,
        Zone::Graveyard,
        false,
        grimoire,
    );
    assert!(is_priority(&runner));
}

/// R8: no source object, so "a spell or ability an opponent controls causes you
/// to discard this card" does not apply (deliberate boundary).
#[test]
fn r8_no_source_means_loxodon_smiter_goes_to_graveyard() {
    let Some(db) = load_db() else { return };
    let mut card = None;
    let mut runner = table(db, |s, db| {
        card = Some(s.add_real_card(P0, "Loxodon Smiter", Zone::Hand, db));
    });
    let card = card.unwrap();

    let result = runner.act(discard(card)).expect("discard succeeds");

    assert_eq!(discarded_events(&result.events), vec![(P0, card)]);
    assert_eq!(zone_of(&runner, card), Zone::Graveyard);
}

/// R9: the discarding player is the card's owner, not the submitting seat.
#[test]
fn r9_discarder_is_owner_not_submitter() {
    let Some(db) = load_db() else { return };
    let mut ids = None;
    let mut runner = table(db, |s, db| {
        let grimoire = s.add_real_card(P1, "Geth's Grimoire", Zone::Battlefield, db);
        let card = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
        ids = Some((grimoire, card));
    });
    let (grimoire, card) = ids.unwrap();
    runner.state_mut().debug_permitted = BTreeSet::from([P0, P1]);

    let result = apply(runner.state_mut(), P1, discard(card)).expect("P1 may submit");

    assert_eq!(discarded_events(&result.events), vec![(P0, card)]);
    assert_eq!(runner.state().stack.len(), 1, "one Grimoire trigger");
    assert_eq!(runner.state().stack[0].source_id, grimoire);
}

/// CR 701.9a: a paused discard still belongs to its card's owner.
#[test]
fn r9_paused_discarder_is_owner_not_submitter_for_both_answers() {
    use engine::game::game_object::ObjectIncarnationRefExt;
    use engine::types::identifiers::ObjectIncarnationRef;
    let db = load_db().expect("Library owner regression requires the committed card fixture");
    for index in [0, 1] {
        let mut ids = None;
        let mut runner = table(db, |s, db| {
            s.add_real_card(P0, "Library of Leng", Zone::Battlefield, db);
            let grimoire = s.add_real_card(P1, "Geth's Grimoire", Zone::Battlefield, db);
            let card = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
            ids = Some((grimoire, card));
        });
        let (grimoire, card) = ids.unwrap();
        runner.state_mut().debug_permitted = BTreeSet::from([P0, P1]);
        let owner_before = discard_count(&runner, P0);
        let submitter_before = discard_count(&runner, P1);
        let original = ObjectIncarnationRef::from_object(&runner.state().objects[&card]);
        let parked = apply(runner.state_mut(), P1, discard(card)).expect("P1 may submit");
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player: P0, .. }
        ));
        assert_eq!(zone_of(&runner, card), Zone::Hand);
        assert_eq!(
            runner
                .state()
                .pending_replacement
                .as_ref()
                .unwrap()
                .discard_provenance
                .as_deref()
                .unwrap()
                .original_hand,
            Some(original)
        );
        assert!(discarded_with_sources(&parked.events).is_empty());
        let result = apply(
            runner.state_mut(),
            P0,
            GameAction::ChooseReplacement { index },
        )
        .expect("owner answers");
        // CR 701.9a + CR 701.9c + CR 603.2c: owner and absent source survive either answer.
        assert_eq!(
            discarded_with_sources(&result.events),
            vec![(P0, card, None)]
        );
        assert_eq!(discard_count(&runner, P0), owner_before + 1);
        assert_eq!(discard_count(&runner, P1), submitter_before);
        assert_eq!(
            zone_of(&runner, card),
            if index == 0 {
                Zone::Library
            } else {
                Zone::Graveyard
            }
        );
        assert_eq!(runner.state().stack.len(), 1);
        assert_eq!(runner.state().stack[0].source_id, grimoire);
        assert_discard_receipt(
            &runner,
            &result.events,
            card,
            original,
            if index == 0 {
                Zone::Library
            } else {
                Zone::Graveyard
            },
            false,
            grimoire,
        );
        assert!(is_priority(&runner));
    }
}

/// R10: rejections leave state unchanged.
#[test]
fn r10_rejections_leave_state_unchanged() {
    let Some(db) = load_db() else { return };
    let mut ids = None;
    let mut runner = table(db, |s, db| {
        let hand = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
        let battlefield = s.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
        let graveyard = s.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
        let library = s.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
        ids = Some((hand, battlefield, graveyard, library));
    });
    let (hand, battlefield, graveyard, library) = ids.unwrap();

    for (label, id) in [
        ("unknown id", ObjectId(9_999)),
        ("battlefield card", battlefield),
        ("graveyard card", graveyard),
        ("library card", library),
    ] {
        let before = digest(&runner);
        assert!(runner.act(discard(id)).is_err(), "{label} must be rejected");
        assert_eq!(digest(&runner), before, "{label} must change nothing");
    }

    // Debug mode off.
    runner.state_mut().debug_mode = false;
    let before = digest(&runner);
    assert!(runner.act(discard(hand)).is_err());
    assert_eq!(digest(&runner), before);
    runner.state_mut().debug_mode = true;

    // Owner no longer in the game.
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .is_eliminated = true;
    // P1 submits, so only the new owner check can reject the action.
    runner.state_mut().debug_permitted = BTreeSet::from([P0, P1]);
    let before = digest(&runner);
    assert!(apply(runner.state_mut(), P1, discard(hand)).is_err());
    assert_eq!(digest(&runner), before);
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .is_eliminated = false;

    // Reach guard: with every gate cleared, the same action is accepted.
    runner.act(discard(hand)).expect("a hand card is accepted");
    assert_eq!(zone_of(&runner, hand), Zone::Graveyard);
}

#[test]
fn r10_non_priority_window_rejects_discard() {
    let Some(db) = load_db() else { return };
    let mut ids = None;
    let mut runner = table(db, |s, db| {
        s.add_real_card(P0, "Library of Leng", Zone::Battlefield, db);
        let first = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
        let second = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
        ids = Some((first, second));
    });
    let (first, second) = ids.unwrap();

    runner.act(discard(first)).expect("discard parks a choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    let before = digest(&runner);
    assert!(runner.act(discard(second)).is_err());
    assert_eq!(digest(&runner), before);
}

fn original_library_choice() -> (GameRunner, ObjectId, ObjectId) {
    let db = load_db().expect("required real Library and Grimoire fixture");
    let mut ids = None;
    let mut runner = table(db, |scenario, db| {
        scenario.add_real_card(P0, "Library of Leng", Zone::Battlefield, db);
        let observer = scenario.add_real_card(P1, "Geth's Grimoire", Zone::Battlefield, db);
        let card = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
        ids = Some((card, observer));
    });
    let (card, observer) = ids.unwrap();
    let first = runner
        .act(discard(card))
        .expect("real Library producer parks");
    assert!(matches!(
        first.waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    assert!(discarded_events(&first.events).is_empty());
    assert!(
        runner.state().pending_discard_batch.is_none(),
        "this is the no-frame public route"
    );
    (runner, card, observer)
}

#[test]
fn r11_library_old_answers_reject_out_of_hand_and_returned_incarnations_after_restore() {
    use engine::game::game_object::ObjectIncarnationRefExt;
    use engine::types::game_state::{GameState, PersistedGameState};
    use engine::types::identifiers::ObjectIncarnationRef;
    for index in [0, 1] {
        for round_trip in [false, true] {
            let (mut runner, card, observer) = original_library_choice();
            let original = ObjectIncarnationRef::from_object(&runner.state().objects[&card]);
            assert_eq!(
                runner
                    .state()
                    .pending_replacement
                    .as_ref()
                    .unwrap()
                    .discard_provenance
                    .as_deref()
                    .unwrap()
                    .original_hand,
                Some(original)
            );
            for to_zone in [Zone::Graveyard]
                .into_iter()
                .chain(round_trip.then_some(Zone::Hand))
            {
                let moved = runner
                    .act(GameAction::Debug(DebugAction::MoveToZone {
                        object_id: card,
                        to_zone,
                        library_position: None,
                        simulate: false,
                    }))
                    .expect("public raw debug movement remains allowed during the prompt");
                assert!(matches!(
                    moved.waiting_for,
                    WaitingFor::ReplacementChoice { .. }
                ));
                assert!(discarded_events(&moved.events).is_empty());
            }
            assert!(!original.is_current(runner.state()));
            assert_ne!(
                runner.state().objects[&card].incarnation,
                original.incarnation
            );
            for ingress in 0..4 {
                let state: GameState = match ingress {
                    0 => runner.state().clone(),
                    1 => serde_json::from_value(serde_json::to_value(runner.state()).unwrap())
                        .unwrap(),
                    _ => {
                        let saved = if ingress == 2 {
                            PersistedGameState::Raw(Box::new(runner.state().clone()))
                        } else {
                            PersistedGameState::capture(runner.state().clone())
                        };
                        serde_json::from_value::<PersistedGameState>(
                            serde_json::to_value(saved).unwrap(),
                        )
                        .expect("known stale authority restores without rebinding")
                        .into_game_state()
                        .unwrap()
                    }
                };
                let mut restored = GameRunner::from_state(state);
                let before = serde_json::to_value(restored.state()).unwrap();
                let journal = restored.state().resolved_rules_journal.clone();
                let error = apply(
                    restored.state_mut(),
                    P0,
                    GameAction::ChooseReplacement { index },
                )
                .expect_err("old Library answer cannot move a new occurrence");
                assert!(error.to_string().contains("original-hand authority"));
                assert_eq!(serde_json::to_value(restored.state()).unwrap(), before);
                assert_eq!(restored.state().resolved_rules_journal, journal);
                assert_eq!(
                    zone_of(&restored, card),
                    if round_trip {
                        Zone::Hand
                    } else {
                        Zone::Graveyard
                    }
                );
                assert_eq!(discard_count(&restored, P0), 0);
                assert!(restored
                    .state()
                    .stack
                    .iter()
                    .all(|entry| entry.source_id != observer));
                assert!(restored.state().deferred_triggers.is_empty());
                assert_eq!(
                    restored
                        .state()
                        .pending_replacement
                        .as_ref()
                        .unwrap()
                        .discard_provenance
                        .as_deref()
                        .unwrap()
                        .original_hand,
                    Some(original)
                );
            }
        }
    }
}

#[test]
fn r12_library_current_pin_restores_and_both_original_answers_complete_once() {
    use engine::types::game_state::{GameState, PersistedGameState};
    for index in [0, 1] {
        let (runner, card, observer) = original_library_choice();
        for ingress in 0..3 {
            let state: GameState = if ingress == 0 {
                serde_json::from_value(serde_json::to_value(runner.state()).unwrap()).unwrap()
            } else {
                let saved = if ingress == 1 {
                    PersistedGameState::Raw(Box::new(runner.state().clone()))
                } else {
                    PersistedGameState::capture(runner.state().clone())
                };
                serde_json::from_value::<PersistedGameState>(serde_json::to_value(saved).unwrap())
                    .unwrap()
                    .into_game_state()
                    .unwrap()
            };
            let mut restored = GameRunner::from_state(state);
            let result = restored
                .act(GameAction::ChooseReplacement { index })
                .unwrap();
            assert_eq!(
                discarded_with_sources(&result.events),
                vec![(P0, card, None)]
            );
            assert_eq!(discard_count(&restored, P0), 1);
            assert_eq!(
                zone_of(&restored, card),
                if index == 0 {
                    Zone::Library
                } else {
                    Zone::Graveyard
                }
            );
            assert_eq!(restored.state().stack.len(), 1);
            assert_eq!(restored.state().stack[0].source_id, observer);
            assert!(is_priority(&restored));
            assert!(restored.state().pending_discard_batch.is_none());
            assert!(restored.state().resolution_stack.is_empty());
        }
    }
}

#[test]
fn r13_unknown_original_pin_rejects_load_live_simulation_and_zero_count_atomically() {
    use engine::game::engine::apply_for_simulation;
    use engine::types::game_state::{GameState, PendingCostMoveResume, PersistedGameState};
    use engine::types::identifiers::{ObjectIncarnationRef, LEGACY_INCARNATION};
    let (runner, card, _) = original_library_choice();
    for boxed in [false, true] {
        for malformed in 0..3 {
            let mut bad = runner.state().clone();
            let pending = bad.pending_replacement.as_mut().unwrap();
            let provenance = pending.discard_provenance.as_deref_mut().unwrap();
            provenance.original_hand = match malformed {
                0 => None,
                1 => Some(ObjectIncarnationRef::of(card, LEGACY_INCARNATION)),
                _ => Some(ObjectIncarnationRef::of(ObjectId(card.0 + 1000), 0)),
            };
            if boxed {
                bad.pending_cost_move_resume = Some(PendingCostMoveResume::ReplacementMayCost {
                    source_id: card,
                    current: card,
                    remaining: vec![],
                    paid_count: 0,
                    outer_replacement: bad.pending_replacement.take().map(Box::new),
                });
            }
            for ingress in 0..3 {
                let error = if ingress == 0 {
                    serde_json::from_value::<GameState>(serde_json::to_value(&bad).unwrap())
                        .unwrap_err()
                        .to_string()
                } else {
                    let saved = if ingress == 1 {
                        PersistedGameState::Raw(Box::new(bad.clone()))
                    } else {
                        PersistedGameState::capture(bad.clone())
                    };
                    serde_json::from_value::<PersistedGameState>(
                        serde_json::to_value(saved).unwrap(),
                    )
                    .unwrap_err()
                    .to_string()
                };
                assert!(error.contains("original-hand authority"), "{error}");
            }
            for simulation in [false, true] {
                for action in [
                    GameAction::ChooseReplacement { index: 0 },
                    GameAction::Debug(DebugAction::CreateCard {
                        card_name: "unused zero-count admission".into(),
                        owner: P0,
                        zone: Zone::Hand,
                        count: 0,
                        attach_to: None,
                        run_etb: false,
                        nonlegendary: false,
                        creation_kind: Default::default(),
                    }),
                ] {
                    let mut live = bad.clone();
                    let before = serde_json::to_value(&live).unwrap();
                    let journal = live.resolved_rules_journal.clone();
                    let result = if simulation {
                        apply_for_simulation(&mut live, P0, action)
                    } else {
                        apply(&mut live, P0, action)
                    };
                    assert!(result
                        .unwrap_err()
                        .to_string()
                        .contains("original-hand authority"));
                    assert_eq!(serde_json::to_value(&live).unwrap(), before);
                    assert_eq!(live.resolved_rules_journal, journal);
                }
            }
        }
    }
}

#[test]
fn r14_carrierless_no_frame_choice_never_recaptures_current_hand_authority() {
    use engine::types::game_state::GameState;
    let (runner, card, _) = original_library_choice();
    let mut encoded = serde_json::to_value(runner.state()).unwrap();
    encoded["pending_replacement"]
        .as_object_mut()
        .unwrap()
        .remove("discard_provenance");
    let mut state: GameState =
        serde_json::from_value(encoded).expect("absent whole optional carrier stays absent");
    assert!(state
        .pending_replacement
        .as_ref()
        .unwrap()
        .discard_provenance
        .is_none());
    let before = serde_json::to_value(&state).unwrap();
    let journal = state.resolved_rules_journal.clone();
    assert!(
        apply(&mut state, P0, GameAction::ChooseReplacement { index: 0 })
            .unwrap_err()
            .to_string()
            .contains("original-hand authority")
    );
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
    assert_eq!(state.resolved_rules_journal, journal);
    assert_eq!(state.objects[&card].zone, Zone::Hand);
}

fn order_discard_observers(runner: &mut GameRunner) {
    if let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for {
        let order = (0..triggers.len()).collect();
        runner
            .act(GameAction::OrderTriggers { order })
            .expect("public observer order");
    }
    assert!(
        is_priority(runner),
        "observers reach Priority: {:?}",
        runner.state().waiting_for
    );
}

fn assert_discard_receipt(
    runner: &GameRunner,
    events: &[GameEvent],
    card: ObjectId,
    original: engine::types::identifiers::ObjectIncarnationRef,
    destination: Zone,
    revealed: bool,
    grimoire: ObjectId,
) {
    use engine::types::game_state::StackEntryKind;
    let event = events
        .iter()
        .find(|event| matches!(event, GameEvent::Discarded { object_id, .. } if *object_id == card))
        .expect("actual completed discard");
    let GameEvent::Discarded {
        player_id,
        object_id,
        source_id,
        observation: Some(observation),
    } = event
    else {
        panic!("current publisher requires observation")
    };
    assert_eq!((*player_id, *object_id, *source_id), (P0, card, None));
    assert_eq!(observation.subject.identity, original);
    assert_eq!(observation.subject.zone, Zone::Hand);
    assert_eq!(observation.subject.owner, P0);
    assert_eq!(observation.delivered_to, Some(destination));
    assert!(observation.pending_reveal.is_none());
    assert_eq!(observation.revealed_by.is_some(), revealed);
    let retained = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == grimoire)
        .expect("actual Grimoire observer");
    assert!(
        matches!(&retained.kind, StackEntryKind::TriggeredAbility { trigger_event: Some(retained), .. } if retained == event)
    );
}

fn resolve_waste_not_and_decline_grimoire(runner: &mut GameRunner) {
    for _ in 0..8 {
        if runner.state().stack.is_empty() && is_priority(runner) {
            return;
        }
        match runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("decline Grimoire draw");
            }
            WaitingFor::Priority { .. } => runner.resolve_top(),
            _ => panic!(
                "unexpected observer prompt: {:?}",
                runner.state().waiting_for
            ),
        }
    }
    panic!("discard observers did not finish");
}

/// CR 701.9c: hidden unrevealed characteristics, including their negations, are undefined.
#[test]
fn r15_library_hidden_discard_masks_all_waste_not_branches() {
    use engine::game::game_object::ObjectIncarnationRefExt;
    use engine::types::identifiers::ObjectIncarnationRef;
    use engine::types::mana::ManaType;
    let db = load_db().expect("printed hidden-discard regressions require the committed fixture");
    for (name, branch) in [("Grizzly Bears", 0), ("Swamp", 1), ("Opt", 2)] {
        for index in [0, 1] {
            let mut ids = None;
            let mut runner = table(db, |s, db| {
                s.add_real_card(P0, "Library of Leng", Zone::Battlefield, db);
                let waste = s.add_real_card(P1, "Waste Not", Zone::Battlefield, db);
                let grimoire = s.add_real_card(P1, "Geth's Grimoire", Zone::Battlefield, db);
                let card = s.add_real_card(P0, name, Zone::Hand, db);
                ids = Some((waste, grimoire, card));
            });
            let (waste, grimoire, card) = ids.unwrap();
            let original = ObjectIncarnationRef::from_object(&runner.state().objects[&card]);
            let before_count = discard_count(&runner, P0);
            let before_hand = runner.state().players[1].hand.len();
            let before_black = runner.state().players[1]
                .mana_pool
                .count_color(ManaType::Black);
            let before_zombies = count_named_on_battlefield(&runner, P1, "Zombie");
            let parked = runner
                .act(discard(card))
                .expect("public discard reaches Library");
            assert!(
                matches!(
                    runner.state().waiting_for,
                    WaitingFor::ReplacementChoice { player: P0, .. }
                ),
                "name={name} index={index}"
            );
            assert_eq!(zone_of(&runner, card), Zone::Hand);
            assert!(discarded_with_sources(&parked.events).is_empty());
            assert_eq!(discard_count(&runner, P0), before_count);
            let result = runner
                .act(GameAction::ChooseReplacement { index })
                .expect("public Library answer");
            order_discard_observers(&mut runner);
            let destination = if index == 0 {
                Zone::Library
            } else {
                Zone::Graveyard
            };
            assert_eq!(zone_of(&runner, card), destination);
            assert_eq!(
                discarded_with_sources(&result.events),
                vec![(P0, card, None)]
            );
            assert_eq!(discard_count(&runner, P0), before_count + 1);
            assert_discard_receipt(
                &runner,
                &result.events,
                card,
                original,
                destination,
                false,
                grimoire,
            );
            assert_eq!(
                runner
                    .state()
                    .stack
                    .iter()
                    .filter(|entry| entry.source_id == grimoire)
                    .count(),
                1,
                "name={name} index={index}"
            );
            assert_eq!(
                runner
                    .state()
                    .stack
                    .iter()
                    .filter(|entry| entry.source_id == waste)
                    .count(),
                usize::from(index == 1),
                "name={name} index={index}"
            );
            resolve_waste_not_and_decline_grimoire(&mut runner);
            assert_eq!(
                count_named_on_battlefield(&runner, P1, "Zombie") - before_zombies,
                usize::from(index == 1 && branch == 0),
                "name={name} index={index}"
            );
            assert_eq!(
                runner.state().players[1]
                    .mana_pool
                    .count_color(ManaType::Black)
                    - before_black,
                if index == 1 && branch == 1 { 2 } else { 0 },
                "name={name} index={index}"
            );
            assert_eq!(
                runner.state().players[1].hand.len() - before_hand,
                usize::from(index == 1 && branch == 2),
                "name={name} index={index}"
            );
            assert!(runner.state().pending_discard_batch.is_none());
            assert!(runner.state().resolution_stack.is_empty());
            assert!(is_priority(&runner));
        }
    }
}

/// CR 701.9c + CR 614.6: the selected printed mandatory reveal precedes the original move.
#[test]
fn r16_printed_shuffle_back_reveal_preserves_typed_discard_observers() {
    use engine::game::game_object::ObjectIncarnationRefExt;
    use engine::types::events::PlayerActionKind;
    use engine::types::identifiers::ObjectIncarnationRef;
    let db = load_db().expect("printed shuffle-back regressions require committed cards");
    for name in ["Nexus of Fate", "Darksteel Colossus"] {
        for rip_count in [0, 2] {
            for select_reveal in [false, true] {
                if rip_count == 0 && !select_reveal {
                    continue;
                }
                let mut ids = None;
                let mut runner = table(db, |s, db| {
                    let waste = s.add_real_card(P1, "Waste Not", Zone::Battlefield, db);
                    let grimoire = s.add_real_card(P1, "Geth's Grimoire", Zone::Battlefield, db);
                    let rips = (0..rip_count)
                        .map(|_| s.add_real_card(P1, "Rest in Peace", Zone::Battlefield, db))
                        .collect::<Vec<_>>();
                    let card = s.add_real_card(P0, name, Zone::Hand, db);
                    ids = Some((waste, grimoire, card, rips));
                });
                let (waste, grimoire, card, rips) = ids.unwrap();
                let original = ObjectIncarnationRef::from_object(&runner.state().objects[&card]);
                let mut events = runner
                    .act(discard(card))
                    .expect("printed discard producer")
                    .events;
                if rip_count != 0 {
                    assert!(matches!(
                        runner.state().waiting_for,
                        WaitingFor::ReplacementChoice { player: P0, .. }
                    ));
                    let pending = runner
                        .state()
                        .pending_replacement
                        .as_ref()
                        .expect("actual inner Moved ordering");
                    assert!(
                        pending.candidates.len() >= 3,
                        "two printed RIP candidates and printed shuffle-back: {name}"
                    );
                    assert!(pending
                        .candidates
                        .iter()
                        .any(|candidate| candidate.source == card));
                    for rip in &rips {
                        assert!(pending
                            .candidates
                            .iter()
                            .any(|candidate| candidate.source == *rip));
                    }
                    assert_eq!(zone_of(&runner, card), Zone::Hand);
                    assert!(!events.iter().any(|event| matches!(
                        event,
                        GameEvent::CardsRevealed { .. } | GameEvent::Discarded { .. }
                    )));
                    let chosen = if select_reveal { card } else { rips[0] };
                    let index = pending
                        .candidates
                        .iter()
                        .position(|candidate| candidate.source == chosen)
                        .unwrap();
                    events.extend(
                        runner
                            .act(GameAction::ChooseReplacement { index })
                            .expect("select exact printed replacement")
                            .events,
                    );
                }
                order_discard_observers(&mut runner);
                let destination = if select_reveal {
                    Zone::Library
                } else {
                    Zone::Exile
                };
                assert_eq!(zone_of(&runner, card), destination);
                assert_eq!(discarded_with_sources(&events), vec![(P0, card, None)]);
                assert_eq!(discard_count(&runner, P0), 1);
                assert_eq!(
                    runner
                        .state()
                        .stack
                        .iter()
                        .filter(|entry| entry.source_id == waste)
                        .count(),
                    1,
                    "typed printed observer: {name} rip={rip_count} reveal={select_reveal}"
                );
                assert_discard_receipt(
                    &runner,
                    &events,
                    card,
                    original,
                    destination,
                    select_reveal,
                    grimoire,
                );
                let reveals = events.iter().enumerate().filter(|(_, event)| matches!(event, GameEvent::CardsRevealed { card_ids, .. } if card_ids == &[card])).map(|(index, _)| index).collect::<Vec<_>>();
                assert_eq!(reveals.len(), usize::from(select_reveal));
                if select_reveal {
                    let moved = events.iter().position(|event| matches!(event, GameEvent::ZoneChanged { object_id, from: Some(Zone::Hand), record, .. } if *object_id == card && record.trigger_source_context().is_some_and(|context| context.identity.reference == original))).expect("original hand departure");
                    assert!(reveals[0] < moved);
                    assert!(
                        events.iter().any(|event| matches!(
                            event,
                            GameEvent::PlayerPerformedAction {
                                player_id: P0,
                                action: PlayerActionKind::ShuffledLibrary,
                                ..
                            }
                        )),
                        "selected Shuffle suffix runs"
                    );
                }
                assert!(runner.state().pending_replacement.is_none());
                assert!(runner.state().pending_discard_batch.is_none());
                assert!(runner.state().resolution_stack.is_empty());
                assert!(is_priority(&runner));
            }
        }
    }
}

#[test]
fn discard_observation_current_legacy_and_malformed_wire_shapes() {
    use engine::types::game_state::{GameState, PersistedGameState};
    for ingress in 0..3 {
        let (runner, card, observer) = original_library_choice();
        let original = runner
            .state()
            .pending_replacement
            .as_ref()
            .unwrap()
            .discard_provenance
            .as_deref()
            .unwrap()
            .observation
            .as_deref()
            .unwrap()
            .clone();
        for missing in [false, true] {
            let mut input = runner.state().clone();
            if missing {
                input
                    .pending_replacement
                    .as_mut()
                    .unwrap()
                    .discard_provenance
                    .as_deref_mut()
                    .unwrap()
                    .observation = None;
            }
            let state: GameState = if ingress == 0 {
                serde_json::from_value(serde_json::to_value(input).unwrap()).unwrap()
            } else {
                let saved = if ingress == 1 {
                    PersistedGameState::Raw(Box::new(input))
                } else {
                    PersistedGameState::capture(input)
                };
                serde_json::from_value::<PersistedGameState>(serde_json::to_value(saved).unwrap())
                    .unwrap()
                    .into_game_state()
                    .unwrap()
            };
            assert_eq!(
                state
                    .pending_replacement
                    .as_ref()
                    .unwrap()
                    .discard_provenance
                    .as_deref()
                    .unwrap()
                    .observation
                    .as_deref(),
                Some(&original),
                "ingress={ingress} missing={missing}"
            );
            let mut restored = GameRunner::from_state(state);
            let result = restored
                .act(GameAction::ChooseReplacement { index: 0 })
                .unwrap();
            assert_discard_receipt(
                &restored,
                &result.events,
                card,
                original.subject.identity,
                Zone::Library,
                false,
                observer,
            );
            let settled = serde_json::to_value(restored.state()).unwrap();
            let retained: GameState = serde_json::from_value(settled)
                .expect("completed hidden observation restores without live fallback");
            assert_eq!(retained.stack, restored.state().stack);
            let mut malformed_event = retained.clone();
            if let engine::types::game_state::StackEntryKind::TriggeredAbility {
                trigger_event:
                    Some(GameEvent::Discarded {
                        observation: Some(observation),
                        ..
                    }),
                ..
            } = &mut malformed_event.stack.front_mut().unwrap().kind
            {
                observation.delivered_to = None;
            } else {
                panic!("retained completed event reaches validation")
            }
            assert!(serde_json::from_value::<GameState>(
                serde_json::to_value(malformed_event).unwrap()
            )
            .unwrap_err()
            .to_string()
            .contains("completed discard lacks observation authority"));
            let mut legacy = retained.clone();
            for entry in legacy.stack.iter_mut() {
                if let engine::types::game_state::StackEntryKind::TriggeredAbility {
                    trigger_event: Some(GameEvent::Discarded { observation, .. }),
                    ..
                } = &mut entry.kind
                {
                    *observation = None;
                }
            }
            let unknown: GameState = serde_json::from_value(serde_json::to_value(legacy).unwrap())
                .expect("whole legacy event absence remains explicit unknown");
            assert!(unknown.stack.iter().any(|entry| matches!(
                &entry.kind,
                engine::types::game_state::StackEntryKind::TriggeredAbility {
                    trigger_event: Some(GameEvent::Discarded {
                        observation: None,
                        ..
                    }),
                    ..
                }
            )));
        }
        for malformed in 0..4 {
            let mut input = runner.state().clone();
            let pending = input.pending_replacement.as_mut().unwrap();
            let observation = pending
                .discard_provenance
                .as_deref_mut()
                .unwrap()
                .observation
                .as_deref_mut()
                .unwrap();
            match malformed {
                0 => observation.subject.zone = Zone::Graveyard,
                1 => observation.subject.owner = P1,
                2 => observation.subject.identity.object_id = observer,
                3 => {
                    observation.delivered_to = Some(Zone::Library);
                }
                _ => unreachable!(),
            }
            let json = serde_json::to_value(&input).unwrap();
            let error = serde_json::from_value::<GameState>(json).unwrap_err();
            assert!(
                error.to_string().contains("observation authority"),
                "{error}"
            );
            let before = serde_json::to_value(&input).unwrap();
            assert!(apply(&mut input, P0, GameAction::ChooseReplacement { index: 0 }).is_err());
            assert_eq!(serde_json::to_value(&input).unwrap(), before);
            let saved = if ingress == 1 {
                PersistedGameState::Raw(Box::new(input))
            } else {
                PersistedGameState::capture(input)
            };
            assert!(serde_json::from_value::<PersistedGameState>(
                serde_json::to_value(saved).unwrap()
            )
            .and_then(|saved| saved.into_game_state().map_err(serde::de::Error::custom))
            .is_err());
        }
        let mut ambiguous = runner.state().clone();
        ambiguous
            .pending_replacement
            .as_mut()
            .unwrap()
            .discard_provenance
            .as_deref_mut()
            .unwrap()
            .observation = None;
        ambiguous.pending_replacement.as_mut().unwrap().depth = 1;
        assert!(
            serde_json::from_value::<GameState>(serde_json::to_value(ambiguous).unwrap())
                .unwrap_err()
                .to_string()
                .contains("observation authority")
        );
    }
}
