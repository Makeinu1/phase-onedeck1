//! THROWAWAY PROBES for the A3a plan (Sandbox `DebugAction::Discard`).
//! Fork-only; never upstreamed. Each test reproduces the planned arm
//! (`discard_caused_by_effect_with_source(.., None, ..)` then the A2 settle on
//! `Complete`, or the replacement prompt on `NeedsReplacementChoice`) and asserts
//! the value the plan expects. A failure message carries the observed values.

use crate::game::effects::discard::{discard_caused_by_effect_with_source, DiscardOutcome};
use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
use crate::game::scenario_db::GameScenarioDbExt;
use crate::test_support::shared_card_db;
use crate::types::actions::GameAction;
use crate::types::events::GameEvent;
use crate::types::game_state::{CastOfferKind, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::phase::Phase;
use crate::types::zones::Zone;

/// The planned arm, minus admission checks. Returns this arm's events.
fn planned_arm(runner: &mut GameRunner, object_id: ObjectId) -> (Vec<GameEvent>, &'static str) {
    let state = runner.state_mut();
    let owner = state.objects[&object_id].owner;
    let mut events = Vec::new();
    let event_start = events.len();
    let path =
        match discard_caused_by_effect_with_source(state, object_id, owner, None, &mut events) {
            DiscardOutcome::Complete => {
                let default_wf = state.waiting_for.clone();
                let wf = crate::game::engine_priority::run_post_action_pipeline_from(
                    state,
                    &mut events,
                    event_start,
                    &default_wf,
                    false,
                    false,
                )
                .expect("settle");
                state.waiting_for = wf;
                "Complete"
            }
            DiscardOutcome::NeedsReplacementChoice(chooser) => {
                state.waiting_for =
                    crate::game::replacement::replacement_choice_waiting_for(chooser, state);
                "NeedsReplacementChoice"
            }
        };
    (events, path)
}

fn discarded_count(events: &[GameEvent], id: ObjectId) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, GameEvent::Discarded { object_id, .. } if *object_id == id))
        .count()
}

fn board() -> GameScenario {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s
}

/// P2: Complete path puts an opponent's "whenever an opponent discards" trigger
/// on the stack once and leaves Priority.
#[test]
fn probe_p2_complete_path_settles_opponent_trigger() {
    let db = shared_card_db();
    let mut s = board();
    s.add_real_card(P1, "Geth's Grimoire", Zone::Battlefield, db);
    let card = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let mut runner = s.build();
    let (events, path) = planned_arm(&mut runner, card);
    let st = runner.state();
    let obs = format!(
        "path={path} zone={:?} discarded={} stack_len={} wf={:?}",
        st.objects[&card].zone,
        discarded_count(&events, card),
        st.stack.len(),
        st.waiting_for
    );
    assert_eq!(path, "Complete", "P2 {obs}");
    assert_eq!(discarded_count(&events, card), 1, "P2 reach {obs}");
    assert_eq!(st.objects[&card].zone, Zone::Graveyard, "P2 {obs}");
    assert_eq!(st.stack.len(), 1, "P2 trigger once {obs}");
    assert!(
        matches!(st.waiting_for, WaitingFor::Priority { .. }),
        "P2 {obs}"
    );
}

/// P1: Library of Leng parks a choice; no trigger before the answer; after the
/// answer the reducer settles Geth's Grimoire exactly once.
#[test]
fn probe_p1_parked_choice_settles_once_after_answer() {
    let db = shared_card_db();
    let mut s = board();
    s.add_real_card(P0, "Library of Leng", Zone::Battlefield, db);
    s.add_real_card(P1, "Geth's Grimoire", Zone::Battlefield, db);
    let card = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let mut runner = s.build();
    let (events, path) = planned_arm(&mut runner, card);
    let pre = format!(
        "path={path} discarded={} stack_len={} wf={:?}",
        discarded_count(&events, card),
        runner.state().stack.len(),
        runner.state().waiting_for
    );
    assert_eq!(path, "NeedsReplacementChoice", "P1 park {pre}");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ReplacementChoice { player, .. } if player == P0),
        "P1 prompt {pre}"
    );
    assert_eq!(runner.state().stack.len(), 0, "P1 no early trigger {pre}");

    let mut results = Vec::new();
    for index in [0usize, 1usize] {
        let mut probe = GameRunner::from_state(runner.state().clone());
        let out = probe.act(GameAction::ChooseReplacement { index });
        let st = probe.state();
        let (ok, n) = match &out {
            Ok(r) => (true, discarded_count(&r.events, card)),
            Err(_) => (false, 0),
        };
        results.push(format!(
            "idx={index} ok={ok} answer_discarded={n} zone={:?} stack_len={} wf={:?}",
            st.objects.get(&card).map(|o| o.zone),
            st.stack.len(),
            st.waiting_for
        ));
        if ok {
            assert_eq!(
                st.stack.len(),
                1,
                "P1 trigger once after answer: {results:?}"
            );
            assert_eq!(n, 1, "P1 one Discarded in answer: {results:?}");
        }
    }
    assert!(
        results.iter().filter(|r| r.contains("ok=true")).count() == 2,
        "P1 both options answerable: {results:?}"
    );
}

