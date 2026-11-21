# Parked claim operations

The Soroban router keeps inbound funds in custody when the token rejects a payout. Settlement
always uses the recipient, token, and integer amount recorded when the authenticated rail
delivered the message. The caller of settlement cannot redirect those funds.

`get_claim_provenance(id)` returns the rail, source chain, and complete 32-byte rail message ID
for claims created with the provenance sidecar. Claims created before this addition have no
sidecar (`None`); their original claim record, including its source chain, route, and source
nonce, remains readable and settleable. No rewrite of old claim storage is required.

`claim_liability(token)` reports the tracked sum of unsettled claims alongside the router's
token balance. Counters are updated as new claims are parked and settled, so the read does not
scan an unbounded claim history. Treat `known == false` as indeterminate, never as solvent. A
failed balance read or an upgraded instance without the tracking marker makes the result
unknown. Existing claim records remain settleable; an older deployment needs an explicit
accounting migration before its liability view can be treated as complete. The view grants no
withdrawal or recovery authority and is not a substitute for monitoring the underlying token
and the router's custody.

`preview_claim_recipient(id)` is a read-only best-effort diagnostic. It can identify a missing
Stellar Asset Contract trustline and an unauthorized Stellar Asset Contract account. Other token
implementations return `Unknown` because a balance read cannot establish that a transfer will
succeed.
`Eligible` is only a point-in-time observation, not a promise that a later transfer will succeed.
Settlement remains the source of truth and never changes the saved recipient.

Pausing disables departures. It does not disable authenticated inbound arrivals or settlement,
so an already-finalized bridge message can be credited and a parked claim can be paid while the
guardian has paused outbound activity. The configured rail receiver and replay guard remain
mandatory in this path.

## Bounded Soroban settlement and diagnostics (PR239)

`settle_claims(settler, ids)` accepts 1–50 IDs and requires the settler's signature. It returns
one `ClaimSettlementResult { id, status }` for each input, in input order, including duplicates.
Status 0 confirms settlement, 1 means missing, 2 means already settled, 3 means an unchanged
payout was refused, 4 means custody is short, and 5 means token reads, token accounting, or
token behavior were unavailable. A refused token transfer alone does not prove the account
has a frozen trustline: use `preview_claim_recipient` for SAC-specific restriction information.

Safe balance preflight reports unavailable token reads before an item moves funds. A refused token
sub-call rolls back its own state, so ordinary frozen/short/unreadable items preserve progress of
unrelated payouts. Soroban does not permit router self-invocation, so it cannot isolate a successful
but inexact token transfer inside a router sub-call. Any nonconforming token mutation, failed
post-transfer balance read, or exact-accounting invariant failure reverts the entire batch,
including earlier payouts and diagnostics. This preserves full custody and liability. A
transaction-wide host resource failure also aborts the whole transaction. Settlement always
pays the saved recipient and amount; repeats cannot pay twice. Liability migration blocks both
settlement entry points while a backfill is running.

The additive `claim_failure_reason(id)` sidecar and `claim_failure_v1` event use normalized
reason 1 for an unchanged refusal, 2 for custody shortage, and 3 for unavailable token/accounting.
Reason 0 means no retained failure, or successful settlement. Claim TTL upkeep also extends the
sidecar; none of the existing claim fields or settlement event shapes change. An inbound claim
records its parking reason. The legacy `settle_claim` API still reverts on failure, so a new
failure event or reason cannot survive that reverting call. Use the structured batch API to
retain an ordinary refused attempt's diagnostic; a reverting invariant failure cannot retain one.
A successful retry clears the sidecar to zero.

## Explicit Soroban provider caps and local rejection checks

`bridge_out_bounded(sender, request, gas_amount, max_provider_budget)` is additive: the legacy
`bridge_out` and `bridge_out_with_gas` signatures and `OutboundRequest` fields remain unchanged.
The provider payment and cap are exact i128 base-unit integers. Negative caps and
nonpositive payments are invalid. A payment above the cap returns `ProviderBudgetExceeded` (48)
before any fee or provider movement. Equality is accepted when the remaining checks pass.

For ITS the cap and payment use the configured ITS native gas token's base units (XLM on the
active deployment), separately from the bridged token amount. If both legs use the same token,
the adapter checks the combined debit for overflow before calling ITS. For CCTP the cap uses
burned-token base units (Stellar USDC on the active deployment); its forwarding budget is part
of the burn, never an extra wallet debit, and the minimum receive amount is checked after that
entire budget. These units are never converted into one shared cross-rail budget.

`explain_outbound(request, gas_amount: Option<i128>, max_provider_budget)` returns zero when
local checks pass, otherwise the earliest stable `HyperionError` code used by bounded dispatch.
Paid-route support and cap validity precede router configuration, pause, amount, enabled route,
single-transfer policy, destination, token, fee/rounding/minimum, flow, adapter, and adapter-local
checks. `None` with cap zero checks the legacy unpaid path; any nonzero cap with `None` is invalid.
The view requires no sender signature and moves no funds. It is a local diagnostic; it cannot
predict changing balances, provider execution, or a later wallet signature.

