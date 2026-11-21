//! Tests for CCTP remote-messenger rotation with overlap controls,
//! historical-message acceptance windows, and emergency-disable controls.

use hyperion_core::{
    cctp::{DEFAULT_ROTATION_OVERLAP_WINDOW, MIN_PEER_ROTATION_DELAY},
    HyperionError,
};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, BytesN,
};

use super::setup::{World, ETHEREUM_DOMAIN};

const ARRIVING: i128 = 500_000_000;

#[test]
fn peer_messenger_rotation_lifecycle_and_timelock() {
    let w = World::new();
    let peer_a = BytesN::from_array(&w.env, &[0xAAu8; 32]);
    let peer_b = BytesN::from_array(&w.env, &[0xBBu8; 32]);

    // Initial peer configuration
    w.adapter().set_peer_messenger(&ETHEREUM_DOMAIN, &peer_a);
    assert_eq!(
        w.adapter().peer_messenger(&ETHEREUM_DOMAIN),
        Some(peer_a.clone())
    );

    // Propose rotation to peer_b with valid delay
    let delay = MIN_PEER_ROTATION_DELAY;
    w.adapter()
        .propose_peer_rotation(&ETHEREUM_DOMAIN, &peer_b, &delay);

    let pending = w
        .adapter()
        .pending_peer_rotation(&ETHEREUM_DOMAIN)
        .expect("pending rotation should be set");
    assert_eq!(pending.new_messenger, peer_b);
    assert_eq!(pending.eta, w.env.ledger().timestamp() + delay);

    // Cannot apply before eta
    assert_eq!(
        w.adapter().try_apply_peer_rotation(&ETHEREUM_DOMAIN),
        Err(Ok(HyperionError::RotationNotReady))
    );

    // Advance time past eta
    w.env.ledger().with_mut(|li| {
        li.timestamp += delay + 1;
    });

    // Apply rotation succeeds
    w.adapter().apply_peer_rotation(&ETHEREUM_DOMAIN);
    assert_eq!(
        w.adapter().peer_messenger(&ETHEREUM_DOMAIN),
        Some(peer_b.clone())
    );
    assert!(w
        .adapter()
        .pending_peer_rotation(&ETHEREUM_DOMAIN)
        .is_none());

    // Historical overlap window is recorded for old peer_a
    let hist = w
        .adapter()
        .old_peer_messenger(&ETHEREUM_DOMAIN)
        .expect("historical overlap window should exist");
    assert_eq!(hist.old_messenger, peer_a);
    assert_eq!(
        hist.valid_until,
        w.env.ledger().timestamp() + DEFAULT_ROTATION_OVERLAP_WINDOW
    );
}

#[test]
fn peer_messenger_rotation_validations() {
    let w = World::new();
    let peer_a = BytesN::from_array(&w.env, &[0xAAu8; 32]);
    let peer_b = BytesN::from_array(&w.env, &[0xBBu8; 32]);
    let zero = BytesN::from_array(&w.env, &[0u8; 32]);

    w.adapter().set_peer_messenger(&ETHEREUM_DOMAIN, &peer_a);

    // Unknown domain
    assert_eq!(
        w.adapter()
            .try_propose_peer_rotation(&99, &peer_b, &MIN_PEER_ROTATION_DELAY),
        Err(Ok(HyperionError::UnknownChain))
    );

    // Zero address
    assert_eq!(
        w.adapter()
            .try_propose_peer_rotation(&ETHEREUM_DOMAIN, &zero, &MIN_PEER_ROTATION_DELAY),
        Err(Ok(HyperionError::ZeroAddressKey))
    );

    // Same peer
    assert_eq!(
        w.adapter()
            .try_propose_peer_rotation(&ETHEREUM_DOMAIN, &peer_a, &MIN_PEER_ROTATION_DELAY),
        Err(Ok(HyperionError::MessengerUnchanged))
    );

    // Delay below minimum
    assert_eq!(
        w.adapter().try_propose_peer_rotation(
            &ETHEREUM_DOMAIN,
            &peer_b,
            &(MIN_PEER_ROTATION_DELAY - 1)
        ),
        Err(Ok(HyperionError::TimelockDelayOutOfRange))
    );

    // Propose valid rotation
    w.adapter()
        .propose_peer_rotation(&ETHEREUM_DOMAIN, &peer_b, &MIN_PEER_ROTATION_DELAY);

    // Already pending
    let peer_c = BytesN::from_array(&w.env, &[0xCCu8; 32]);
    assert_eq!(
        w.adapter()
            .try_propose_peer_rotation(&ETHEREUM_DOMAIN, &peer_c, &MIN_PEER_ROTATION_DELAY),
        Err(Ok(HyperionError::RotationAlreadyPending))
    );
}

