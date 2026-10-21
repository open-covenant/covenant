"""Offline regression checks, NOT an AWS IAM authorization simulator or attestation proof."""

import copy
import fnmatch
import hashlib
import unittest
from render import (
    CONTEXT_KEYS,
    FIELDS,
    FORBIDDEN_ACTIONS,
    POLICY_PHASES,
    SIGNING_PURPOSE,
    key_policy,
    parent_arn,
    pcr3,
    template,
    validate,
)
from provision_request import request


def fixture(phase="service"):
    # Synthetic non-production IDs/measurements. Never used to render a deployment.
    return dict(
        account_id="999988887777",
        region="eu-north-1",
        name="test-covenant-broker",
        key_admin_role_arn="arn:aws:iam::999988887777:role/test-admin",
        provisioning_role_arn="arn:aws:iam::999988887777:role/test-provisioner",
        parent_role_name="test-parent",
        policy_phase=phase,
        approved_pcr0=(
            None
            if phase == "provision_only"
            else hashlib.sha384(("synthetic-test-image-" + phase).encode()).hexdigest()
        ),
        eif_sha256=(
            None
            if phase == "provision_only"
            else hashlib.sha256(("synthetic-test-eif-" + phase).encode()).hexdigest()
        ),
        parent_ami_id="ami-0123456789abcdef0",
        parent_instance_type="m6i.xlarge",
        vpc_id="vpc-0123456789abcdef0",
        private_subnet_ids=["subnet-0123456789abcdef0", "subnet-0123456789abcdef1"],
        private_route_table_ids=["rtb-0123456789abcdef0", "rtb-0123456789abcdef1"],
        s3_prefix_list_id="pl-0123456789abcdef0",
        dynamodb_prefix_list_id="pl-0123456789abcdef1",
        ingress_security_group_id="sg-0123456789abcdef0",
        broker_id="test-broker",
        purpose="test-workspace",
    )


def structural_decision(policy, action, principal, values):
    """Only the generated policy's limited operators; rejects unknown constructs.

    No IAM identities, session policies, SCPs, grants, endpoint policies, resource
    existence, attestation validation, AWS context construction or services here.
    """
    matched = []
    for statement in policy["Statement"]:
        if not any(
            fnmatch.fnmatchcase(action.lower(), a.lower()) for a in statement["Action"]
        ):
            continue
        p = statement["Principal"]
        if p != "*" and p != {"AWS": principal}:
            continue
        passes = True
        for operator, pairs in statement.get("Condition", {}).items():
            for key, expected in pairs.items():
                actual = values.get(key)
                if operator == "Null":
                    ok = (actual is None) == (expected == "true")
                elif operator in ("StringEquals", "ArnEquals"):
                    ok = actual is not None and actual in (
                        expected if isinstance(expected, list) else [expected]
                    )
                elif operator in ("StringNotEquals", "ArnNotEquals"):
                    ok = actual is None or actual not in (
                        expected if isinstance(expected, list) else [expected]
                    )
                elif operator == "StringEqualsIgnoreCase":
                    ok = actual is not None and actual.lower() == expected.lower()
                elif operator == "StringNotEqualsIgnoreCase":
                    ok = actual is None or actual.lower() != expected.lower()
                elif operator == "ForAnyValue:StringNotEquals":
                    ok = actual is not None and any(
                        value not in expected for value in actual
                    )
                elif operator == "ForAllValues:StringEquals":
                    ok = actual is None or all(value in expected for value in actual)
                else:
                    raise AssertionError("unmodelled operator " + operator)
                passes = passes and ok
        if passes:
            matched.append(statement["Effect"])
    return (
        "Deny"
        if "Deny" in matched
        else "Allow" if "Allow" in matched else "ImplicitDeny"
    )