Axelar and CCTP expose additive auth-free `check_dispatch` methods backed by the same local
validation as real dispatch. Missing methods, host errors, malformed results and unknown adapter
error codes produce `AdapterMethodUnavailable` (49); amount-only quotes never replace a missing
method. Known adapter contract errors retain their stable code. Older adapter deployments must
upgrade to obtain these diagnostics and use bounded dispatch; legacy entry points continue to
work. The bounded call retains the sender's actual nested token and provider authorization.

These changes do not activate a new route. Stellar Testnet outbound ITS/CCTP remain the active
supported deployment; EVM and other rails retain their independent deployment and validation gates.

## EVM bounded dispatch and independent batch outcomes

`bridgeOutBounded(request, maxProviderBudget)` preserves the legacy request and caps the whole
native provider payment (`msg.value`) in wei before fee movement. Equality passes; comparisons
remain exact through uint256. Native gas units are separate from ERC20 transfer units. The EVM
CCTP adapter uses finalized delivery with a fixed zero token forwarding fee; the cap does not
pretend to price Stellar XLM or USDC budgets. The legacy `bridgeOut` remains compatible.

`explainOutbound(request, providerPayment, maxProviderBudget)` returns zero or the earliest
local error selector used by bounded dispatch: cap, pause, amount, route, destination, token,
fee/minimum, flow, adapter and adapter-local validation. The public read-only `checkOutbound`
helper lets this view normalize reverting local errors. It does not sign, move funds, consume
flow, or predict changing wallet balances/provider execution. Auth-free `checkDispatch` on the
real CCTP/ITS adapters shares their exact local dispatch validation. Missing methods are
`AdapterMethodUnavailable`; no amount-only quote is accepted in their place. Older adapters
need to upgrade for the additive bounded/explanation API; existing entrypoints retain their
signatures.

The EVM batch keeps an outer reentrancy guard and calls a router-only atomic item entrypoint.
A reverted transfer, unreadable balance, false return after movement, or inexact balance
mutation rolls back only that item. Other valid items can settle before or after it, and repeated
IDs cannot pay twice. External callers and token callbacks cannot invoke the item entrypoint.
Successful legacy `ClaimSettled` events retain the actual outer settler. Legacy single-claim
settlement still reverts on failure and therefore cannot retain a new failure event.

Version 1 EVM reason codes are 1 recipient/token refusal, 2 custody shortage, 3 unreadable token,
and 4 inexact token payout. Existing batch status values remain fixed; `TokenUnavailable` and
`InvalidTokenPayout` append two outcomes. Failed items preserve full custody, stored amount and
recipient, and a later successful retry clears the reason. Soroban's documented whole-batch
rollback for successful nonconforming token changes remains different because its host does not
support the EVM item's invocation rollback mechanism. These local changes activate no deployment.

## EVM-development Axelar ITS gas top-ups

Each EVM Axelar ITS dispatch records a fixed-size intent keyed by the router nonce, adapter,
original sender, token/token id, original amount, and a hash of the Hyperion and Axelar destination
bindings. The router uses an additive sender-aware adapter entry point for ITS; the adapter accepts
that call only from its configured router.

The updated router requires the configured ITS adapter to implement `dispatchWithSender`; it does
not fall back to the legacy `dispatch` selector. An older adapter therefore causes ITS outbound
dispatches through this router to revert. Before enabling this EVM development path, deploy the
compatible adapter and update the router's adapter configuration through its timelock. This does
not migrate intents from an older adapter; its already in-flight messages cannot use this top-up
entry point.

Destination chain names are limited to 64 bytes and Stellar strkeys to 69 bytes before they enter
that intent. A top-up must echo the recorded destination binding, token id, token address, and
original amount, and the original sender must make the top-up call; these fields are checked, not
used to make another transfer. The top-up path only forwards native currency to Axelar's gas
service, so it cannot change the recipient, token, or amount of the original ITS transfer.

The original sender's first top-up binds the intent to an explicit Axelar source transaction
hash/log index and chosen refund recipient. Later calls must come from the same sender and repeat
the exact message and refund binding. Axelar's gas-service API accepts that source-log reference but
does not expose a way for this adapter to inspect the receipt and prove the reference identifies
the ITS log; operators must take it from the original transaction's Axelar ITS event. Once bound, the message reference and refund address cannot be changed, and a message reference cannot bind
to another intent. Each intent has one cumulative
native-spend cap in wei, set at its first top-up; each payment is checked against the remaining
amount before calling Axelar. Axelar sends any later refund to the bound refund recipient.

Top-ups intentionally are not replay-rejected: more than one payment to the same bound message
is valid if the cumulative cap permits it. The message/refund binding and cap are immutable after
the first payment, and repeated identical calls cannot spend beyond the remaining cap. A new
dispatch has a different nonce and therefore a different intent. This remains an EVM-development
path and does not change the active Stellar Testnet deployment gate.