#[test]
fn cancel_peer_messenger_rotation() {
    let w = World::new();
    let peer_a = BytesN::from_array(&w.env, &[0xAAu8; 32]);
    let peer_b = BytesN::from_array(&w.env, &[0xBBu8; 32]);

    w.adapter().set_peer_messenger(&ETHEREUM_DOMAIN, &peer_a);

    // Cannot cancel when none queued
    assert_eq!(
        w.adapter().try_cancel_peer_rotation(&ETHEREUM_DOMAIN),
        Err(Ok(HyperionError::RotationNotPending))
    );

    // Propose then cancel
    w.adapter()
        .propose_peer_rotation(&ETHEREUM_DOMAIN, &peer_b, &MIN_PEER_ROTATION_DELAY);
    assert!(w
        .adapter()
        .pending_peer_rotation(&ETHEREUM_DOMAIN)
        .is_some());

    w.adapter().cancel_peer_rotation(&ETHEREUM_DOMAIN);
    assert!(w
        .adapter()
        .pending_peer_rotation(&ETHEREUM_DOMAIN)
        .is_none());
    assert_eq!(w.adapter().peer_messenger(&ETHEREUM_DOMAIN), Some(peer_a));
}

#[test]
fn in_flight_messages_accepted_during_overlap_window_and_refused_after_expiration() {
    let w = World::new();
    let peer_a = [0xAAu8; 32];
    let peer_b = [0xBBu8; 32];
    let peer_a_bn = BytesN::from_array(&w.env, &peer_a);
    let peer_b_bn = BytesN::from_array(&w.env, &peer_b);

    // Set initial peer messenger A
    w.adapter().set_peer_messenger(&ETHEREUM_DOMAIN, &peer_a_bn);

    // Rotate to peer B
    w.adapter()
        .propose_peer_rotation(&ETHEREUM_DOMAIN, &peer_b_bn, &MIN_PEER_ROTATION_DELAY);
    w.env.ledger().with_mut(|li| {
        li.timestamp += MIN_PEER_ROTATION_DELAY + 1;
    });
    w.adapter().apply_peer_rotation(&ETHEREUM_DOMAIN);

    // 1. In-flight message with old peer A arrives during overlap window
    let mut spec_a = w.spec(ARRIVING, 101, w.hook_to_recipient());
    spec_a.sender = peer_a;
    let msg_a = spec_a.encode(&w.env);

    let claim_id = w.adapter().receive(&w.relayer, &msg_a, &w.attestation());
    assert_eq!(claim_id, 0);
    assert_eq!(w.token().balance(&w.recipient), ARRIVING);

    // 2. Message from new peer B also succeeds
    let mut spec_b = w.spec(ARRIVING, 102, w.hook_to_recipient());
    spec_b.sender = peer_b;
    let msg_b = spec_b.encode(&w.env);

    let claim_id_b = w.adapter().receive(&w.relayer, &msg_b, &w.attestation());
    assert_eq!(claim_id_b, 0);
    assert_eq!(w.token().balance(&w.recipient), ARRIVING * 2);

    // 3. Fast forward time past the overlap acceptance window
    w.env.ledger().with_mut(|li| {
        li.timestamp += DEFAULT_ROTATION_OVERLAP_WINDOW + 10;
    });

    // In-flight message with retired old peer A now expires and is refused!
    let mut spec_a_late = w.spec(ARRIVING, 103, w.hook_to_recipient());
    spec_a_late.sender = peer_a;
    let msg_a_late = spec_a_late.encode(&w.env);

    assert_eq!(
        w.adapter()
            .try_receive(&w.relayer, &msg_a_late, &w.attestation()),
        Err(Ok(HyperionError::OldMessengerExpired))
    );
    // Balance has not changed; old message never acquires custody
    assert_eq!(w.token().balance(&w.recipient), ARRIVING * 2);
}

#[test]
fn messages_from_unauthorized_peers_never_acquire_inbound_custody() {
    let w = World::new();
    let authorized_peer = [0xAAu8; 32];
    let unauthorized_peer = [0xEEu8; 32];

    w.adapter().set_peer_messenger(
        &ETHEREUM_DOMAIN,
        &BytesN::from_array(&w.env, &authorized_peer),
    );

    // An unauthorized peer sends an attested message
    let mut spec = w.spec(ARRIVING, 201, w.hook_to_recipient());
    spec.message_sender = unauthorized_peer;
    spec.sender = unauthorized_peer;
    let msg = spec.encode(&w.env);

    assert_eq!(
        w.adapter().try_receive(&w.relayer, &msg, &w.attestation()),
        Err(Ok(HyperionError::UnexpectedRailContract))
    );
    // Unauthorized peer never acquires custody; recipient and adapter balances remain zero
    assert_eq!(w.token().balance(&w.recipient), 0);
    assert_eq!(w.token().balance(&w.adapter_id), 0);
}

