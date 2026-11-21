# CCTP finality policy

## Supported policies

Soroban supports two explicit lane policies: finalized (threshold `2000`) and fast
(threshold `1000`). New and previously configured lanes default to finalized; no weaker
threshold is inferred from legacy adapter configuration. Fast finality requires an admin to
set a nonzero fee budget and then explicitly select `Fast` for that linked domain. Arbitrary
thresholds and attempts to enable fast finality through the legacy global `set_fee_policy`
entrypoint are rejected. The fee-budget ceiling remains global, while the finality choice is
per lane.

Use the lane-aware `quote_dispatch` view before presenting a transfer. It returns the destination
domain, effective maximum fee and finality threshold. The existing dispatch event reports the
same threshold and fee actually sent to Circle. `quote_max_fee` remains for compatibility and
does not identify a lane; it is not a replacement for `quote_dispatch`.

The EVM development adapter is finalized-only. `quoteFinalityPolicy` reports threshold `2000`
for a configured, enabled lane, and `FinalityPolicyUsed` records that value at dispatch. This
does not independently validate or activate the EVM route.

## Compatibility and migration

The Soroban `Config` wire shape and existing dispatch signatures are unchanged. A new persistent
domain policy key is additive; a missing key resolves to finalized, including on upgrade. Thus
an older deployment whose global threshold was set below `2000` becomes finalized on all lanes
until an operator explicitly selects fast finality per lane. This intentionally favors safety
over preserving a weaker legacy setting. Existing `set_fee_policy` calls that pass a threshold
other than `2000` must be replaced with a fee-budget update using `2000`, followed, only where
validated and intended, by `set_lane_finality_policy(domain, Fast)`.

The EVM change is additive: the existing lane and burn interfaces remain intact, with a new
read-only policy quote and dispatch diagnostic event. No route is enabled by these changes.
Stellar Testnet outbound ITS/CCTP remains the active deployment; EVM and other development
paths remain gated pending independent validation. Circle's message transmitter continues to
authenticate inbound attestations; this policy does not alter inbound replay checks, exact
integer amounts, recipient-bound claims, or authenticated router dispatch.
