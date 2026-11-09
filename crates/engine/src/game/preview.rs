//! Non-mutating action preview (issue #5468).
//!
//! Diffs the PUBLIC deltas an action would produce — life-total changes and
//! public-zone object transitions — so an embedder can drive hover-preview UX
//! ("this kills that", "you take 4") without committing the action. The caller
//! runs the action on a throwaway clone (never rendered) and passes the
//! before/after snapshots here.
//!
//! Hidden-information safety has three parts. Transitions are computed from the
//! AUTHORITATIVE before/after states and surfaced only when at least one
//! endpoint is a public zone, so a fully-hidden hand↔library move (a draw) is
//! never surfaced. Every displayed field (`name`, `controller`) comes from the
//! actor's `filter_state_for_viewer` projection at the observed endpoint, and a
//! transition whose observed endpoint the projection does not hold is dropped.
//! CR 400.2 draws the public/hidden zone line.

use serde::{Deserialize, Serialize};

use crate::ai_support::legal_actions_for_viewer;
use crate::game::engine::{
    action_rejection_for_engine_error, apply_for_simulation, explicit_debug_permission_rejection,
    preflight_debug_action_with_rejection, EngineError,
};
use crate::game::game_object::GameObject;
use crate::game::visibility::filter_action_rejection_for_viewer;
use crate::types::action_rejection::ActionRejection;
use crate::types::actions::GameAction;
use crate::types::events::GameEvent;
use crate::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// A single object's public zone transition (e.g. `Battlefield → Graveyard` is a
/// death; `Stack → Graveyard` is a resolved/countered spell).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZoneChange {
    pub object_id: ObjectId,
    pub name: String,
    pub controller: PlayerId,
    pub from: Zone,
    pub to: Zone,
}

/// A player's life-total change (`delta` is signed: negative = life lost).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifeDelta {
    pub player: PlayerId,
    pub delta: i32,
}

/// An object that ceased to exist during the action (`from` = the public zone it
/// left). CR 111.7 / CR 704.5d: a token that leaves the battlefield/stack is
/// removed by state-based actions, so it is simply gone from the after-snapshot —
/// the headline "this kills that" case when the victim is a token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CeasedObject {
    pub object_id: ObjectId,
    pub name: String,
    pub controller: PlayerId,
    pub from: Zone,
}

/// The public, viewer-safe result of previewing an action.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PreviewDiff {
    pub life_deltas: Vec<LifeDelta>,
    pub zone_changes: Vec<ZoneChange>,
    /// Objects newly present in a public zone (e.g. a created token).
    pub created: Vec<ObjectId>,
    /// Objects that were in a public zone before and ceased to exist (e.g. a
    /// token that died), so they appear in neither `after.objects` nor
    /// `zone_changes`.
    pub ceased: Vec<CeasedObject>,
}

/// One side of a preview diff. `authoritative` decides which objects exist and
/// in which zone; `view` (the actor's `filter_state_for_viewer` projection of
/// `authoritative`) decides every field the actor is shown. Pairing them by type
/// keeps a projection from standing in for its authoritative state.
#[derive(Debug, Clone, Copy)]
pub struct PreviewSnapshot<'a> {
    pub authoritative: &'a GameState,
    pub view: &'a GameState,
}

/// CR 400.2: a zone whose contents every player can see. Hand and Library are
/// hidden; every other zone is public.
fn zone_is_public(zone: Zone) -> bool {
    !matches!(zone, Zone::Hand | Zone::Library)
}

/// CR 400.2: the endpoint of a transition the actor observed — the
/// destination when it is public, otherwise the origin — read from the
/// actor's projection. `None` when neither endpoint is public (elided) or the
/// projection does not hold the object there (fail closed: nothing about the
/// object is read from authoritative state).
fn observed_endpoint<'v>(
    id: ObjectId,
    before_view: &'v GameState,
    from: Zone,
    after_view: &'v GameState,
    to: Zone,
) -> Option<&'v GameObject> {
    [(after_view, to), (before_view, from)]
        .into_iter()
        .filter(|(_, zone)| zone_is_public(*zone))
        .find_map(|(view, _)| view.objects.get(&id))
}

