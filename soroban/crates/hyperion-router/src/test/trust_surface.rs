use hyperion_core::{
    inbound::{Origin, Recipient, Router},
    HyperionError, RouteKind,
};
use soroban_sdk::{Address, Env};

struct RouterInterfaceFixture;

impl Router for RouterInterfaceFixture {
    fn bridge_in(
        _env: Env,
        _caller: Address,
        _route: RouteKind,
        _token: Address,
        _amount: i128,
        _recipient: Recipient,
        _origin: Origin,
    ) -> Result<u64, HyperionError> {
        Err(HyperionError::InvalidAmount)
    }

    fn is_paused(_env: Env) -> bool {
        true
    }

    fn treasury(_env: Env) -> Result<Address, HyperionError> {
        Err(HyperionError::InvalidAmount)
    }
}

#[test]
fn adapter_router_interface_remains_the_minimal_inbound_boundary() {
    // This implementation is intentionally complete: adding a required router capability
    // forces this adapter-facing trust-boundary fixture to be reviewed.
    let env = Env::default();
    assert!(matches!(
        RouterInterfaceFixture::treasury(env),
        Err(HyperionError::InvalidAmount)
    ));
}
