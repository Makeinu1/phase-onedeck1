//! Preview reports the public transitions of an opponent's cards from the
//! authoritative before/after states, with every displayed field read from the
//! actor's projection at the observed public endpoint (CR 400.2, CR 108.4).
//!
//! The "not ceased" / "not created" clauses below discriminate only once the
//! projection stops keying concealed hand and library occurrences by their real
//! object ids: these clauses guard the projection change that re-keys concealed
//! hand and library occurrences.
//!
//! Card texts are the verbatim Oracle texts of the committed card fixture.

use engine::game::preview::{compute_preview_diff, preview_action, PreviewSnapshot, ZoneChange};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const UNSUMMON: &str = "Return target creature to its owner's hand.";
const THOUGHT_SCOUR: &str = "Target player mills two cards.\nDraw a card.";
const FUNERAL_CHARM: &str = "Choose one —\n\
• Target player discards a card.\n\
• Target creature gets +2/-1 until end of turn.\n\
• Target creature gains swampwalk until end of turn. (It can't be blocked as long as defending player controls a Swamp.)";

fn change(id: ObjectId, name: &str, controller: PlayerId, from: Zone, to: Zone) -> ZoneChange {
    ZoneChange {
        object_id: id,
        name: name.to_string(),
        controller,
        from,
        to,
    }
}

fn sorted(mut changes: Vec<ZoneChange>) -> Vec<ZoneChange> {
    changes.sort_by_key(|c| c.object_id.0);
    changes
}

/// P0 (active) passes so P1 holds priority. The caller then casts for P1 and
/// calls [`pass_to_p0`].
fn pass_to_p1(runner: &mut GameRunner) {
    runner.act(GameAction::PassPriority).expect("P0 passes");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
}

/// CR 117.3c: P1 keeps priority after casting; P1 passing hands it to P0 with
/// the spell still on the stack.
fn pass_to_p0(runner: &mut GameRunner, spell: ObjectId) {
    runner.act(GameAction::PassPriority).expect("P1 passes");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert!(
        runner.state().stack.iter().any(|e| e.source_id == spell),
        "the spell is still on the stack"
    );
}

/// Performs the previewed action for real and asserts the resolution completed
/// (CR 117.3b: the active player receives priority; the stack is empty).
fn resolve_for_real(runner: &mut GameRunner) {
    runner.act(GameAction::PassPriority).expect("P0 passes");
    assert!(runner.state().stack.is_empty(), "stack resolved");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

struct Bounce {
    runner: GameRunner,
    bears: ObjectId,
    unsummon: ObjectId,
}

/// P1 owns Grizzly Bears (controlled by `controller`) and casts Unsummon on
/// them; priority is with P0 and Unsummon is on the stack.
fn bounce_board(controller: PlayerId) -> Bounce {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario
        .add_creature(P1, "Grizzly Bears", 2, 2)
        .controlled_by(controller)
        .id();
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P1, "Unsummon", true, UNSUMMON)
        .id();
    let mut runner = scenario.build();
    pass_to_p1(&mut runner);
    runner.cast(unsummon).target_objects(&[bears]).commit();
    pass_to_p0(&mut runner, unsummon);
    Bounce {
        runner,
        bears,
        unsummon,
    }
}

/// CR 400.2 + CR 400.3: Unsummon moves the Bears from the battlefield (public)
/// to their owner's hand (hidden). The transition is observed at the public
/// endpoint, so it carries the Bears' name, not the hidden hand object's.
#[test]
fn opponent_bounce_previews_as_battlefield_to_hand_named_from_the_battlefield() {
    let Bounce {
        mut runner,
        bears,
        unsummon,
    } = bounce_board(P1);

    let diff = preview_action(runner.state(), P0, &GameAction::PassPriority).expect("preview");
    assert_eq!(
        diff.zone_changes,
        sorted(vec![
            change(bears, "Grizzly Bears", P1, Zone::Battlefield, Zone::Hand),
            change(unsummon, "Unsummon", P1, Zone::Stack, Zone::Graveyard),
        ])
    );
    assert!(diff.created.is_empty());
    assert!(diff.ceased.is_empty());

    // Live state untouched by the preview.
    assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
    assert!(runner.state().stack.iter().any(|e| e.source_id == unsummon));

    // Reach guard: the real action puts the Bears in P1's hand.
    resolve_for_real(&mut runner);
    assert!(runner.state().players[P1.0 as usize]
        .hand
        .iter()
        .any(|id| *id == bears));
}

