//! Claim retention and real executable-upgrade regression fixtures.
extern crate std;
use super::doubles::{evm_destination, message_id, Delivery};
use super::setup::{World, EVM_DECIMALS, HUNDRED};
use crate::{
    storage, AdminAction, ClaimHistory, DataKey, DebitBreakdown, Destination, OutboundRequest,
};
use hyperion_core::{AddressKind, HyperionError, RouteKind};
use soroban_sdk::{
    testutils::{Events, Ledger},
    BytesN, IntoVal, Map, Symbol, Val,
};

fn parked(w: &World, nonce: u64) -> u64 {
    let token = w.with_failable_token();
    w.failable(&token).mint(&w.rail_id, &HUNDRED);
    w.failable(&token).set_blocked(&w.recipient, &true);
    w.rail().deliver(&Delivery {
        route: RouteKind::Cctp,
        token,
        amount: HUNDRED,
        recipient: w.recipient.clone(),
        recipient_kind: AddressKind::Account,
        source_chain: w.ethereum(),
        source_nonce: nonce,
    })
}
fn settled(w: &World, nonce: u64) -> u64 {
    let id = parked(w, nonce);
    let claim = w.router().get_claim(&id);
    w.failable(&claim.token).set_blocked(&w.recipient, &false);
    w.router().settle_claim(&w.user, &id);
    id
}

#[test]
fn shared_storage_access_preserves_existing_keys_and_checked_liabilities() {
    let w = World::new();
    w.env.as_contract(&w.router_id, || {
        let key = DataKey::ClaimLiability(w.token_id.clone());
        w.env.storage().instance().set(&key, &i128::MAX);
        assert_eq!(
            storage::increase_claim_liability(&w.env, &w.token_id, 1),
            Err(HyperionError::InvalidAmount)
        );
        assert_eq!(storage::claim_liability(&w.env, &w.token_id), i128::MAX);
        storage::decrease_claim_liability(&w.env, &w.token_id, i128::MAX).unwrap();
        assert_eq!(
            storage::decrease_claim_liability(&w.env, &w.token_id, 1),
            Err(HyperionError::InvalidAmount)
        );
        assert_eq!(storage::claim_liability(&w.env, &w.token_id), 0);
        storage::increase_claim_liability(&w.env, &w.token_id, 7).unwrap();
        assert_eq!(w.env.storage().instance().get::<_, i128>(&key), Some(7));

        w.env.storage().instance().set(&DataKey::OutNonce, &17u64);
        w.env.storage().instance().set(&DataKey::ClaimSeq, &27u64);
        w.env.storage().instance().set(&DataKey::QueueSeq, &37u64);
        assert_eq!(storage::next_out_nonce(&w.env), 18);
        assert_eq!(storage::next_claim_id(&w.env), 28);
        assert_eq!(storage::next_queue_id(&w.env), 38);
        assert_eq!(storage::out_nonce(&w.env), 18);
        assert_eq!(storage::claim_count(&w.env), 28);
        assert_eq!(storage::queue_count(&w.env), 38);
    });
}

#[test]
fn batch_settlement_records_true_age_and_archived_claim_cannot_repay_after_restoration() {
    let w = World::new();
    let id = parked(&w, 105);
    let original = w.router().get_claim(&id);
    w.env
        .ledger()
        .set_timestamp(w.env.ledger().timestamp() + storage::MIN_CLAIM_RETENTION_SECONDS * 2);
    w.failable(&original.token)
        .set_blocked(&w.recipient, &false);
    let results = w
        .router()
        .settle_claims(&w.user, &soroban_sdk::vec![&w.env, id]);
    assert_eq!(results.get(0).unwrap().status, 0);
    w.router().archive_settled_claims(&0, &32);
    assert_eq!(w.router().claim_history(&id), ClaimHistory::Live(id));
    w.env
        .ledger()
        .set_timestamp(w.env.ledger().timestamp() + storage::MIN_CLAIM_RETENTION_SECONDS);
    w.router().archive_settled_claims(&0, &32);
    assert_eq!(w.router().claim_history(&id), ClaimHistory::Archived(id));
    w.env.as_contract(&w.router_id, || {
        storage::set_claim(&w.env, &original);
    });
    let results = w
        .router()
        .settle_claims(&w.user, &soroban_sdk::vec![&w.env, id]);
    assert_eq!(results.get(0).unwrap().status, 2);
    assert_eq!(w.failable(&original.token).balance(&w.recipient), HUNDRED);
    assert_eq!(w.router().claim_liability(&original.token).outstanding, 0);
}

