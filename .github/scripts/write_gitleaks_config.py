"""Retain default secret rules while recognizing narrowly reviewed public fixtures."""

import base64
import binascii
from pathlib import Path
import re
import sys

PUBLIC_ACCOUNT_FIXTURES = (
    "GAAZI4TCR3TY5OJHCTJC2A4QSY6CJWJH5IAJTGKIN2ER7LBNVKOCCWN7",
    "GA5ZYIIVYDX5GRF2BKEQD2Y47ZCPG3HX3PUUNPVQ5ZECB57MJG7RLVNR",
)
FIXTURE_PATH = "script/stellar/test/reconcile-axelar.test.mjs"
PUBLIC_CONTRACT_FIXTURES = (
    "CCW67TSZV3SSS2HXMBQ5JFGCKJNXKZM7UQUWUZPUTHXSTZLEO7SJMI75",
)
SYNTHETIC_CONTRACT_FIXTURES = (
    "CGAS7PQXN7P55U25D2BWWX7AUKC7E3AOP7XGQLB7IYY3Z3QWVQO32GAS",
)
AUTH_SNAPSHOT_FIXTURE_PATHS = (
    "script/stellar/test/fixtures/auth-snapshots/changed-amount/snapshot.json",
    "script/stellar/test/fixtures/auth-snapshots/changed-gas-recipient/snapshot.json",
    "script/stellar/test/fixtures/auth-snapshots/changed-spender/snapshot.json",
    "script/stellar/test/fixtures/auth-snapshots/unexpected-sub-invocation/snapshot.json",
    "script/stellar/test/fixtures/auth-snapshots/changed-fee-recipient/snapshot.json",
    "script/stellar/test/fixtures/auth-snapshots/valid-paid-dispatch/snapshot.json",
    "script/stellar/test/inspect-axelar-auth.test.mjs",
)


def verify_public_account(value):
    """Version 48 denotes a public Ed25519 account; signing seeds use version 144."""
    if not isinstance(value, str) or not re.fullmatch(r"G[A-Z2-7]{55}", value):
        raise ValueError("Reviewed fixture must be a canonical Stellar public account")
    decoded = base64.b32decode(value)
    if (len(decoded) != 35 or decoded[0] != 48
            or decoded[-2:] != binascii.crc_hqx(decoded[:-2], 0).to_bytes(2, "little")):
        raise ValueError("Reviewed public account fixture has an invalid version or checksum")


def verify_public_contract(value):
    """Version 16 denotes a public contract; checksum validation stays mandatory."""
    if not isinstance(value, str) or not re.fullmatch(r"C[A-Z2-7]{55}", value):
        raise ValueError("Reviewed fixture must be a canonical Stellar public contract")
    decoded = base64.b32decode(value)
    if (len(decoded) != 35 or decoded[0] != 16
            or decoded[-2:] != binascii.crc_hqx(decoded[:-2], 0).to_bytes(2, "little")):
        raise ValueError("Reviewed public contract fixture has an invalid version or checksum")


def verify_synthetic_contract_fixture(value):
    """Admit only the historical dummy gas ID, which intentionally has no valid checksum."""
    if value != "CGAS7PQXN7P55U25D2BWWX7AUKC7E3AOP7XGQLB7IYY3Z3QWVQO32GAS":
        raise ValueError("Only the exact reviewed synthetic gas contract fixture is allowed")


def configuration():
    for value in PUBLIC_ACCOUNT_FIXTURES:
        verify_public_account(value)
    for value in PUBLIC_CONTRACT_FIXTURES:
        verify_public_contract(value)
    for value in SYNTHETIC_CONTRACT_FIXTURES:
        verify_synthetic_contract_fixture(value)
    # Extend this rule without replacing its regex, entropy threshold or default allowlists.
    # AND is essential: neither the path alone nor the public value in another file is exempt.
    values = "|".join(PUBLIC_ACCOUNT_FIXTURES)
    path = re.escape(FIXTURE_PATH)
    contract_values = "|".join(PUBLIC_CONTRACT_FIXTURES + SYNTHETIC_CONTRACT_FIXTURES)
    contract_paths = "|".join(re.escape(path) for path in AUTH_SNAPSHOT_FIXTURE_PATHS)
    return f"""[extend]
useDefault = true

[[rules]]
id = "generic-api-key"
[[rules.allowlists]]
description = "Two checksum-verified public Stellar account fixtures, never signing seeds"
condition = "AND"
regexTarget = "secret"
regexes = ['''^({values})$''']
paths = ['''^{path}$''']

[[rules.allowlists]]
description = "Exact reviewed public token and synthetic gas contract IDs in paid-dispatch fixtures"
condition = "AND"
regexTarget = "secret"
regexes = ['''^({contract_values})$''']
paths = ['''^({contract_paths})$''']
"""


if __name__ == "__main__":
    Path(sys.argv[1]).write_text(configuration())