/// Diff two snapshots (before/after an action) into the PUBLIC deltas a viewer
/// could legitimately observe.
///
/// Each [`PreviewSnapshot`] pairs an authoritative state with the actor's
/// `filter_state_for_viewer` projection of it. The authoritative states decide
/// existence and zones; the views decide every displayed field, so any identity
/// the viewer may not see (opponents' hands, libraries, face-down cards) is
/// already redacted in `ZoneChange.name`. A transition is only surfaced when at
/// least ONE endpoint is a public zone: a departure from a public zone is public
/// information even if the destination is hidden (a bounce), and an arrival into
/// a public zone is public even if the origin was hidden (a cast from hand, a
/// mill). Only fully-hidden hand↔library moves (draws, hand shuffles) are
/// elided, since neither endpoint is observable. Output ordering is
/// deterministic (sorted by id).
pub fn compute_preview_diff(
    before: PreviewSnapshot<'_>,
    after: PreviewSnapshot<'_>,
) -> PreviewDiff {
    let mut life_deltas = Vec::new();
    for a in &after.view.players {
        if let Some(b) = before.view.players.iter().find(|p| p.id == a.id) {
            if a.life != b.life {
                life_deltas.push(LifeDelta {
                    player: a.id,
                    delta: a.life - b.life,
                });
            }
        }
    }
    life_deltas.sort_by_key(|l| l.player.0);

    let mut zone_changes = Vec::new();
    let mut created = Vec::new();
    for (id, a) in &after.authoritative.objects {
        match before.authoritative.objects.get(id) {
            // CR 400.2: observable iff one endpoint is a public zone; displayed
            // fields come from the actor's projection at that endpoint.
            // CR 108.4 + CR 108.4a: a card in a hand or library has no controller
            // (its owner stands in); the controller shown is the permanent's or
            // spell's at the observed public endpoint (CR 110.2 / CR 112.2).
            Some(b) if b.zone != a.zone => {
                if let Some(shown) = observed_endpoint(*id, before.view, b.zone, after.view, a.zone)
                {
                    zone_changes.push(ZoneChange {
                        object_id: *id,
                        name: shown.name.clone(),
                        controller: shown.controller,
                        from: b.zone,
                        to: a.zone,
                    });
                }
            }
            Some(_) => {}
            // Fail closed: the projection must hold what it reports.
            None if zone_is_public(a.zone) && after.view.objects.contains_key(id) => {
                created.push(*id)
            }
            None => {}
        }
    }

    // Objects present before and gone after ceased to exist (CR 111.7 /
    // CR 704.5d — a token leaving battlefield/stack via SBAs). Only report those
    // that were in a public zone, so a token dying on the battlefield ("this
    // kills that") surfaces while a hidden-zone disappearance stays hidden.
    let mut ceased = Vec::new();
    for (id, b) in &before.authoritative.objects {
        if after.authoritative.objects.contains_key(id) || !zone_is_public(b.zone) {
            continue;
        }
        if let Some(shown) = before.view.objects.get(id) {
            ceased.push(CeasedObject {
                object_id: *id,
                name: shown.name.clone(),
                controller: shown.controller,
                from: b.zone,
            });
        }
    }

    zone_changes.sort_by_key(|z| z.object_id.0);
    created.sort_by_key(|id| id.0);
    ceased.sort_by_key(|c| c.object_id.0);

    PreviewDiff {
        life_deltas,
        zone_changes,
        created,
        ceased,
    }
}

/// Simulates an ordinary action and returns only its viewer-safe public diff.
/// The original state is never mutated.
pub fn preview_action(
    state: &GameState,
    actor: PlayerId,
    action: &GameAction,
) -> Result<PreviewDiff, EngineError> {
    let before_view = crate::game::visibility::filter_state_for_viewer(state, actor);
    let mut projected = state.clone();
    apply_for_simulation(&mut projected, actor, action.clone())?;
    let after_view = crate::game::visibility::filter_state_for_viewer(&projected, actor);
    Ok(compute_preview_diff(
        PreviewSnapshot {
            authoritative: state,
            view: &before_view,
        },
        PreviewSnapshot {
            authoritative: &projected,
            view: &after_view,
        },
    ))
}

