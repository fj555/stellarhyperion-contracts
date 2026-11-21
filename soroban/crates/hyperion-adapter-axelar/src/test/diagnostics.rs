//! What `check_token_mapping` says before anybody activates a mapping.
//!
//! Registration is a one-time admin action per asset and the failure worth catching is a second
//! registration that quietly rewrites the first. These tests hold the diagnostic to three
//! promises: it names every field that disagrees rather than the first, it reads the same facts
//! the transfer paths read, and it changes nothing while doing so.

use super::setup::{World, STRANGE_TOKEN_ID};
use crate::rail::TokenManagerType;
use crate::storage;
use hyperion_core::{HyperionError, TokenMappingConflict};
use soroban_sdk::{testutils::Events, BytesN, Vec};

/// How many events this adapter itself published, which is what an indexer watching it sees.
fn adapter_event_count(w: &World) -> usize {
    w.env
        .events()
        .all()
        .filter_by_contract(&w.adapter_id)
        .events()
        .len()
}

#[test]
fn a_mapping_that_matches_everything_is_reported_as_compatible() {
    let w = World::unlinked();

    // Nothing mapped yet, ITS knows the id, the asset reports the decimals the caller expected.
    let report = w
        .adapter()
        .check_token_mapping(&w.token_id, &w.burn_id(), &Some(7), &Some(4));

    assert!(report.conflicts.is_empty(), "{:?}", report.conflicts);
    assert_eq!(report.mapped_token_id, None);
    assert_eq!(report.mapped_token, None);
    assert_eq!(report.rail_token, Some(w.token_id.clone()));
    assert_eq!(report.rail_manager, Some(w.manager_id.clone()));
    assert_eq!(report.rail_manager_type, Some(4));
    assert_eq!(report.onchain_decimals, Some(7));
    assert_eq!(report.expected_decimals, Some(7));
    assert!(report.is_compatible());
}

#[test]
fn a_second_token_id_for_one_asset_names_the_id_already_stored() {
    let w = World::unlinked();
    w.adapter().map_token(&w.token_id, &w.burn_id());

    // Pointing an already mapped asset at a different token id would change where it travels on
    // the way out.
    let report = w
        .adapter()
        .check_token_mapping(&w.token_id, &w.lock_id(), &None, &None);

    assert_eq!(report.mapped_token_id, Some(w.burn_id()));
    assert_eq!(
        report.conflicts.first(),
        Some(TokenMappingConflict::TokenId(w.burn_id(), w.lock_id()))
    );
}

#[test]
fn a_second_asset_for_one_token_id_names_the_asset_already_stored() {
    let w = World::unlinked();
    w.adapter().map_token(&w.token_id, &w.burn_id());

    // The other direction, and the one that matters for money arriving: the reverse mapping is
    // what decides which local asset an inbound delivery is paid in.
    let report = w
        .adapter()
        .check_token_mapping(&w.lock_asset, &w.burn_id(), &None, &None);

    assert_eq!(report.mapped_token, Some(w.token_id.clone()));
    assert_eq!(
        report.conflicts.first(),
        Some(TokenMappingConflict::LocalToken(
            w.token_id.clone(),
            w.lock_asset.clone()
        ))
    );
}

#[test]
fn a_token_id_axelar_has_never_heard_of_is_reported_as_unregistered() {
    let w = World::unlinked();
    let strange = BytesN::from_array(&w.env, &STRANGE_TOKEN_ID);

    let report = w
        .adapter()
        .check_token_mapping(&w.token_id, &strange, &None, &None);

    assert_eq!(
        report.conflicts,
        Vec::from_array(&w.env, [TokenMappingConflict::UnregisteredTokenId])
    );
    assert_eq!(report.rail_token, None);
    assert_eq!(report.rail_manager, None);
}

#[test]
fn a_token_id_axelar_backs_with_another_asset_is_reported_against_that_asset() {
    let w = World::unlinked();
    // ITS says the burn id is backed by `token_id`, and this asks about `lock_asset`. Both halves
    // are real; the pairing is the mistake.
    let report = w
        .adapter()
        .check_token_mapping(&w.lock_asset, &w.burn_id(), &None, &None);

    assert_eq!(
        report.conflicts,
        Vec::from_array(
            &w.env,
            [TokenMappingConflict::RailTokenMismatch(
                w.token_id.clone(),
                w.lock_asset.clone()
            )]
        )
    );
}

#[test]
fn decimals_that_disagree_are_reported_with_both_numbers() {
    let w = World::unlinked();

    let report = w
        .adapter()
        .check_token_mapping(&w.token_id, &w.burn_id(), &Some(6), &None);

    assert_eq!(
        report.conflicts,
        Vec::from_array(&w.env, [TokenMappingConflict::Decimals(7, 6)])
    );
}

#[test]
fn a_manager_kind_that_disagrees_is_reported_with_both_numbers() {
    let w = World::unlinked();

    // Locking rather than burning changes what the outbound path has to authorise, so an operator
    // who believes the wrong one is worth telling before they activate.
    let report = w
        .adapter()
        .check_token_mapping(&w.token_id, &w.burn_id(), &None, &Some(2));

    assert_eq!(
        report.conflicts,
        Vec::from_array(&w.env, [TokenMappingConflict::ManagerType(4, 2)])
    );
}