#[test]
fn token_messenger_rotation_lifecycle_and_overlap() {
    let w = World::new();
    let old_messenger = w.messenger_id.clone();
    let new_messenger = Address::generate(&w.env);

    // Propose rotation
    w.adapter()
        .propose_messenger_rotation(&new_messenger, &MIN_PEER_ROTATION_DELAY);

    let pending = w
        .adapter()
        .pending_messenger_rotation()
        .expect("pending messenger rotation should exist");
    assert_eq!(pending.new_messenger, new_messenger);

    // Cannot apply before eta
    assert_eq!(
        w.adapter().try_apply_messenger_rotation(),
        Err(Ok(HyperionError::RotationNotReady))
    );

    // Advance time and apply
    w.env.ledger().with_mut(|li| {
        li.timestamp += MIN_PEER_ROTATION_DELAY + 1;
    });
    w.adapter().apply_messenger_rotation();

    assert_eq!(w.adapter().get_config().token_messenger, new_messenger);
    assert!(w.adapter().pending_messenger_rotation().is_none());
    let hist = w
        .adapter()
        .old_token_messenger()
        .expect("historical overlap window should exist");
    assert_eq!(hist.old_messenger, old_messenger);
    assert_eq!(
        hist.valid_until,
        w.env.ledger().timestamp() + DEFAULT_ROTATION_OVERLAP_WINDOW
    );

    // In-flight message naming old messenger is accepted during overlap
    let msg_old = w.inbound(ARRIVING, 301);
    let claim_id = w.adapter().receive(&w.relayer, &msg_old, &w.attestation());
    assert_eq!(claim_id, 0);
    assert_eq!(w.token().balance(&w.recipient), ARRIVING);

    // Advance past overlap window
    w.env.ledger().with_mut(|li| {
        li.timestamp += DEFAULT_ROTATION_OVERLAP_WINDOW + 10;
    });

    // Message naming old messenger is now refused with OldMessengerExpired
    let msg_expired = w.inbound(ARRIVING, 302);
    assert_eq!(
        w.adapter()
            .try_receive(&w.relayer, &msg_expired, &w.attestation()),
        Err(Ok(HyperionError::OldMessengerExpired))
    );
}

#[test]
fn emergency_disable_controls_halt_custody_immediately() {
    let w = World::new();

    // Normal inbound works
    let claim_id = w
        .adapter()
        .receive(&w.relayer, &w.inbound(ARRIVING, 401), &w.attestation());
    assert_eq!(claim_id, 0);
    assert_eq!(w.token().balance(&w.recipient), ARRIVING);

    // Emergency disable the domain
    w.adapter().set_domain_disabled(&ETHEREUM_DOMAIN, &true);
    assert!(w.adapter().is_domain_disabled(&ETHEREUM_DOMAIN));

    // Inbound is immediately refused
    assert_eq!(
        w.adapter()
            .try_receive(&w.relayer, &w.inbound(ARRIVING, 402), &w.attestation()),
        Err(Ok(HyperionError::RouteDisabled))
    );

    // Outbound dispatch check is immediately refused
    assert_eq!(
        w.adapter().try_check_dispatch(
            &w.router_id,
            &w.token_id,
            &1_000_000,
            &w.ethereum(),
            &BytesN::from_array(&w.env, &[0x11u8; 32]),
            &None
        ),
        Err(Ok(HyperionError::RouteDisabled))
    );

    // Re-enabling restores custody flow
    w.adapter().set_domain_disabled(&ETHEREUM_DOMAIN, &false);
    assert!(!w.adapter().is_domain_disabled(&ETHEREUM_DOMAIN));

    let claim_id_restored =
        w.adapter()
            .receive(&w.relayer, &w.inbound(ARRIVING, 403), &w.attestation());
    assert_eq!(claim_id_restored, 0);
    assert_eq!(w.token().balance(&w.recipient), ARRIVING * 2);
}

#[test]
fn burn_sender_cannot_impersonate_active_or_historical_envelope_peer() {
    let w = World::new();
    let old = BytesN::from_array(&w.env, &[0xaa; 32]);
    let active = BytesN::from_array(&w.env, &[0xbb; 32]);
    w.adapter().set_peer_messenger(&ETHEREUM_DOMAIN, &old);
    w.adapter()
        .propose_peer_rotation(&ETHEREUM_DOMAIN, &active, &MIN_PEER_ROTATION_DELAY);
    w.env
        .ledger()
        .with_mut(|li| li.timestamp += MIN_PEER_ROTATION_DELAY);
    w.adapter().apply_peer_rotation(&ETHEREUM_DOMAIN);
    for (nonce, claimed_burn_sender) in [(701, [0xaa; 32]), (702, [0xbb; 32])] {
        let mut spec = w.spec(ARRIVING, nonce, w.hook_to_recipient());
        spec.sender = [0xee; 32];
        spec.message_sender = claimed_burn_sender;
        assert_eq!(
            w.adapter()
                .try_receive(&w.relayer, &spec.encode(&w.env), &w.attestation()),
            Err(Ok(HyperionError::UnexpectedRailContract))
        );
        assert_eq!(w.token().balance(&w.recipient), 0);
        assert_eq!(w.token().balance(&w.adapter_id), 0);
    }
}
