use super::setup::{net_of, World, HUNDRED};
use hyperion_core::{HyperionError, RouteKind};
use hyperion_router::{Destination, OutboundRequest};
use soroban_sdk::{
    testutils::{MockAuth, MockAuthInvoke},
    IntoVal,
};

fn request(w: &World) -> OutboundRequest {
    OutboundRequest {
        token: w.token_id.clone(),
        amount: HUNDRED,
        route: RouteKind::AxelarIts,
        destination: Destination {
            chain: w.ethereum(),
            address: w.evm(1),
        },
        destination_decimals: 7,
        min_destination_amount: net_of(HUNDRED),
    }
}
#[test]
fn paid_transfer_keeps_router_fees_and_pays_actual_gas_atomically() {
    let w = World::new();
    w.its().set_native(&w.lock_asset);
    let before = w.lock_token().balance(&w.user);
    let nonce = w.router().bridge_out_with_gas(&w.user, &request(&w), &1000);
    assert_eq!(nonce, 1);
    assert!(w.its().last_transfer().gas_attached);
    assert_eq!(w.its().last_transfer().amount, net_of(HUNDRED));
    assert_eq!(w.lock_token().balance(&w.user), before - 1000);
    assert_eq!(w.lock_token().balance(&w.its_id), 1000);
    assert_eq!(w.token().balance(&w.treasury), HUNDRED - net_of(HUNDRED));
    assert_eq!(w.token().balance(&w.bare_id), 0);
    assert_eq!(w.lock_token().balance(&w.bare_id), 0);
}
#[test]
fn the_sender_authorizes_gas_and_router_debit_with_no_adapter_signature() {
    for bounded in [false, true] {
        let w = World::new();
        w.its().set_native(&w.lock_asset);
        let req = request(&w);
        let fee = HUNDRED - net_of(HUNDRED);
        let gas = Some(crate::rail::Token {
            address: w.lock_asset.clone(),
            amount: 1000,
        });
        let no_data: Option<soroban_sdk::Bytes> = None;
        let recipient = soroban_sdk::Bytes::from_array(
            &w.env,
            &hyperion_core::address::bytes32_to_evm(&w.env, &req.destination.address)
                .unwrap()
                .to_array(),
        );
        w.env.mock_auths(&[MockAuth {
            address: &w.user,
            invoke: &MockAuthInvoke {
                contract: &w.router_id,
                fn_name: if bounded {
                    "bridge_out_bounded"
                } else {
                    "bridge_out_with_gas"
                },
                args: if bounded {
                    (w.user.clone(), req.clone(), 1000i128, 1000i128).into_val(&w.env)
                } else {
                    (w.user.clone(), req.clone(), 1000i128).into_val(&w.env)
                },
                sub_invokes: &[
                    MockAuthInvoke {
                        contract: &w.token_id,
                        fn_name: "transfer",
                        args: (w.user.clone(), w.treasury.clone(), fee).into_val(&w.env),
                        sub_invokes: &[],
                    },
                    MockAuthInvoke {
                        contract: &w.bare_id,
                        fn_name: "dispatch_from",
                        args: (
                            w.router_id.clone(),
                            w.user.clone(),
                            w.token_id.clone(),
                            net_of(HUNDRED),
                            w.ethereum(),
                            req.destination.address.clone(),
                            1u64,
                            1000i128,
                        )
                            .into_val(&w.env),
                        sub_invokes: &[MockAuthInvoke {
                            contract: &w.its_id,
                            fn_name: "interchain_transfer",
                            args: (
                                w.user.clone(),
                                w.burn_id(),
                                w.axelar_ethereum(),
                                recipient,
                                net_of(HUNDRED),
                                no_data,
                                gas,
                            )
                                .into_val(&w.env),
                            sub_invokes: &[
                                MockAuthInvoke {
                                    contract: &w.token_id,
                                    fn_name: "burn",
                                    args: (w.user.clone(), net_of(HUNDRED)).into_val(&w.env),
                                    sub_invokes: &[],
                                },
                                MockAuthInvoke {
                                    contract: &w.lock_asset,
                                    fn_name: "transfer",
                                    args: (w.user.clone(), w.its_id.clone(), 1000i128)
                                        .into_val(&w.env),
                                    sub_invokes: &[],
                                },
                            ],
                        }],
                    },
                ],
            },
        }]);
        if bounded {
            w.router().bridge_out_bounded(&w.user, &req, &1000, &1000);
        } else {
            w.router().bridge_out_with_gas(&w.user, &req, &1000);
        }
        assert!(w.its().last_transfer().gas_attached);
    }
}
#[test]
fn router_refusal_rolls_back_the_gas_debit() {
    let w = World::new();
    w.its().set_native(&w.lock_asset);
    w.router().pause(&w.admin);
    let balance = w.lock_token().balance(&w.user);
    assert!(w
        .router()
        .try_bridge_out_with_gas(&w.user, &request(&w), &1000)
        .is_err());
    assert_eq!(w.lock_token().balance(&w.user), balance);
    assert_eq!(w.lock_token().balance(&w.bare_id), 0);
}
#[test]
fn gas_cannot_be_zero_or_negative_and_route_must_match() {
    let w = World::new();
    for gas in [0, -1] {
        assert_eq!(
            w.router()
                .try_bridge_out_with_gas(&w.user, &request(&w), &gas),
            Err(Ok(HyperionError::InvalidAmount))
        );
    }
    let mut req = request(&w);
    req.route = RouteKind::Allbridge;
    assert_eq!(
        w.router().try_bridge_out_with_gas(&w.user, &req, &1000),
        Err(Ok(HyperionError::UnsupportedRoute))
    );
}