#[test]
fn cleanup_preserves_liability_replay_exact_history_and_restart_cursor() {
    let w = World::new();
    let first = settled(&w, 101);
    let pending = parked(&w, 102);
    let legacy = settled(&w, 103);
    w.env.as_contract(&w.router_id, || {
        w.env
            .storage()
            .persistent()
            .remove(&DataKey::ClaimSettledAt(legacy))
    });
    let liability = w
        .router()
        .claim_liability(&w.router().get_claim(&pending).token);
    assert_eq!(w.router().archive_settled_claims(&0, &1), 1);
    assert!(matches!(
        w.router().claim_history(&first),
        ClaimHistory::Live(_)
    ));
    w.env
        .ledger()
        .set_timestamp(w.env.ledger().timestamp() + storage::MIN_CLAIM_RETENTION_SECONDS);
    assert_eq!(w.router().archive_settled_claims(&0, &1), 1);
    let events = w.env.events().all();
    let soroban_sdk::xdr::ContractEventBody::V0(body) = &events.events().last().unwrap().body;
    let data: Val = body.data.clone().into_val(&w.env);
    let event: Map<Symbol, Val> = data.into_val(&w.env);
    let archived_claim: crate::PendingClaim = event
        .get(Symbol::new(&w.env, "claim"))
        .unwrap()
        .into_val(&w.env);
    assert_eq!(archived_claim.id, first);
    assert_eq!(archived_claim.amount, HUNDRED);
    assert!(archived_claim.settled);
    let provenance: Option<crate::ClaimProvenance> = event
        .get(Symbol::new(&w.env, "provenance"))
        .unwrap()
        .into_val(&w.env);
    assert_eq!(provenance.unwrap().message_id, message_id(&w.env, 101));
    assert_eq!(w.router().archive_settled_claims(&1, &32), 3);
    assert_eq!(w.router().archive_settled_claims(&1, &32), 3);
    assert_eq!(
        w.router().claim_history(&first),
        ClaimHistory::Archived(first)
    );
    assert!(matches!(
        w.router().claim_history(&pending),
        ClaimHistory::Live(_)
    ));
    assert!(matches!(
        w.router().claim_history(&legacy),
        ClaimHistory::Live(_)
    ));
    assert_eq!(
        w.router().try_settle_claim(&w.user, &first),
        Err(Ok(HyperionError::ClaimAlreadySettled))
    );
    assert!(w
        .router()
        .was_processed(&RouteKind::Cctp, &message_id(&w.env, 101)));
    assert_eq!(w.router().claim_liability(&liability.token), liability);
    assert_eq!(
        w.router().try_archive_settled_claims(&0, &33),
        Err(Ok(HyperionError::InvalidLimit))
    );
    assert_eq!(w.router().claim_history(&999), ClaimHistory::Unavailable);
    // A future reviewed schema backfill may skip only the permanent intentional marker.
    w.env.as_contract(&w.router_id, || {
        storage::set_schema_version(&w.env, 1);
        w.env
            .storage()
            .instance()
            .remove(&DataKey::LiabilityTracking);
        w.env
            .storage()
            .instance()
            .remove(&DataKey::ClaimLiability(liability.token.clone()));
    });
    w.run_action(AdminAction::BeginStorageMigration(
        crate::CURRENT_SCHEMA_VERSION,
    ));
    let migrated = w
        .router()
        .migrate_storage(&w.admin, &crate::CURRENT_SCHEMA_VERSION, &32);
    assert!(!migrated.migration_pending);
    assert_eq!(w.router().claim_liability(&liability.token), liability);
}

#[test]
fn cleanup_uses_settlement_time_not_creation_time_and_expired_sidecar_is_unknown() {
    let w = World::new();
    let id = parked(&w, 104);
    w.env
        .ledger()
        .set_timestamp(w.env.ledger().timestamp() + storage::MIN_CLAIM_RETENTION_SECONDS * 2);
    let claim = w.router().get_claim(&id);
    w.failable(&claim.token).set_blocked(&w.recipient, &false);
    w.router().settle_claim(&w.user, &id);
    w.router().archive_settled_claims(&0, &32);
    assert!(matches!(
        w.router().claim_history(&id),
        ClaimHistory::Live(_)
    ));
    w.env.as_contract(&w.router_id, || {
        w.env
            .storage()
            .persistent()
            .remove(&DataKey::ClaimSettledAt(id))
    });
    w.env
        .ledger()
        .set_timestamp(w.env.ledger().timestamp() + storage::MIN_CLAIM_RETENTION_SECONDS);
    w.router().archive_settled_claims(&0, &32);
    assert!(matches!(
        w.router().claim_history(&id),
        ClaimHistory::Live(_)
    ));
}