/// Viewer-safe form of [`preview_action`] with stable rejection metadata.
pub fn preview_action_with_rejection(
    state: &GameState,
    actor: PlayerId,
    action: &GameAction,
) -> Result<PreviewDiff, ActionRejection> {
    let related_object_ids = action.related_object_ids();
    if matches!(action, GameAction::Debug(_)) {
        if let Some(rejection) =
            explicit_debug_permission_rejection(state, actor, related_object_ids.clone())
        {
            return Err(rejection);
        }
    }
    preview_action(state, actor, action).map_err(|error| {
        filter_action_rejection_for_viewer(
            state,
            actor,
            &action_rejection_for_engine_error(&error, related_object_ids),
        )
    })
}

/// Returns the mana sources the automatic payment path uses for `action`,
/// without changing the live game state.
///
/// This accepts only an exact, engine-offered automatic `CastSpell` action.
/// Simulating that action keeps the preview on the same payment path as a real
/// cast; this layer deliberately does not reconstruct casts or invoke the
/// payment resolver itself. If an X spell pauses to announce its value, the
/// drag preview continues with X = 0: drag has no announced X value, and zero
/// is the rules-defined default for an unchosen X outside the stack.
/// Other player choices still return an empty list because they determine the
/// final payment path.
pub fn preview_auto_payment_sources(
    state: &GameState,
    actor: PlayerId,
    action: &GameAction,
) -> Result<Vec<ObjectId>, EngineError> {
    let GameAction::CastSpell {
        object_id,
        payment_mode: CastPaymentMode::Auto,
        ..
    } = action
    else {
        return Ok(Vec::new());
    };

    if !legal_actions_for_viewer(state, actor)
        .0
        .iter()
        .any(|candidate| candidate == action)
    {
        return Ok(Vec::new());
    }

    let mut sim = state.clone();
    // Auto-pass is a convenience setting for future priority windows, not part
    // of casting payment. A preview must stop at the cast transaction rather
    // than continuing through unrelated automatic passes.
    sim.auto_pass.clear();
    let result = apply_for_simulation(&mut sim, actor, action.clone())?;
    // CR 107.3g: Before this card is cast, its unannounced {X} is 0. Preserve
    // that drag-time view by advancing the throwaway cast simulation at X=0.
    let mut events = result.events;
    if matches!(&result.waiting_for, WaitingFor::ChooseXValue { min: 0, .. }) {
        let x_result = apply_for_simulation(&mut sim, actor, GameAction::ChooseX { value: 0 })?;
        events.extend(x_result.events);
    }
    Ok(mana_source_ids_before_spell_cast(&events, *object_id))
}

/// Viewer-safe form of [`preview_auto_payment_sources`] with stable rejection
/// metadata. It preserves the legacy preview's no-mutation behavior.
pub fn preview_auto_payment_sources_with_rejection(
    state: &GameState,
    actor: PlayerId,
    action: &GameAction,
) -> Result<Vec<ObjectId>, ActionRejection> {
    if let GameAction::Debug(debug_action) = action {
        preflight_debug_action_with_rejection(state, actor, debug_action)?;
    }

    let related_object_ids = action.related_object_ids();
    preview_auto_payment_sources(state, actor, action).map_err(|error| {
        filter_action_rejection_for_viewer(
            state,
            actor,
            &action_rejection_for_engine_error(&error, related_object_ids),
        )
    })
}

