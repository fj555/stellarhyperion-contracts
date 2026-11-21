"""Exercise the pinned scanner, including real secret shapes beside reviewed public fixtures."""

import base64
import binascii
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import tomllib
import unittest
from unittest.mock import patch

import write_gitleaks_config as config


def strkey(version, payload):
    raw = bytes([version]) + payload
    return base64.b32encode(raw + binascii.crc_hqx(raw, 0).to_bytes(2, "little")).decode()


class GitleaksConfigurationTests(unittest.TestCase):
    def test_only_exact_public_values_and_path_extend_one_default_rule(self):
        value = tomllib.loads(config.configuration())
        self.assertEqual(value["extend"], {"useDefault": True})
        self.assertEqual(len(value["rules"]), 1)
        rule = value["rules"][0]
        self.assertEqual(set(rule), {"id", "allowlists"})
        self.assertEqual(rule["id"], "generic-api-key")
        self.assertEqual(len(rule["allowlists"]), 2)
        for allowlist in rule["allowlists"]:
            self.assertEqual(allowlist["condition"], "AND")
            self.assertEqual(allowlist["regexTarget"], "secret")

    def test_signing_seed_and_invalid_public_checksum_cannot_enter_config(self):
        invalid = config.PUBLIC_ACCOUNT_FIXTURES[0][:-1] + "A"
        seed = strkey(144, bytes(range(1, 33)))
        for candidate in (seed, invalid, strkey(16, bytes(range(1, 33)))):
            with self.subTest(candidate_type=candidate[0]), self.assertRaises(ValueError):
                with patch.object(config, "PUBLIC_ACCOUNT_FIXTURES", (candidate,)):
                    config.configuration()

    def test_contract_validator_rejects_seed_accounts_and_invalid_checksums(self):
        public = config.PUBLIC_CONTRACT_FIXTURES[0]
        config.verify_public_contract(public)
        for candidate in (
            strkey(144, bytes(range(1, 33))), config.PUBLIC_ACCOUNT_FIXTURES[0],
            public[:-1] + "A", config.SYNTHETIC_CONTRACT_FIXTURES[0], None,
        ):
            with self.subTest(candidate=candidate), self.assertRaises(ValueError):
                with patch.object(config, "PUBLIC_CONTRACT_FIXTURES", (candidate,)):
                    config.configuration()

    def test_synthetic_exception_cannot_admit_other_identifiers_or_keys(self):
        dummy = config.SYNTHETIC_CONTRACT_FIXTURES[0]
        for candidate in (
            dummy[:-1] + "A", config.PUBLIC_CONTRACT_FIXTURES[0],
            strkey(144, bytes(range(1, 33))), None,
        ):
            with self.subTest(candidate=candidate), self.assertRaises(ValueError):
                with patch.object(config, "SYNTHETIC_CONTRACT_FIXTURES", (candidate,)):
                    config.configuration()

    def scan(self, entries):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            source.mkdir()
            configuration = root / "gitleaks.toml"
            configuration.write_text(config.configuration())
            report = root / "report.json"
            for path, content in entries.items():
                target = source / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text(content)
            for arguments in (
                ("init", "--quiet"), ("config", "user.name", "dotmantissa"),
                ("config", "user.email", "negativemantissa@gmail.com"),
                ("add", "."), ("commit", "--quiet", "-m", "Exercise secret scanner boundaries"),
            ):
                subprocess.run(["git", *arguments], cwd=source, check=True, capture_output=True)
            result = subprocess.run([
                "gitleaks", "git", "--redact", "--no-banner", "--ignore-gitleaks-allow",
                "--config", str(configuration), "--gitleaks-ignore-path", str(root / "absent"),
                "--report-format", "json", "--report-path", str(report), "--log-opts=--max-count=1 HEAD", ".",
            ], cwd=source, capture_output=True, text=True, check=False)
            self.assertIn(result.returncode, (0, 1), "Pinned secret scanner failed to run")
            findings = json.loads(report.read_text())
            self.assertEqual(result.returncode, int(bool(findings)))
            return findings

    def test_both_reviewed_public_fixtures_are_recognized(self):
        for public in config.PUBLIC_ACCOUNT_FIXTURES:
            with self.subTest(public=public):
                self.assertEqual(self.scan({
                    config.FIXTURE_PATH: f'const strkey = "{public}";\n',
                }), [])

    def test_real_signing_seed_and_generic_token_remain_detected_in_fixture_path(self):
        seed = strkey(144, bytes(range(1, 33)))
        generic = hashlib.sha256(b"synthetic scanner regression token").hexdigest()
        findings = self.scan({config.FIXTURE_PATH:
            f'const strkey = "{seed}";\nconst api_token = "{generic}";\n'})
        self.assertEqual(len(findings), 2)
        self.assertTrue(all(finding["RuleID"] == "generic-api-key" for finding in findings))

    def test_same_public_value_elsewhere_and_unreviewed_value_still_findings(self):
        findings = self.scan({
            "unreviewed.mjs": f'const strkey = "{config.PUBLIC_ACCOUNT_FIXTURES[0]}";\n',
            config.FIXTURE_PATH: f'const strkey = "{strkey(48, bytes(range(1, 33)))}";\n',
        })
        self.assertEqual(len(findings), 2)

    def test_real_token_beside_public_fixture_remains_detected(self):
        generic = hashlib.sha256(b"synthetic scanner regression token").hexdigest()
        findings = self.scan({config.FIXTURE_PATH:
            f'const strkey = "{config.PUBLIC_ACCOUNT_FIXTURES[0]}";\n'
            f'const api_token = "{generic}";\n'})
        self.assertEqual(len(findings), 1)
        self.assertEqual(findings[0]["StartLine"], 2)

    def test_reviewed_contract_values_recognized_in_all_exact_snapshot_paths(self):
        public = config.PUBLIC_CONTRACT_FIXTURES[0]
        dummy = config.SYNTHETIC_CONTRACT_FIXTURES[0]
        self.assertEqual(self.scan({path:
            f'const token = "{public}";\nconst gasToken = "{dummy}";\n'
            for path in config.AUTH_SNAPSHOT_FIXTURE_PATHS
        }), [])

    def test_reviewed_contract_values_detected_in_other_and_nearby_paths(self):
        public = config.PUBLIC_CONTRACT_FIXTURES[0]
        dummy = config.SYNTHETIC_CONTRACT_FIXTURES[0]
        allowed = config.AUTH_SNAPSHOT_FIXTURE_PATHS[0]
        paths = (
            "unreviewed.mjs", allowed + ".bak", "prefix/" + allowed,
            "script/stellar/test/fixtures/auth-snapshots/unreviewed/snapshot.json",
        )
        findings = self.scan({path:
            f'const token = "{public}";\nconst gasToken = "{dummy}";\n'
            for path in paths
        })
        self.assertEqual(len(findings), 2 * len(paths))

    def test_unreviewed_and_corrupt_contracts_detected_in_reviewed_path(self):
        public = config.PUBLIC_CONTRACT_FIXTURES[0]
        dummy = config.SYNTHETIC_CONTRACT_FIXTURES[0]
        invalid_public = public[:-1] + "A"
        invalid_dummy = dummy[:-1] + "A"
        findings = self.scan({config.AUTH_SNAPSHOT_FIXTURE_PATHS[0]:
            f'const token = "{strkey(16, bytes(range(1, 33)))}";\n'
            f'const api_token = "{invalid_public}";\n'
            f'const gasToken = "{invalid_dummy}";\n'})
        self.assertEqual(len(findings), 3)

    def test_secrets_beside_reviewed_contracts_detected_in_all_snapshot_paths(self):
        public = config.PUBLIC_CONTRACT_FIXTURES[0]
        dummy = config.SYNTHETIC_CONTRACT_FIXTURES[0]
        seed = strkey(144, bytes(range(1, 33)))
        generic = hashlib.sha256(b"synthetic scanner regression token").hexdigest()
        findings = self.scan({path:
            f'const token = "{public}";\nconst gasToken = "{dummy}";\n'
            f'const strkey = "{seed}";\nconst api_token = "{generic}";\n'
            for path in config.AUTH_SNAPSHOT_FIXTURE_PATHS
        })
        self.assertEqual(len(findings), 2 * len(config.AUTH_SNAPSHOT_FIXTURE_PATHS))
        self.assertTrue(all(finding["StartLine"] in (3, 4) for finding in findings))
        self.assertTrue(all(finding["RuleID"] == "generic-api-key" for finding in findings))


if __name__ == "__main__":
    unittest.main()