#[test]
fn versioned_quotes_conserve_dust_and_legacy_result_matches_dispatch() {
    let w = World::new();
    for amount in [1i128, 9, 10, 10_000_005] {
        let old = w.router().quote_routes(&w.token_id, &amount, &EVM_DECIMALS);
        let new = w
            .router()
            .quote_routes_v2(&w.token_id, &amount, &EVM_DECIMALS);
        for i in 0..old.len() {
            assert_eq!(old.get(i).unwrap(), new.get(i).unwrap().quote);
            let DebitBreakdown::Known(b) = new.get(i).unwrap().breakdown else {
                panic!("valid amount needs breakdown")
            };
            assert_eq!(b.requested_amount, b.actual_debit + b.retained_dust);
            assert_eq!(b.actual_debit, b.fee + b.net_amount);
        }
    }
    let amount = 10_000_005;
    let before = w.token().balance(&w.user);
    let v = w
        .router()
        .quote_routes_v2(&w.token_id, &amount, &EVM_DECIMALS)
        .get(0)
        .unwrap()
        .breakdown;
    let DebitBreakdown::Known(b) = v else {
        panic!("valid amount needs breakdown")
    };
    w.router().bridge_out(
        &w.user,
        &OutboundRequest {
            token: w.token_id.clone(),
            amount,
            route: RouteKind::Cctp,
            destination: Destination {
                chain: w.ethereum(),
                address: evm_destination(&w.env, 0x33),
            },
            destination_decimals: EVM_DECIMALS,
            min_destination_amount: 0,
        },
    );
    assert_eq!(before - w.token().balance(&w.user), b.actual_debit);
}

#[test]
#[ignore = "requires released and freshly built candidate WASM; run rehearse-upgrade.sh"]
fn actual_wasm_upgrade_preserves_state_and_failure_retains_executable() {
    let old_path = std::env::var("HYPERION_UPGRADE_OLD_WASM").expect("released WASM path required");
    let new_path =
        std::env::var("HYPERION_UPGRADE_NEW_WASM").expect("candidate WASM path required");
    let w = World::uninitialised();
    let old = std::fs::read(old_path).unwrap();
    w.env.register_at(&w.router_id, old.as_slice(), ());
    w.router().initialize(
        &w.admin,
        &w.guardian,
        &w.treasury,
        &super::setup::FEE_BPS,
        &hyperion_core::DEFAULT_FLOW_WINDOW_LEDGERS,
        &crate::MIN_TIMELOCK_DELAY,
    );
    w.enable_every_route();
    w.register_token(
        w.token_id.clone(),
        hyperion_core::STELLAR_DECIMALS,
        super::setup::FLOW_LIMIT,
    );
    w.fund_user(HUNDRED * 10);
    let pending = parked(&w, 201);
    let paid = settled(&w, 202);
    w.router().bridge_out(
        &w.user,
        &OutboundRequest {
            token: w.token_id.clone(),
            amount: HUNDRED,
            route: RouteKind::Cctp,
            destination: Destination {
                chain: w.ethereum(),
                address: evm_destination(&w.env, 0x44),
            },
            destination_decimals: EVM_DECIMALS,
            min_destination_amount: 0,
        },
    );
    let queued = w
        .router()
        .queue_action(&w.admin, &AdminAction::SetFeeBps(20));
    let snapshot = || {
        std::format!(
            "{:?}",
            (
                (
                    w.router().get_config(),
                    w.router().get_token(&w.token_id),
                    w.router().get_adapter(&RouteKind::Cctp),
                    w.router().get_rail_receiver(&RouteKind::Cctp),
                    w.router().get_transfer(&1)
                ),
                w.router().get_claim(&pending),
                w.router().get_claim(&paid),
                w.router().get_claim_provenance(&pending),
                w.router()
                    .claim_liability(&w.router().get_claim(&pending).token),
                w.router()
                    .was_processed(&RouteKind::Cctp, &message_id(&w.env, 201)),
                w.router().flow_available(&w.token_id, &RouteKind::Cctp),
                w.router().get_queued(&queued)
            )
        )
    };
    let before = snapshot();
    let original_executable = w.router_id.executable();
    let bad = w.router().queue_action(
        &w.admin,
        &AdminAction::Upgrade(BytesN::from_array(&w.env, &[0xee; 32])),
    );
    w.env
        .ledger()
        .set_timestamp(w.router().get_queued(&bad).eta);
    assert!(w.router().try_execute_action(&w.admin, &bad).is_err());
    assert_eq!(w.router_id.executable(), original_executable);
    assert_eq!(before, snapshot());
    assert_eq!(w.router().get_queued(&bad).id, bad);
    let bytes = std::fs::read(new_path).unwrap();
    let hash = w.env.deployer().upload_contract_wasm(bytes.as_slice());
    let upgrade = w
        .router()
        .queue_action(&w.admin, &AdminAction::Upgrade(hash.clone()));
    w.env
        .ledger()
        .set_timestamp(w.router().get_queued(&upgrade).eta);
    w.router().execute_action(&w.admin, &upgrade);
    assert_eq!(
        w.router_id.executable(),
        Some(soroban_sdk::Executable::Wasm(hash))
    );
    assert_eq!(before, snapshot());
    assert!(!w
        .router()
        .quote_routes_v2(&w.token_id, &HUNDRED, &EVM_DECIMALS)
        .is_empty());
    std::println!("WASM_UPGRADE_REPORT intentionally_migrated=[executable,upgrade_queue,audit_events] unexpected_differences=[] failed_upgrade_retained_original=true");
}