class PolicyTests(unittest.TestCase):
    def setUp(self):
        self.c = fixture()
        self.policy = key_policy(self.c)
        self.parent = parent_arn(self.c)
        self.values = {
            "aws:PrincipalArn": self.parent,
            "aws:SourceVpc": self.c["vpc_id"],
            "kms:RecipientAttestation:ImageSha384": self.c["approved_pcr0"],
            "kms:RecipientAttestation:PCR3": pcr3(self.c),
            "kms:EncryptionContextKeys": CONTEXT_KEYS.copy(),
        }
        self.values.update(
            {"kms:EncryptionContext:" + key: "test-value" for key in CONTEXT_KEYS}
        )
        self.values.update(
            {
                "kms:EncryptionContext:covenant:protocol": "application-key-v1",
                "kms:EncryptionContext:covenant:broker_id": self.c["broker_id"],
                "kms:EncryptionContext:covenant:purpose": self.c["purpose"],
            }
        )

    def decision(self, action="kms:Decrypt", principal=None, values=None):
        return structural_decision(
            self.policy,
            action,
            principal or self.parent,
            self.values if values is None else values,
        )

    def test_approved_shape_allowed(self):
        self.assertEqual(self.decision(), "Allow")

    def signing_values(self):
        return dict(
            self.values,
            **{
                "kms:EncryptionContext:covenant:purpose": SIGNING_PURPOSE,
                "kms:EncryptionContext:covenant:tenant_id": self.c["broker_id"],
                "kms:EncryptionContext:covenant:key_id": SIGNING_PURPOSE,
                "kms:EncryptionContext:covenant:key_version": "1",
            }
        )

    def test_provision_only_unconditionally_denies_decrypt_without_fake_measurements(
        self,
    ):
        policy = key_policy(fixture("provision_only"))
        deny = next(
            s
            for s in policy["Statement"]
            if s["Sid"] == "DenyAllDecryptionDuringProvisioning"
        )
        self.assertEqual(deny["Principal"], "*")
        self.assertNotIn("Condition", deny)
        self.assertFalse(
            any(
                s["Effect"] == "Allow" and "kms:Decrypt" in s["Action"]
                for s in policy["Statement"]
            )
        )
        # An externally added Allow cannot override the explicit bootstrap Deny.
        policy["Statement"].append(
            {
                "Effect": "Allow",
                "Action": ["kms:Decrypt"],
                "Principal": "*",
                "Resource": "*",
            }
        )
        for principal in (
            self.parent,
            self.c["key_admin_role_arn"],
            self.c["provisioning_role_arn"],
        ):
            for context in (self.values, self.signing_values()):
                for attest in (True, False):
                    values = dict(context, **{"aws:PrincipalArn": principal})
                    if not attest:
                        values = {
                            k: v
                            for k, v in values.items()
                            if not k.startswith("kms:RecipientAttestation:")
                        }
                    self.assertEqual(
                        structural_decision(policy, "kms:Decrypt", principal, values),
                        "Deny",
                    )

    def test_bootstrap_allows_only_separate_ciphertext_provisioning(self):
        policy = key_policy(fixture("provision_only"))
        role = self.c["provisioning_role_arn"]
        for context in (self.values, self.signing_values()):
            values = {
                k: v
                for k, v in context.items()
                if not k.startswith("kms:RecipientAttestation:")
            }
            values["aws:PrincipalArn"] = role
            self.assertEqual(
                structural_decision(
                    policy, "kms:GenerateDataKeyWithoutPlaintext", role, values
                ),
                "Allow",
            )
            for action in ("kms:GenerateDataKey", "kms:Encrypt", "kms:Decrypt"):
                self.assertEqual(
                    structural_decision(policy, action, role, values), "Deny"
                )
            values.pop("aws:SourceVpc")
            self.assertEqual(
                structural_decision(
                    policy, "kms:GenerateDataKeyWithoutPlaintext", role, values
                ),
                "Deny",
            )

    def test_enrollment_only_accepts_reserved_signing_context_and_own_measurement(self):
        c = fixture("signing_enrollment")
        self.assertNotEqual(c["approved_pcr0"], self.c["approved_pcr0"])
        policy = key_policy(c)
        for action, role in (
            ("kms:Decrypt", self.parent),
            ("kms:GenerateDataKeyWithoutPlaintext", c["provisioning_role_arn"]),
        ):
            values = dict(
                self.values,
                **{
                    "aws:PrincipalArn": role,
                    "kms:RecipientAttestation:ImageSha384": c["approved_pcr0"],
                }
            )
            self.assertEqual(structural_decision(policy, action, role, values), "Deny")
            values.update(self.signing_values())
            values.update(
                {
                    "aws:PrincipalArn": role,
                    "kms:RecipientAttestation:ImageSha384": c["approved_pcr0"],
                }
            )
            self.assertEqual(structural_decision(policy, action, role, values), "Allow")
            for key in ("tenant_id", "key_id", "key_version"):
                bad = dict(values, **{"kms:EncryptionContext:covenant:" + key: "wrong"})
                self.assertEqual(structural_decision(policy, action, role, bad), "Deny")
        for context_key in (
            "kms:RecipientAttestation:ImageSha384",
            "kms:RecipientAttestation:PCR3",
        ):
            values = self.signing_values()
            values["kms:RecipientAttestation:ImageSha384"] = c["approved_pcr0"]
            for wrong in (None, "0" * 96, self.c["approved_pcr0"]):
                bad = dict(values)
                bad.pop(context_key)
                if wrong is not None:
                    bad[context_key] = wrong
                self.assertEqual(
                    structural_decision(policy, "kms:Decrypt", self.parent, bad), "Deny"
                )
        # Replacing enrollment with service drops the old image, not the root.
        values = self.signing_values()
        values["kms:RecipientAttestation:ImageSha384"] = c["approved_pcr0"]
        self.assertEqual(self.decision(values=values), "Deny")

    def test_all_phases_keep_context_role_and_administrator_guards(self):
        for phase in POLICY_PHASES:
            c = fixture(phase)
            policy = key_policy(c)
            role = c["provisioning_role_arn"]
            values = dict(self.signing_values(), **{"aws:PrincipalArn": role})
            for key in CONTEXT_KEYS:
                for empty in (None, ""):
                    bad = dict(values)
                    bad.pop("kms:EncryptionContext:" + key)
                    if empty is not None:
                        bad["kms:EncryptionContext:" + key] = empty
                    self.assertEqual(
                        structural_decision(
                            policy, "kms:GenerateDataKeyWithoutPlaintext", role, bad
                        ),
                        "Deny",
                    )
            bad = dict(
                values, **{"kms:EncryptionContextKeys": CONTEXT_KEYS + ["extra"]}
            )
            self.assertEqual(
                structural_decision(
                    policy, "kms:GenerateDataKeyWithoutPlaintext", role, bad
                ),
                "Deny",
            )
            for other in (self.parent, c["key_admin_role_arn"]):
                bad = dict(values, **{"aws:PrincipalArn": other})
                self.assertEqual(
                    structural_decision(
                        policy, "kms:GenerateDataKeyWithoutPlaintext", other, bad
                    ),
                    "Deny",
                )
            administrator = c["key_admin_role_arn"]
            self.assertEqual(
                structural_decision(
                    policy,
                    "kms:PutKeyPolicy",
                    administrator,
                    {"aws:PrincipalArn": administrator},
                ),
                "Allow",
            )
            self.assertEqual(
                structural_decision(
                    policy,
                    "kms:PutKeyPolicy",
                    self.parent,
                    {"aws:PrincipalArn": self.parent},
                ),
                "Deny",
            )

    def test_missing_and_wrong_attestation_are_explicit_denies(self):
        for key in (
            "kms:RecipientAttestation:ImageSha384",
            "kms:RecipientAttestation:PCR3",
        ):
            for value in (None, "0" * 96, "f" * 96):
                with self.subTest(key=key, value=value):
                    v = dict(self.values)
                    v.pop(key)
                    if value is not None:
                        v[key] = value
                    self.assertEqual(self.decision(values=v), "Deny")

    def test_every_missing_empty_context_denied(self):
        for key in CONTEXT_KEYS:
            for value in (None, ""):
                with self.subTest(key=key, value=value):
                    v = dict(self.values)
                    v.pop("kms:EncryptionContext:" + key)
                    if value is not None:
                        v["kms:EncryptionContext:" + key] = value
                    self.assertEqual(self.decision(values=v), "Deny")

    def test_extra_context_denied(self):
        v = dict(self.values, **{"kms:EncryptionContextKeys": CONTEXT_KEYS + ["evil"]})
        self.assertEqual(self.decision(values=v), "Deny")

    def test_wrong_namespace_and_missing_vpc_denied(self):
        for key in (
            "kms:EncryptionContext:covenant:protocol",
            "kms:EncryptionContext:covenant:broker_id",
            "kms:EncryptionContext:covenant:purpose",
            "aws:SourceVpc",
        ):
            v = dict(self.values)
            v[key] = "wrong"
            self.assertEqual(self.decision(values=v), "Deny")
            v.pop(key)
            self.assertEqual(self.decision(values=v), "Deny")

    def test_wrong_roles_including_admin_cannot_decrypt(self):
        for role in (
            self.c["key_admin_role_arn"],
            self.c["provisioning_role_arn"],
            "arn:aws:iam::999988887777:root",
        ):
            v = dict(self.values, **{"aws:PrincipalArn": role})
            self.assertEqual(self.decision(principal=role, values=v), "Deny")

    def test_no_plaintext_provisioning_reencrypt_or_grants(self):
        for action in (
            "kms:GenerateDataKey",
            "kms:GenerateDataKeyPair",
            "kms:Encrypt",
            "kms:ReEncryptFrom",
            "kms:ReEncryptTo",
            "kms:CreateGrant",
            "kms:GetParametersForImport",
            "kms:ImportKeyMaterial",
            "kms:ReplicateKey",
            "kms:ScheduleKeyDeletion",
        ):
            for role in (
                self.parent,
                self.c["key_admin_role_arn"],
                self.c["provisioning_role_arn"],
            ):
                with self.subTest(action=action, role=role):
                    self.assertEqual(
                        self.decision(
                            action=action,
                            principal=role,
                            values=dict(self.values, **{"aws:PrincipalArn": role}),
                        ),
                        "Deny",
                    )

    def test_ciphertext_only_provisioning_role(self):
        role = self.c["provisioning_role_arn"]
        v = {
            k: value
            for k, value in self.values.items()
            if not k.startswith("kms:RecipientAttestation:")
        }
        v["aws:PrincipalArn"] = role
        self.assertEqual(
            self.decision("kms:GenerateDataKeyWithoutPlaintext", role, v), "Allow"
        )
        self.assertEqual(self.decision("kms:GenerateDataKeyWithoutPlaintext"), "Deny")

    def test_admin_can_change_policy_and_is_explicit_trust(self):
        role = self.c["key_admin_role_arn"]
        v = {"aws:PrincipalArn": role}
        self.assertEqual(self.decision("kms:PutKeyPolicy", role, v), "Allow")
        self.assertEqual(self.decision("kms:PutKeyPolicy"), "Deny")

    def test_reserved_signing_scope(self):
        v = dict(self.values)
        v.update(
            {
                "kms:EncryptionContext:covenant:purpose": "broker-response-signing",
                "kms:EncryptionContext:covenant:tenant_id": self.c["broker_id"],
                "kms:EncryptionContext:covenant:key_id": "broker-response-signing",
                "kms:EncryptionContext:covenant:key_version": "1",
            }
        )
        self.assertEqual(self.decision(values=v), "Allow")
        for key in ("tenant_id", "key_id", "key_version"):
            bad = dict(v)
            bad["kms:EncryptionContext:covenant:" + key] = "wrong"
            self.assertEqual(self.decision(values=bad), "Deny")

    def test_provision_request_matches_context(self):
        value = request(
            self.c,
            {
                "tenant_id": "tenant",
                "key_id": "workspace",
                "key_version": 1,
                "owner_pubkey_b58": "1" * 32,
                "kms_key_arn": "arn:aws:kms:eu-north-1:999988887777:key/12345678-1234-1234-1234-123456789abc",
            },
        )
        self.assertEqual(set(value["EncryptionContext"]), set(CONTEXT_KEYS))
        self.assertEqual(value["NumberOfBytes"], 32)
        self.assertNotIn("Plaintext", value)

    def test_bad_config_fails_closed(self):
        for field, bad in (
            ("account_id", "123456789012"),
            ("approved_pcr0", "0" * 96),
            ("eif_sha256", "f" * 64),
            ("parent_ami_id", None),
            ("key_admin_role_arn", self.parent),
            ("provisioning_role_arn", self.parent),
            ("private_subnet_ids", ["subnet-0123456789abcdef0"]),
            ("purpose", "*"),
            ("purpose", SIGNING_PURPOSE),
            ("policy_phase", None),
            ("policy_phase", "unknown"),
        ):
            with self.subTest(field=field):
                c = dict(self.c)
                c[field] = bad
                with self.assertRaises(ValueError):
                    validate(c)

    def test_phase_measurements_cannot_be_omitted_or_smuggled_into_bootstrap(self):
        for phase in POLICY_PHASES:
            for field in ("approved_pcr0", "eif_sha256"):
                for invalid in (
                    None,
                    "0" * (96 if field == "approved_pcr0" else 64),
                    self.c[field],
                ):
                    if phase != "provision_only" and invalid == self.c[field]:
                        continue
                    if phase == "provision_only" and invalid is None:
                        continue
                    c = fixture(phase)
                    c[field] = invalid
                    with self.subTest(phase=phase, field=field, invalid=invalid):
                        with self.assertRaises(ValueError):
                            validate(c)
        c = fixture()
        c.pop("policy_phase")
        with self.assertRaises(ValueError):
            validate(c)

    def test_reserved_seed_request_is_explicit_and_works_before_enrollment_build(self):
        metadata = {
            "tenant_id": self.c["broker_id"],
            "key_id": SIGNING_PURPOSE,
            "key_version": 1,
            "owner_pubkey_b58": "1" * 32,
            "kms_key_arn": "arn:aws:kms:eu-north-1:999988887777:key/12345678-1234-1234-1234-123456789abc",
        }
        for phase in POLICY_PHASES:
            value = request(fixture(phase), metadata, key_kind="response-signing")
            self.assertEqual(
                value["EncryptionContext"]["covenant:purpose"], SIGNING_PURPOSE
            )
            for field in ("tenant_id", "key_id", "key_version"):
                bad = dict(
                    metadata, **{field: 2 if field == "key_version" else "wrong"}
                )
                with self.assertRaises(ValueError):
                    request(fixture(phase), bad, key_kind="response-signing")
            with self.assertRaises(ValueError):
                request(fixture(phase), metadata)
        application = dict(metadata, tenant_id="tenant", key_id="application")
        with self.assertRaises(ValueError):
            request(fixture("signing_enrollment"), application)
        for version in (True, 0, 9007199254740992):
            with self.assertRaises(ValueError):
                request(self.c, dict(application, key_version=version))

    def test_pcr3_uses_null_prefix(self):
        self.assertEqual(
            pcr3(self.c), hashlib.sha384(b"\0" * 48 + self.parent.encode()).hexdigest()
        )
        self.assertNotEqual(
            pcr3(self.c), hashlib.sha384(self.parent.encode()).hexdigest()
        )

    def test_template_dormant_retained_and_private(self):
        r = template(self.c)["Resources"]
        self.assertEqual(r["DormantParents"]["Properties"]["MaxSize"], "0")
        self.assertEqual(r["DormantParents"]["Properties"]["DesiredCapacity"], "0")
        for name in ("WrappingKey", "Ciphertexts", "PolicyLedger"):
            self.assertEqual(r[name]["DeletionPolicy"], "Retain")
            self.assertEqual(r[name]["UpdateReplacePolicy"], "Retain")
        launch = r["ParentLaunchTemplate"]["Properties"]["LaunchTemplateData"]
        self.assertFalse(launch["NetworkInterfaces"][0]["AssociatePublicIpAddress"])
        self.assertTrue(launch["EnclaveOptions"]["Enabled"])
        self.assertEqual(launch["MetadataOptions"]["HttpTokens"], "required")
        self.assertNotIn("UserData", launch)
        self.assertTrue(r["PolicyLedger"]["Properties"]["DeletionProtectionEnabled"])
        self.assertNotIn("0.0.0.0/0", str(r["ParentGroup"]))

    def test_every_phase_remains_dormant_without_null_tags_or_resource_replacement(
        self,
    ):
        reference_resources = template(self.c)["Resources"]
        reference_ids = set(reference_resources)
        for phase in POLICY_PHASES:
            t = template(fixture(phase))
            self.assertEqual(set(t["Resources"]), reference_ids)
            self.assertEqual(t["Metadata"]["PolicyPhase"], phase)
            for size in ("MinSize", "MaxSize", "DesiredCapacity"):
                self.assertEqual(
                    t["Resources"]["DormantParents"]["Properties"][size], "0"
                )
            tags = t["Resources"]["ParentLaunchTemplate"]["Properties"][
                "LaunchTemplateData"
            ]["TagSpecifications"][0]["Tags"]
            self.assertTrue(all(isinstance(tag["Value"], str) for tag in tags))
            if phase == "provision_only":
                self.assertNotIn("ApprovedPcr0", t["Metadata"])
                self.assertNotIn("ApprovedEifSha256", t["Metadata"])
                self.assertFalse(any(tag["Key"] == "CovenantEifSha256" for tag in tags))
            for resource in ("WrappingKey", "Ciphertexts", "PolicyLedger"):
                self.assertEqual(t["Resources"][resource]["DeletionPolicy"], "Retain")
                self.assertEqual(
                    t["Resources"][resource]["UpdateReplacePolicy"], "Retain"
                )
                before = copy.deepcopy(reference_resources[resource])
                after = copy.deepcopy(t["Resources"][resource])
                if resource == "WrappingKey":
                    before["Properties"].pop("KeyPolicy")
                    after["Properties"].pop("KeyPolicy")
                self.assertEqual(after, before)


if __name__ == "__main__":
    unittest.main()