#[test]
fn insufficient_relay_gas_rolls_back_fee_burn_nonce_and_flow() {
    let w = World::new();
    w.its().set_native(&w.lock_asset);
    let before = w.token().balance(&w.user);
    assert!(w
        .router()
        .try_bridge_out_with_gas(&w.user, &request(&w), &(HUNDRED * 100))
        .is_err());
    assert_eq!(w.token().balance(&w.user), before);
    assert_eq!(w.token().balance(&w.treasury), 0);
    assert_eq!(w.router().last_out_nonce(), 0);
    assert_eq!(w.its().transfer_count(), 0);
}
#[test]
fn a_direct_paid_dispatch_from_a_stranger_is_refused() {
    let w = World::new();
    w.its().set_native(&w.lock_asset);
    assert_eq!(
        w.bare().try_dispatch_from(
            &w.relayer,
            &w.user,
            &w.token_id,
            &HUNDRED,
            &w.ethereum(),
            &w.evm(1),
            &1,
            &1000
        ),
        Err(Ok(HyperionError::Unauthorized))
    );
}

#[test]
fn bridge_and_gas_in_the_same_token_preserve_the_exact_combined_debit() {
    let w = World::new();
    w.its().set_native(&w.token_id);
    let before = w.token().balance(&w.user);
    w.router().bridge_out_with_gas(&w.user, &request(&w), &1000);
    assert_eq!(w.token().balance(&w.user), before - HUNDRED - 1000);
    assert_eq!(w.token().balance(&w.its_id), 1000);
    assert_eq!(w.token().balance(&w.treasury), HUNDRED - net_of(HUNDRED));
}

#[test]
fn explicit_its_cap_uses_the_actual_gas_asset_and_rejects_before_any_fee() {
    let w = World::new();
    w.its().set_native(&w.lock_asset);
    let req = request(&w);
    let bridge_before = w.token().balance(&w.user);
    let gas_before = w.lock_token().balance(&w.user);
    for (gas, cap, reason) in [
        (1000, 999, HyperionError::ProviderBudgetExceeded),
        (1000, -1, HyperionError::InvalidAmount),
        (0, 0, HyperionError::InvalidAmount),
    ] {
        assert_eq!(
            w.router().explain_outbound(&req, &Some(gas), &cap),
            reason as u32
        );
        assert_eq!(
            w.router().try_bridge_out_bounded(&w.user, &req, &gas, &cap),
            Err(Ok(reason))
        );
        assert_eq!(w.token().balance(&w.user), bridge_before);
        assert_eq!(w.lock_token().balance(&w.user), gas_before);
        assert_eq!(w.token().balance(&w.treasury), 0);
        assert_eq!(w.its().transfer_count(), 0);
        assert_eq!(w.router().last_out_nonce(), 0);
    }
    // Views require no account signature and do not perform the provider payment.
    w.env.mock_auths(&[]);
    assert_eq!(w.router().explain_outbound(&req, &Some(1000), &1000), 0);
    assert!(w
        .router()
        .try_bridge_out_bounded(&w.user, &req, &1000, &1000)
        .is_err());
    w.env.mock_all_auths();
    w.router().bridge_out_bounded(&w.user, &req, &1000, &1000);
    assert_eq!(w.lock_token().balance(&w.user), gas_before - 1000);
    assert_eq!(w.token().balance(&w.user), bridge_before - HUNDRED);
}