/// P3: Rest in Peace redirects to exile and the discard still counts.
#[test]
fn probe_p3_rest_in_peace_redirect_is_still_a_discard() {
    let db = shared_card_db();
    let mut s = board();
    s.add_real_card(P1, "Rest in Peace", Zone::Battlefield, db);
    s.add_real_card(P1, "Geth's Grimoire", Zone::Battlefield, db);
    let card = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let mut runner = s.build();
    let (events, path) = planned_arm(&mut runner, card);
    let st = runner.state();
    let obs = format!(
        "path={path} zone={:?} discarded={} stack_len={} wf={:?}",
        st.objects[&card].zone,
        discarded_count(&events, card),
        st.stack.len(),
        st.waiting_for
    );
    assert_eq!(st.objects[&card].zone, Zone::Exile, "P3 {obs}");
    assert_eq!(discarded_count(&events, card), 1, "P3 {obs}");
    assert_eq!(st.stack.len(), 1, "P3 trigger {obs}");
}

/// P4: Madness card is exiled, its trigger reaches the stack through the
/// settle, and resolving it offers the Madness cast.
#[test]
fn probe_p4_madness_trigger_offers_cast() {
    let db = shared_card_db();
    let mut s = board();
    let card = s.add_real_card(P0, "Violent Eruption", Zone::Hand, db);
    let mut runner = s.build();
    let (events, path) = planned_arm(&mut runner, card);
    let after = format!(
        "path={path} zone={:?} discarded={} stack_len={} wf={:?}",
        runner.state().objects[&card].zone,
        discarded_count(&events, card),
        runner.state().stack.len(),
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&card].zone,
        Zone::Exile,
        "P4 {after}"
    );
    assert_eq!(runner.state().stack.len(), 1, "P4 madness trigger {after}");
    let mut trail = Vec::new();
    for _ in 0..4 {
        if matches!(runner.state().waiting_for, WaitingFor::CastOffer { .. }) {
            break;
        }
        let r = runner.act(GameAction::PassPriority);
        trail.push(format!(
            "{:?} -> {:?}",
            r.is_ok(),
            runner.state().waiting_for
        ));
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CastOffer { player, kind: CastOfferKind::Madness { object_id, .. } }
                if player == P0 && object_id == card
        ),
        "P4 cast offer {after} trail={trail:?}"
    );
}

/// P5: with no source, Loxodon Smiter's opponent-cause replacement does not apply.
#[test]
fn probe_p5_no_source_smiter_goes_to_graveyard() {
    let db = shared_card_db();
    let mut s = board();
    let card = s.add_real_card(P0, "Loxodon Smiter", Zone::Hand, db);
    let mut runner = s.build();
    let (events, path) = planned_arm(&mut runner, card);
    let st = runner.state();
    let obs = format!(
        "path={path} zone={:?} discarded={} wf={:?}",
        st.objects[&card].zone,
        discarded_count(&events, card),
        st.waiting_for
    );
    assert_eq!(st.objects[&card].zone, Zone::Graveyard, "P5 {obs}");
    assert_eq!(discarded_count(&events, card), 1, "P5 {obs}");
}

/// P6: the authority reports `Complete` for a card that is not in a hand
/// (justifies the arm's own in-hand precheck, plan D2).
#[test]
fn probe_p6_authority_silently_completes_off_hand_card() {
    let db = shared_card_db();
    let mut s = board();
    let card = s.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let mut runner = s.build();
    let (events, path) = planned_arm(&mut runner, card);
    let st = runner.state();
    let obs = format!(
        "path={path} zone={:?} discarded={}",
        st.objects[&card].zone,
        discarded_count(&events, card)
    );
    assert_eq!(path, "Complete", "P6 {obs}");
    assert_eq!(st.objects[&card].zone, Zone::Battlefield, "P6 {obs}");
    assert_eq!(discarded_count(&events, card), 0, "P6 {obs}");
}
