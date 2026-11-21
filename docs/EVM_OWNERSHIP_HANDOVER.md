# Local adapter ownership handover

Run `RPC=http://127.0.0.1:8545 bash script/anvil-ownership-handover.sh`
against a fresh local Anvil node with its standard unlocked development accounts.
The driver and handover script require chain ID 31337 or 1337. The driver performs
the ordinary deployment and timelock rehearsal before changing adapter owners.

Each real CCTP and Axelar ITS adapter uses `Ownable2Step` for configuration.
Nomination sets `pendingOwner()` while the previous owner retains configuration
authority. The nominated account must call `acceptOwnership()` before configuring
the adapter. Its acceptance clears the pending owner and removes the previous
owner's configuration authority.

The router uses `AccessControl`. Adapter ownership does not grant router admin or
guardian roles, replace the registered rail, change the immutable `ROUTER()`
binding, or authorize direct dispatch. The report reads the actual role IDs and
checks the admin and guardian from the phase-one deployment record.

The rehearsal saves `evm/deployments/ownership-<chain>-initial.json`,
`-pending.json`, `-partial.json` and `-accepted.json`. The partial report observes
one accepted adapter alongside one still nominated adapter. Every report includes
each adapter's current owner, pending owner, account and bound router; it records
the observation block without treating the requested next owner as the current
owner. The ordinary phase-one record also includes observed ownership and router
role fields.

The standalone `OwnershipHandover.s.sol` supports `report`, `nominate` and `accept`
through `HYPERION_HANDOVER_MODE`, with `HYPERION_RECORD` selecting the phase-one
record and `HYPERION_HANDOVER_NEXT_OWNER` selecting the nominee. Nomination and
acceptance require their respective unlocked local sender. Already completed
steps are left completed; conflicting pending nominations are refused.

The driver verifies pending-owner refusal, previous-owner refusal after
acceptance, repeated nomination and acceptance, distinct partial ownership states,
unchanged router roles and binding, new-owner configuration, and another paid
transfer through the router. Contract tests also verify that neither adapter owner
can dispatch directly. This development tooling does not activate a live network
deployment or supply live governance acceptance evidence.