#[test]
fn every_disagreement_is_reported_at_once_in_a_fixed_order() {
    let w = World::new();

    // One call, four separate mistakes: the asset is mapped to another id, the id belongs to
    // another asset, ITS backs the id with that other asset, and the decimals are wrong. A
    // diagnostic that stopped at the first would need four round trips to say the same thing.
    let report = w
        .adapter()
        .check_token_mapping(&w.lock_asset, &w.burn_id(), &Some(6), &None);

    assert_eq!(
        report.conflicts,
        Vec::from_array(
            &w.env,
            [
                TokenMappingConflict::TokenId(w.lock_id(), w.burn_id()),
                TokenMappingConflict::LocalToken(w.token_id.clone(), w.lock_asset.clone()),
                TokenMappingConflict::RailTokenMismatch(w.token_id.clone(), w.lock_asset.clone()),
                TokenMappingConflict::Decimals(7, 6),
            ]
        )
    );
}

#[test]
fn a_repeat_of_a_live_mapping_is_accepted_and_announces_nothing() {
    let w = World::new();

    // A deploy script that runs twice should be boring. This one already ran.
    w.adapter().map_token(&w.token_id, &w.burn_id());

    // No second `TokenMapped`: the mapping already read back as proposed, so nothing changed.
    assert_eq!(adapter_event_count(&w), 0);
    w.env.as_contract(&w.adapter_id, || {
        assert_eq!(storage::token_id(&w.env, &w.token_id), Some(w.burn_id()));
    });
}

#[test]
fn rewriting_a_live_mapping_is_refused_rather_than_applied() {
    let w = World::new();

    assert_eq!(
        w.adapter().try_map_token(&w.lock_asset, &w.burn_id()),
        Err(Ok(HyperionError::AlreadyConfigured))
    );
    // And the mapping that was there is still there, in both directions.
    w.env.as_contract(&w.adapter_id, || {
        assert_eq!(storage::token_id(&w.env, &w.token_id), Some(w.burn_id()));
        assert_eq!(storage::token_id(&w.env, &w.lock_asset), Some(w.lock_id()));
        assert_eq!(
            storage::token_of(&w.env, &w.burn_id()),
            Some(w.token_id.clone())
        );
    });
}

#[test]
fn a_mapping_axelar_cannot_confirm_is_refused_as_unmapped() {
    let w = World::unlinked();
    let strange = BytesN::from_array(&w.env, &STRANGE_TOKEN_ID);

    assert_eq!(
        w.adapter().try_map_token(&w.token_id, &strange),
        Err(Ok(HyperionError::TokenNotMapped))
    );
}

#[test]
fn the_diagnostic_writes_nothing_and_announces_nothing() {
    let w = World::new();

    let report = w
        .adapter()
        .check_token_mapping(&w.token_id, &w.burn_id(), &Some(7), &Some(4));

    assert!(report.conflicts.is_empty());
    // A dry run that emitted `TokenMapped` would have indexers recording a change that never
    // happened.
    assert_eq!(adapter_event_count(&w), 0);
    w.env.as_contract(&w.adapter_id, || {
        assert_eq!(storage::token_id(&w.env, &w.token_id), Some(w.burn_id()));
        assert_eq!(
            storage::token_of(&w.env, &w.burn_id()),
            Some(w.token_id.clone())
        );
    });
}

#[test]
fn the_diagnostic_needs_no_authorisation() {
    let w = World::new();
    w.env.set_auths(&[]);
    // Anybody may ask the question. Only the consequences are gated, and those live in
    // `map_token`.
    let report = w
        .adapter()
        .check_token_mapping(&w.token_id, &w.burn_id(), &None, &None);
    assert!(report.conflicts.is_empty());
}

#[test]
fn an_asset_with_unreadable_metadata_is_reported_rather_than_guessed_at() {
    let w = World::unlinked();
    // A contract that exists and answers nothing this build understands. Read the same way
    // `check_token_behavior` reads it on the router, so the two diagnostics agree.
    let mute = w.env.register(crate::test::doubles::MuteToken, ());
    w.its().register(
        &w.burn_id(),
        &mute,
        &w.manager_id,
        &TokenManagerType::MintBurn,
    );

    let report = w
        .adapter()
        .check_token_mapping(&mute, &w.burn_id(), &Some(7), &None);

    assert_eq!(
        report.conflicts,
        Vec::from_array(&w.env, [TokenMappingConflict::UnreadableMetadata])
    );
    assert_eq!(report.onchain_decimals, None);
}

#[test]
#[should_panic(expected = "Unauthorized")]
fn a_stranger_cannot_rewrite_a_mapping() {
    let w = World::new();
    w.env.set_auths(&[]);
    // Without a signature there is no admin, so the rewrite never reaches the conflict check.
    w.adapter().map_token(&w.token_id, &w.burn_id());
}
