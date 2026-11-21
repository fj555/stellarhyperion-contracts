# Token admission and exact accounting

Registration checks the token's reported decimals against the configured integer precision and requires a readable balance. Soroban's `check_token_behavior` performs the same metadata check without registering a token. A successful check classifies the token as **metadata compatible, transfer behavior unverified**. A read-only metadata call cannot establish that a token transfers honestly, does not rebase, or will retain its current implementation. Operators must review the token implementation and any upgrade authority before activation. Registration is not a certification of arbitrary token code.

Both routers require exact sender debits and recipient credits around source transfers, inbound pulls, recipient payouts and claim settlement. A token that returns success while charging a transfer fee, taking an extra debit or silently under-delivering causes the entire transaction to revert. A failed payout creates a pending claim only when neither balance changed. A missing trustline is treated as a zero recipient balance only for the host-authenticated built-in Stellar Asset Contract and its specific missing-trustline error; arbitrary token balance errors are rejected. The claim remains recipient-bound and funded until an exact settlement succeeds. Account aliases that make an expected debit or credit impossible, including a recipient equal to the router, are rejected.

The paid Stellar path also verifies the sender's complete source debit. ITS accounts for gas when the bridge asset is also the native gas token. The CCTP adapter checks its exact receipt before approving Circle, and the ITS adapter checks exact sender debit and lock-manager receipt; burn-manager routes rely on ITS's authenticated burn operation. These checks retain the original sender authorization tree, rail authentication, replay key and transaction boundary. Any failed invariant rolls back fees, nonce, flow consumption, approvals and rail dispatch in the source transaction.

Canonical USDC's six-decimal ERC-20 and seven-decimal Stellar Asset Contract semantics remain compatible; the local suites exercise standard ERC-20 and actual Stellar Asset Contract fixtures. No funded network transfer or new deployment is claimed by this change. The active Stellar Testnet outbound ITS/CCTP scope is unchanged. EVM deployments, inbound production and other rails remain subject to their existing independent validation and activation gates.

## Registration conflicts before activation

A registration can disagree with what is already configured in four ways: the local asset is
already linked to another token id, the token id is already linked to another local asset, the
asset's reported decimals are not what the registration expects, or Axelar's own registry backs the
token id with a different asset or a different kind of token manager. Each is a separate field, and
an operator fixing one at a time needs a separate failed transaction per field.

`soroban/crates/hyperion-adapter-axelar` exposes `check_token_mapping` and
`evm/src/adapters/AxelarItsAdapter.sol` exposes `checkTokenMapping` for exactly that. Both are read
only: they write no storage, extend no TTL, emit no event, and return every disagreement in one
call, in the same order, using the same names (`TokenMappingConflict`). Repeated validation of a
correct mapping therefore returns the same empty report and leaves the contract untouched. The
diagnostic reports the observed fields alongside the verdict, so an operator can see what Axelar
says the id should be rather than only that the proposal was refused.

Registration itself is unchanged in what it accepts. A conflicting registration is still refused —
`AlreadyConfigured` when a mapping would be repointed in either direction, `TokenNotMapped` when
Axelar has no asset behind the id — and a repeated identical registration is still a successful
no-op that writes nothing and publishes no event. The diagnostic adds a way to see the conflict
before activation, not a way to apply one.

## Compatibility and migration

Token configuration, integer amounts, wire layouts and replay/claim storage retain their existing formats. Existing configured tokens are checked on every transfer; registering a token checks current metadata at queue and execution time. No storage migration or automatic token re-registration is required. Operators should disable unsupported tokens, review previously queued registrations, and test canonical assets before upgrading the router and affected paid adapters together. Previously parked claims retain their original recipient and amount; a non-conforming token cannot mark them settled. An unsupported token may require a separately reviewed recovery plan rather than accepting false accounting.

Balance checks detect observable mismatches in the transaction. A deliberately dishonest `balanceOf` implementation, compensating rebase, later token upgrade or dishonest rail dependency can lie consistently about balances; metadata and delta checks cannot prove otherwise. Those risks remain part of operator token/rail review.