/// CR 108.4 + CR 110.2: a card in a hand has no controller; the controller
/// shown for a public-to-hidden move is the permanent's at the public endpoint.
/// P1 owns the Bears, P0 (the actor) controls them.
#[test]
fn bounce_of_a_stolen_creature_shows_its_battlefield_controller() {
    let Bounce {
        mut runner, bears, ..
    } = bounce_board(P0);

    let diff = preview_action(runner.state(), P0, &GameAction::PassPriority).expect("preview");
    assert!(
        diff.zone_changes.contains(&change(
            bears,
            "Grizzly Bears",
            P0,
            Zone::Battlefield,
            Zone::Hand
        )),
        "{:?}",
        diff.zone_changes
    );

    // Reach guard: CR 400.3, the card goes to its OWNER's hand, not P0's.
    resolve_for_real(&mut runner);
    assert!(runner.state().players[P1.0 as usize]
        .hand
        .iter()
        .any(|id| *id == bears));
    assert!(!runner.state().players[P0.0 as usize]
        .hand
        .iter()
        .any(|id| *id == bears));
}

/// The preview equals the diff of the real action for the actor, and the owner
/// sees the same public transition.
#[test]
fn preview_matches_the_real_action_for_actor_and_owner() {
    let Bounce {
        mut runner,
        bears,
        unsummon,
    } = bounce_board(P1);

    let previewed = preview_action(runner.state(), P0, &GameAction::PassPriority).expect("preview");
    let before = runner.state().clone();
    runner.act(GameAction::PassPriority).expect("P0 passes");
    let after = runner.state().clone();

    let diff_for = |viewer: PlayerId| {
        let before_view = filter_state_for_viewer(&before, viewer);
        let after_view = filter_state_for_viewer(&after, viewer);
        compute_preview_diff(
            PreviewSnapshot {
                authoritative: &before,
                view: &before_view,
            },
            PreviewSnapshot {
                authoritative: &after,
                view: &after_view,
            },
        )
    };

    let expected = sorted(vec![
        change(bears, "Grizzly Bears", P1, Zone::Battlefield, Zone::Hand),
        change(unsummon, "Unsummon", P1, Zone::Stack, Zone::Graveyard),
    ]);
    assert_eq!(previewed.zone_changes, expected);
    assert_eq!(diff_for(P0).zone_changes, expected);
    assert!(diff_for(P1).zone_changes.contains(&change(
        bears,
        "Grizzly Bears",
        P1,
        Zone::Battlefield,
        Zone::Hand
    )));
}

/// CR 701.17a + CR 121.1: Thought Scour mills two cards (library to graveyard,
/// public arrival) and then draws one (library to hand, both hidden: elided).
#[test]
fn opponent_mill_previews_library_to_graveyard_and_elides_the_draw() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let plains = scenario.add_card_to_library_top(P1, "Plains");
    let forest = scenario.add_card_to_library_top(P1, "Forest");
    let mountain = scenario.add_card_to_library_top(P1, "Mountain");
    let scour = scenario
        .add_spell_to_hand_from_oracle(P1, "Thought Scour", true, THOUGHT_SCOUR)
        .id();
    let mut runner = scenario.build();
    pass_to_p1(&mut runner);
    runner.cast(scour).target_player(P1).commit();
    pass_to_p0(&mut runner, scour);

    let diff = preview_action(runner.state(), P0, &GameAction::PassPriority).expect("preview");
    assert_eq!(
        diff.zone_changes,
        sorted(vec![
            change(mountain, "Mountain", P1, Zone::Library, Zone::Graveyard),
            change(forest, "Forest", P1, Zone::Library, Zone::Graveyard),
            change(scour, "Thought Scour", P1, Zone::Stack, Zone::Graveyard),
        ])
    );
    assert!(diff.created.is_empty());
    assert!(diff.ceased.is_empty());

    // Reach guard: the draw really happened, so its absence above is elision.
    resolve_for_real(&mut runner);
    let p1 = &runner.state().players[P1.0 as usize];
    assert!(p1.hand.iter().any(|id| *id == plains));
    assert!(p1.graveyard.iter().any(|id| *id == mountain));
    assert!(p1.graveyard.iter().any(|id| *id == forest));
}

/// CR 701.9a: Funeral Charm's first mode makes the target player discard a
/// card (hand to graveyard, public arrival). With one card in hand the discard
/// needs no choice.
#[test]
fn opponent_discard_previews_hand_to_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let swamp = scenario.add_card_to_hand(P1, "Swamp");
    let charm = scenario
        .add_spell_to_hand_from_oracle(P1, "Funeral Charm", true, FUNERAL_CHARM)
        .id();
    let mut runner = scenario.build();
    pass_to_p1(&mut runner);
    runner.cast(charm).modes(&[0]).target_player(P1).commit();
    pass_to_p0(&mut runner, charm);

    let diff = preview_action(runner.state(), P0, &GameAction::PassPriority).expect("preview");
    assert_eq!(
        diff.zone_changes,
        sorted(vec![
            change(swamp, "Swamp", P1, Zone::Hand, Zone::Graveyard),
            change(charm, "Funeral Charm", P1, Zone::Stack, Zone::Graveyard),
        ])
    );
    assert!(diff.created.is_empty());

    // Reach guard: the real action discards the Swamp without pausing.
    resolve_for_real(&mut runner);
    assert!(runner.state().players[P1.0 as usize]
        .graveyard
        .iter()
        .any(|id| *id == swamp));
}