fn mana_source_ids_before_spell_cast(events: &[GameEvent], object_id: ObjectId) -> Vec<ObjectId> {
    let Some(spell_cast_index) = events.iter().position(|event| {
        matches!(event, GameEvent::SpellCast { object_id: cast_object_id, .. } if *cast_object_id == object_id)
    }) else {
        return Vec::new();
    };

    let mut sources = events
        .iter()
        .take(spell_cast_index)
        .filter_map(|event| match event {
            GameEvent::ManaAdded { source_id, .. } => Some(*source_id),
            _ => None,
        })
        .collect::<Vec<_>>();
    sources.sort_by_key(|source_id| source_id.0);
    sources.dedup();
    sources
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::GameObject;
    use crate::types::identifiers::CardId;

    fn obj(id: u64, owner: u8, name: &str, zone: Zone) -> GameObject {
        GameObject::new(
            ObjectId(id),
            CardId(id),
            PlayerId(owner),
            name.to_string(),
            zone,
        )
    }

    /// An authorized viewer: the view is the authoritative state itself.
    fn unredacted(state: &GameState) -> PreviewSnapshot<'_> {
        PreviewSnapshot {
            authoritative: state,
            view: state,
        }
    }

    /// Test-only simulation of a projection that gives concealed hand and
    /// library occurrences no key: every Hand/Library object is removed from
    /// `objects` and from each player's `hand`/`library`. It makes no claim
    /// about the real projection and must not be mistaken for it.
    fn concealing_view(state: &GameState) -> GameState {
        let mut view = state.clone();
        let concealed: Vec<ObjectId> = view
            .objects
            .iter()
            .filter(|(_, o)| !zone_is_public(o.zone))
            .map(|(id, _)| *id)
            .collect();
        for id in &concealed {
            view.objects.remove(id);
        }
        for player in &mut view.players {
            player.hand.retain(|id| !concealed.contains(id));
            player.library.retain(|id| !concealed.contains(id));
        }
        view
    }

    fn insert(state: &mut GameState, id: u64, name: &str, zone: Zone) {
        state.objects.insert(ObjectId(id), obj(id, 0, name, zone));
    }

    fn zone_change(id: u64, name: &str, controller: u8, from: Zone, to: Zone) -> ZoneChange {
        ZoneChange {
            object_id: ObjectId(id),
            name: name.to_string(),
            controller: PlayerId(controller),
            from,
            to,
        }
    }

    #[test]
    fn reports_life_delta_and_public_zone_change() {
        let mut before = GameState::new_two_player(1);
        let mut after = before.clone();

        // A creature on the battlefield in `before` dies (→ graveyard) in `after`,
        // and player 1 loses 4 life.
        before
            .objects
            .insert(ObjectId(10), obj(10, 0, "Bear", Zone::Battlefield));
        after
            .objects
            .insert(ObjectId(10), obj(10, 0, "Bear", Zone::Graveyard));
        after.players[1].life = before.players[1].life - 4;

        let diff = compute_preview_diff(unredacted(&before), unredacted(&after));
        assert_eq!(
            diff.zone_changes,
            vec![ZoneChange {
                object_id: ObjectId(10),
                name: "Bear".to_string(),
                controller: PlayerId(0),
                from: Zone::Battlefield,
                to: Zone::Graveyard,
            }]
        );
        assert_eq!(
            diff.life_deltas,
            vec![LifeDelta {
                player: PlayerId(1),
                delta: -4,
            }]
        );
        assert!(diff.created.is_empty());
    }

    #[test]
    fn reports_created_token_in_public_zone() {
        let before = GameState::new_two_player(1);
        let mut after = before.clone();
        after
            .objects
            .insert(ObjectId(20), obj(20, 0, "Soldier", Zone::Battlefield));

        let diff = compute_preview_diff(unredacted(&before), unredacted(&after));
        assert_eq!(diff.created, vec![ObjectId(20)]);
        assert!(diff.zone_changes.is_empty());
    }

    #[test]
    fn reports_public_endpoint_transitions_but_elides_pure_hidden_moves() {
        // Widened predicate (#5491 review): a transition with at least one PUBLIC
        // endpoint is observable and reported; only fully-hidden hand↔library
        // moves (draws) are elided. A creation into a hidden zone is not reported.
        let mut before = GameState::new_two_player(1);
        let mut after = before.clone();

        // Cast: hand → stack (arrival into a public zone) — REPORTED.
        before
            .objects
            .insert(ObjectId(31), obj(31, 0, "Bolt", Zone::Hand));
        after
            .objects
            .insert(ObjectId(31), obj(31, 0, "Bolt", Zone::Stack));
        // Bounce: battlefield → hand (departure from a public zone) — REPORTED.
        before
            .objects
            .insert(ObjectId(32), obj(32, 0, "Bear", Zone::Battlefield));
        after
            .objects
            .insert(ObjectId(32), obj(32, 0, "Bear", Zone::Hand));
        // Draw: library → hand (both hidden) — ELIDED.
        before
            .objects
            .insert(ObjectId(33), obj(33, 0, "Secret", Zone::Library));
        after
            .objects
            .insert(ObjectId(33), obj(33, 0, "Secret", Zone::Hand));
        // Milled into the library (hidden arrival) — not `created`.
        after
            .objects
            .insert(ObjectId(34), obj(34, 0, "Milled", Zone::Library));

        let diff = compute_preview_diff(unredacted(&before), unredacted(&after));
        let changed: Vec<u64> = diff.zone_changes.iter().map(|z| z.object_id.0).collect();
        assert_eq!(
            changed,
            vec![31, 32],
            "cast + bounce reported, draw elided: {:?}",
            diff.zone_changes
        );
        assert!(
            diff.created.is_empty(),
            "hidden-zone creation is not reported"
        );
    }

    #[test]
    fn reports_ceased_token_but_not_hidden_disappearance() {
        // #5491 review blocker: a token that dies (removed from `after.objects` by
        // state-based actions, CR 111.7 / 704.5d) must still surface — the headline
        // "this kills that" case. A disappearance from a hidden zone must not.
        let mut before = GameState::new_two_player(1);
        let after = before.clone();
        // These exist only in `before` → they ceased to exist during the action.
        before
            .objects
            .insert(ObjectId(40), obj(40, 0, "Zombie Token", Zone::Battlefield));
        before
            .objects
            .insert(ObjectId(41), obj(41, 0, "Milled Token", Zone::Library));

        let diff = compute_preview_diff(unredacted(&before), unredacted(&after));
        assert_eq!(
            diff.ceased,
            vec![CeasedObject {
                object_id: ObjectId(40),
                name: "Zombie Token".to_string(),
                controller: PlayerId(0),
                from: Zone::Battlefield,
            }],
            "public-zone token death surfaces; hidden-zone disappearance stays hidden",
        );
    }

    // Building-block unit for the preview consumer: transitions come from the
    // authoritative endpoints even when the view holds no key for a concealed
    // hand/library occurrence.
    #[test]
    fn transitions_survive_a_view_without_hidden_endpoint_keys() {
        let mut before = GameState::new_two_player(1);
        let mut after = before.clone();
        // Bounce: battlefield -> hand (CR 400.2: departure from a public zone).
        insert(&mut before, 32, "Bear", Zone::Battlefield);
        insert(&mut after, 32, "Bear", Zone::Hand);
        // Discard: hand -> graveyard.
        insert(&mut before, 35, "Swamp", Zone::Hand);
        insert(&mut after, 35, "Swamp", Zone::Graveyard);
        // Mill: library -> graveyard.
        insert(&mut before, 36, "Plains", Zone::Library);
        insert(&mut after, 36, "Plains", Zone::Graveyard);
        // Draw: library -> hand (both hidden) is elided.
        insert(&mut before, 33, "Secret", Zone::Library);
        insert(&mut after, 33, "Secret", Zone::Hand);
        // Token dies (CR 111.7 / CR 704.5d).
        insert(&mut before, 40, "Zombie Token", Zone::Battlefield);
        // Token created.
        insert(&mut after, 20, "Soldier", Zone::Battlefield);

        let before_view = concealing_view(&before);
        let after_view = concealing_view(&after);
        let diff = compute_preview_diff(
            PreviewSnapshot {
                authoritative: &before,
                view: &before_view,
            },
            PreviewSnapshot {
                authoritative: &after,
                view: &after_view,
            },
        );

        assert_eq!(
            diff.zone_changes,
            vec![
                zone_change(32, "Bear", 0, Zone::Battlefield, Zone::Hand),
                zone_change(35, "Swamp", 0, Zone::Hand, Zone::Graveyard),
                zone_change(36, "Plains", 0, Zone::Library, Zone::Graveyard),
            ]
        );
        assert_eq!(diff.created, vec![ObjectId(20)]);
        assert_eq!(
            diff.ceased
                .iter()
                .map(|c| c.object_id.0)
                .collect::<Vec<_>>(),
            vec![40]
        );
    }

    #[test]
    fn displayed_fields_come_from_the_view_not_the_authoritative_state() {
        let mut before = GameState::new_two_player(1);
        let mut after = before.clone();
        // Library -> exile arrival whose identity the actor may not see.
        insert(&mut before, 50, "Secret Name", Zone::Library);
        insert(&mut after, 50, "Secret Name", Zone::Exile);
        // Bounce whose authoritative name differs from the shown one.
        insert(&mut before, 51, "Real", Zone::Battlefield);
        insert(&mut after, 51, "Real", Zone::Hand);

        let mut before_view = before.clone();
        let mut after_view = after.clone();
        after_view.objects.get_mut(&ObjectId(50)).unwrap().name =
            crate::game::visibility::HIDDEN_CARD_NAME.to_string();
        before_view.objects.get_mut(&ObjectId(51)).unwrap().name = "Shown".to_string();

        let diff = compute_preview_diff(
            PreviewSnapshot {
                authoritative: &before,
                view: &before_view,
            },
            PreviewSnapshot {
                authoritative: &after,
                view: &after_view,
            },
        );
        assert_eq!(
            diff.zone_changes,
            vec![
                zone_change(
                    50,
                    crate::game::visibility::HIDDEN_CARD_NAME,
                    0,
                    Zone::Library,
                    Zone::Exile
                ),
                zone_change(51, "Shown", 0, Zone::Battlefield, Zone::Hand),
            ]
        );
    }

    #[test]
    fn a_public_endpoint_missing_from_the_view_is_dropped() {
        let mut before = GameState::new_two_player(1);
        let mut after = before.clone();
        // 60: bounce whose before-view key is missing. 61: created token whose
        // after-view key is missing.
        insert(&mut before, 60, "Gone", Zone::Battlefield);
        insert(&mut after, 60, "Gone", Zone::Hand);
        insert(&mut after, 61, "Phantom", Zone::Battlefield);
        // Positive control: present in both views, proves the loop ran.
        insert(&mut before, 62, "Control", Zone::Battlefield);
        insert(&mut after, 62, "Control", Zone::Graveyard);

        let mut before_view = before.clone();
        let mut after_view = after.clone();
        before_view.objects.remove(&ObjectId(60));
        after_view.objects.remove(&ObjectId(60));
        after_view.objects.remove(&ObjectId(61));

        let diff = compute_preview_diff(
            PreviewSnapshot {
                authoritative: &before,
                view: &before_view,
            },
            PreviewSnapshot {
                authoritative: &after,
                view: &after_view,
            },
        );
        assert_eq!(
            diff.zone_changes,
            vec![zone_change(
                62,
                "Control",
                0,
                Zone::Battlefield,
                Zone::Graveyard
            )]
        );
        assert!(diff.created.is_empty());
        assert!(diff.ceased.is_empty());
    }

    #[test]
    fn view_only_keys_are_never_reported() {
        let before = GameState::new_two_player(1);
        let mut after = before.clone();
        insert(&mut after, 20, "Soldier", Zone::Battlefield);

        // Handle-shaped ids present only in the after-view.
        let handle_hand = ObjectId((1 << 52) + 1);
        let handle_battlefield = ObjectId((1 << 52) + 2);
        let mut after_view = after.clone();
        after_view.objects.insert(
            handle_hand,
            GameObject::new(
                handle_hand,
                crate::types::identifiers::CardId(1),
                PlayerId(1),
                "Handle".to_string(),
                Zone::Hand,
            ),
        );
        after_view.objects.insert(
            handle_battlefield,
            GameObject::new(
                handle_battlefield,
                crate::types::identifiers::CardId(2),
                PlayerId(1),
                "Handle".to_string(),
                Zone::Battlefield,
            ),
        );

        let diff = compute_preview_diff(
            unredacted(&before),
            PreviewSnapshot {
                authoritative: &after,
                view: &after_view,
            },
        );
        assert_eq!(diff.created, vec![ObjectId(20)]);
        assert!(diff.zone_changes.is_empty());
        assert!(diff.ceased.is_empty());
    }

    #[test]
    fn both_public_moves_keep_the_after_first_reading_and_hidden_moves_stay_elided() {
        let mut before = GameState::new_two_player(1);
        let mut after = before.clone();
        insert(&mut before, 10, "Bear", Zone::Battlefield);
        insert(&mut after, 10, "Bear", Zone::Graveyard);
        insert(&mut before, 33, "Secret", Zone::Library);
        insert(&mut after, 33, "Secret", Zone::Hand);

        let diff = compute_preview_diff(unredacted(&before), unredacted(&after));
        assert_eq!(
            diff.zone_changes,
            vec![zone_change(
                10,
                "Bear",
                0,
                Zone::Battlefield,
                Zone::Graveyard
            )]
        );
    }
}
