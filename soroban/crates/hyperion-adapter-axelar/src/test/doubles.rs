//! Stand-ins for assets that are the wrong shape rather than merely misconfigured.

use soroban_sdk::{contract, contractimpl, Env};

/// An asset that answers nothing this build understands.
///
/// Not a broken token: a contract that was never a SEP-41 asset at all, which is what an operator
/// pastes in when they copy an address from the wrong column. It has to be told apart from a token
/// with the wrong decimals, because the fix is different.
#[contract]
pub struct MuteToken;

#[contractimpl]
impl MuteToken {
    pub fn something_else(_env: Env) -> u32 {
        0
    }
}