#[test]
fn its_explanation_matches_adapter_destination_and_combined_debit_overflow() {
    let w = World::new();
    w.its().set_native(&w.token_id);
    let mut req = request(&w);
    req.destination.chain = soroban_sdk::String::from_str(&w.env, "unlinked");
    assert_eq!(
        w.router().explain_outbound(&req, &Some(1000), &1000),
        HyperionError::UnknownChain as u32
    );
    assert_eq!(
        w.router()
            .try_bridge_out_bounded(&w.user, &req, &1000, &1000),
        Err(Ok(HyperionError::UnknownChain))
    );
    req.destination.chain = w.ethereum();
    req.destination.address = soroban_sdk::BytesN::from_array(&w.env, &[1; 32]);
    assert_eq!(
        w.router().explain_outbound(&req, &Some(1000), &1000),
        HyperionError::NotEvmAddress as u32
    );
    assert_eq!(
        w.router()
            .try_bridge_out_bounded(&w.user, &req, &1000, &1000),
        Err(Ok(HyperionError::NotEvmAddress))
    );
    req.destination.address = w.evm(1);
    assert_eq!(
        w.router()
            .explain_outbound(&req, &Some(i128::MAX), &i128::MAX),
        HyperionError::DecimalOverflow as u32
    );
    assert_eq!(
        w.router()
            .try_bridge_out_bounded(&w.user, &req, &i128::MAX, &i128::MAX),
        Err(Ok(HyperionError::DecimalOverflow))
    );
    assert_eq!(w.its().transfer_count(), 0);
    assert_eq!(w.token().balance(&w.treasury), 0);
    assert_eq!(w.router().last_out_nonce(), 0);
}

#[test]
fn paid_dispatch_auth_spec_returns_exact_offline_parameters_for_burning_and_locking() {
    let w = World::new();
    w.its().set_native(&w.lock_asset);

    // 1. Burnable asset spec
    let spec = w
        .bare()
        .paid_dispatch_auth_spec(&w.token_id, &HUNDRED, &w.ethereum(), &1000);
    assert_eq!(spec.token, w.token_id);
    assert_eq!(spec.amount, HUNDRED);
    assert_eq!(spec.token_manager, w.token_id);
    assert!(spec.takes_by_burning);
    assert_eq!(spec.gas_token, w.lock_asset);
    assert_eq!(spec.gas_recipient, w.its_id);
    assert_eq!(spec.gas_amount, 1000);

    // 2. Lockable asset spec
    let lock_spec = w
        .bare()
        .paid_dispatch_auth_spec(&w.lock_asset, &HUNDRED, &w.ethereum(), &500);
    assert_eq!(lock_spec.token, w.lock_asset);
    assert_eq!(lock_spec.amount, HUNDRED);
    assert_eq!(lock_spec.token_manager, w.manager_id);
    assert!(!lock_spec.takes_by_burning);
    assert_eq!(lock_spec.gas_token, w.lock_asset);
    assert_eq!(lock_spec.gas_recipient, w.its_id);
    assert_eq!(lock_spec.gas_amount, 500);

    // 3. Invalid inputs
    assert_eq!(
        w.bare()
            .try_paid_dispatch_auth_spec(&w.token_id, &0, &w.ethereum(), &1000),
        Err(Ok(HyperionError::InvalidAmount))
    );
    assert_eq!(
        w.bare()
            .try_paid_dispatch_auth_spec(&w.token_id, &HUNDRED, &w.ethereum(), &0),
        Err(Ok(HyperionError::InvalidAmount))
    );
}
