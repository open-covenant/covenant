#!/usr/bin/env python3
"""Offline-only CloudFormation/policy renderer. Never contacts AWS or creates keys."""
import argparse
import hashlib
import json
import re
from pathlib import Path

CONTEXT_KEYS = [
    "covenant:" + x
    for x in (
        "broker_id",
        "tenant_id",
        "key_id",
        "key_version",
        "purpose",
        "owner_pubkey_b58",
        "protocol",
    )
]
FIELDS = set(
    "account_id region name key_admin_role_arn provisioning_role_arn parent_role_name policy_phase approved_pcr0 eif_sha256 parent_ami_id parent_instance_type vpc_id private_subnet_ids private_route_table_ids s3_prefix_list_id dynamodb_prefix_list_id ingress_security_group_id broker_id purpose".split()
)
POLICY_PHASES = ("provision_only", "signing_enrollment", "service")
SIGNING_PURPOSE = "broker-response-signing"
USE_ACTIONS = ["kms:Decrypt", "kms:GenerateDataKeyWithoutPlaintext"]
FORBIDDEN_ACTIONS = [
    "kms:Encrypt",
    "kms:ReEncrypt*",
    "kms:GenerateDataKey",
    "kms:GenerateDataKeyPair*",
    "kms:DeriveSharedSecret",
    "kms:GenerateMac",
    "kms:Sign",
    "kms:CreateGrant",
    "kms:ImportKeyMaterial",
    "kms:GetParametersForImport",
    "kms:DeleteImportedKeyMaterial",
    "kms:ReplicateKey",
    "kms:UpdatePrimaryRegion",
]
ADMIN_ACTIONS = [
    "kms:DescribeKey",
    "kms:GetKeyPolicy",
    "kms:PutKeyPolicy",
    "kms:EnableKey",
    "kms:DisableKey",
    "kms:GetKeyRotationStatus",
    "kms:EnableKeyRotation",
    "kms:DisableKeyRotation",
    "kms:RotateKeyOnDemand",
    "kms:ListResourceTags",
    "kms:TagResource",
    "kms:UntagResource",
    "kms:ListGrants",
    "kms:RevokeGrant",
    "kms:CancelKeyDeletion",
]


def require(pattern, value, name):
    if not isinstance(value, str) or re.fullmatch(pattern, value) is None:
        raise ValueError("invalid or missing " + name)
    return value


def validate(c):
    if not isinstance(c, dict) or set(c) != FIELDS:
        raise ValueError("configuration must contain exactly the documented fields")
    require(r"[0-9]{12}", c["account_id"], "account_id")
    if c["account_id"] in {"000000000000", "123456789012", "111122223333"}:
        raise ValueError("example account IDs are forbidden")
    require(r"[a-z]{2}-[a-z]+-[1-9][0-9]*", c["region"], "commercial AWS region")
    require(r"[a-z][a-z0-9-]{2,39}", c["name"], "name")
    if len(c["name"] + "-ciphertexts-" + c["account_id"] + "-" + c["region"]) > 63:
        raise ValueError("deployment name makes bucket name exceed 63 characters")
    require(r"[A-Za-z0-9+=,.@_-]{1,64}", c["parent_role_name"], "parent_role_name")
    role_pattern = r"arn:aws:iam::" + c["account_id"] + r":role/[A-Za-z0-9+=,.@_/-]+"
    for field in ("key_admin_role_arn", "provisioning_role_arn"):
        require(role_pattern, c[field], field)
    parent = parent_arn(c)
    if len({parent, c["key_admin_role_arn"], c["provisioning_role_arn"]}) != 3:
        raise ValueError("administrator, provisioner and parent roles must be separate")
    if c["policy_phase"] not in POLICY_PHASES:
        raise ValueError("invalid or missing policy_phase")
    for field, length in (("approved_pcr0", 96), ("eif_sha256", 64)):
        if c["policy_phase"] == "provision_only":
            if c[field] is not None:
                raise ValueError(field + " must be null in provision_only")
            continue
        require(r"[0-9a-f]{" + str(length) + "}", c[field], field)
        if len(set(c[field])) == 1:
            raise ValueError(
                field + " must be a reviewed measurement, not a debug/placeholder value"
            )
    for field, prefix in (
        ("parent_ami_id", "ami"),
        ("vpc_id", "vpc"),
        ("ingress_security_group_id", "sg"),
        ("s3_prefix_list_id", "pl"),
        ("dynamodb_prefix_list_id", "pl"),
    ):
        require(prefix + r"-[0-9a-f]{8,17}", c[field], field)
    require(
        r"[a-z][a-z0-9]*\.[a-z0-9]+", c["parent_instance_type"], "parent_instance_type"
    )
    for field, prefix in (
        ("private_subnet_ids", "subnet"),
        ("private_route_table_ids", "rtb"),
    ):
        values = c[field]
        if (
            not isinstance(values, list)
            or len(values) < 2
            or len(values) > 3
            or len(set(values)) != len(values)
        ):
            raise ValueError(field + " requires two or three distinct IDs")
        for value in values:
            require(prefix + r"-[0-9a-f]{8,17}", value, field)
    for field in ("broker_id", "purpose"):
        require(r"[A-Za-z0-9._:/-]{1,128}", c[field], field)
    if c["purpose"] == SIGNING_PURPOSE:
        raise ValueError(
            "purpose must name the application, not the reserved signing context"
        )
    return c


def parent_arn(c):
    return "arn:aws:iam::" + c["account_id"] + ":role/" + c["parent_role_name"]


def pcr3(c):
    return hashlib.sha384(bytes(48) + parent_arn(c).encode()).hexdigest()


def statement(sid, effect, actions, principal="*", condition=None):
    result = {
        "Sid": sid,
        "Effect": effect,
        "Principal": principal,
        "Action": actions,
        "Resource": "*",
    }
    if condition:
        result["Condition"] = condition
    return result


def key_policy(c):
    validate(c)
    fixed = {
        "covenant:protocol": "application-key-v1",
        "covenant:broker_id": c["broker_id"],
        "covenant:purpose": (
            [SIGNING_PURPOSE]
            if c["policy_phase"] == "signing_enrollment"
            else sorted({c["purpose"], SIGNING_PURPOSE})
        ),
    }
    statements = [
        statement(
            "AdministerWithoutKeyUse",
            "Allow",
            ADMIN_ACTIONS,
            {"AWS": c["key_admin_role_arn"]},
        ),
        statement("DenyAlternateCryptoAndDelegation", "Deny", FORBIDDEN_ACTIONS),
        statement("DenyDeletingRoot", "Deny", ["kms:ScheduleKeyDeletion"]),
        statement(
            "DenyOtherPolicyAdministrators",
            "Deny",
            ["kms:PutKeyPolicy"],
            condition={"ArnNotEquals": {"aws:PrincipalArn": c["key_admin_role_arn"]}},
        ),
        statement(
            "DenyOtherDecryptPrincipals",
            "Deny",
            ["kms:Decrypt"],
            condition={"ArnNotEquals": {"aws:PrincipalArn": parent_arn(c)}},
        ),
        statement(
            "DenyOtherProvisioners",
            "Deny",
            ["kms:GenerateDataKeyWithoutPlaintext"],
            condition={
                "ArnNotEquals": {"aws:PrincipalArn": c["provisioning_role_arn"]}
            },
        ),
        statement(
            "DenyUseOutsidePrivateVpc",
            "Deny",
            USE_ACTIONS,
            condition={"StringNotEquals": {"aws:SourceVpc": c["vpc_id"]}},
        ),
    ]
    if c["policy_phase"] == "provision_only":
        statements.append(
            statement("DenyAllDecryptionDuringProvisioning", "Deny", ["kms:Decrypt"])
        )
    attestation_pins = (
        ()
        if c["policy_phase"] == "provision_only"
        else (
            ("Image", "kms:RecipientAttestation:ImageSha384", c["approved_pcr0"]),
            ("ParentRole", "kms:RecipientAttestation:PCR3", pcr3(c)),
        )
    )
    for suffix, key, expected in attestation_pins:
        statements += [
            statement(
                "DenyMissing" + suffix,
                "Deny",
                ["kms:Decrypt"],
                condition={"Null": {key: "true"}},
            ),
            statement(
                "DenyWrong" + suffix,
                "Deny",
                ["kms:Decrypt"],
                condition={"StringNotEqualsIgnoreCase": {key: expected}},
            ),
        ]
    for index, key in enumerate(CONTEXT_KEYS):
        policy_key = "kms:EncryptionContext:" + key
        statements += [
            statement(
                "DenyMissingContext" + str(index),
                "Deny",
                USE_ACTIONS,
                condition={"Null": {policy_key: "true"}},
            ),
            statement(
                "DenyEmptyContext" + str(index),
                "Deny",
                USE_ACTIONS,
                condition={"StringEquals": {policy_key: ""}},
            ),
        ]
    for index, (key, value) in enumerate(fixed.items()):
        statements.append(
            statement(
                "DenyWrongNamespace" + str(index),
                "Deny",
                USE_ACTIONS,
                condition={"StringNotEquals": {"kms:EncryptionContext:" + key: value}},
            )
        )
    for index, (key, value) in enumerate(
        {
            "covenant:tenant_id": c["broker_id"],
            "covenant:key_id": "broker-response-signing",
            "covenant:key_version": "1",
        }.items()
    ):
        statements.append(
            statement(
                "DenyWrongSigningContext" + str(index),
                "Deny",
                USE_ACTIONS,
                condition={
                    "StringEquals": {
                        "kms:EncryptionContext:covenant:purpose": "broker-response-signing"
                    },
                    "StringNotEquals": {"kms:EncryptionContext:" + key: value},
                },
            )
        )
    statements.append(
        statement(
            "DenyExtraContext",
            "Deny",
            USE_ACTIONS,
            condition={
                "ForAnyValue:StringNotEquals": {
                    "kms:EncryptionContextKeys": CONTEXT_KEYS
                }
            },
        )
    )
    context_condition = {
        "StringEquals": {"kms:EncryptionContext:" + k: v for k, v in fixed.items()},
        "ForAllValues:StringEquals": {"kms:EncryptionContextKeys": CONTEXT_KEYS},
        "Null": {"kms:EncryptionContext:" + key: "false" for key in CONTEXT_KEYS},
    }
    if c["policy_phase"] != "provision_only":
        decrypt_condition = dict(context_condition)
        decrypt_condition["StringEqualsIgnoreCase"] = {
            "kms:RecipientAttestation:ImageSha384": c["approved_pcr0"],
            "kms:RecipientAttestation:PCR3": pcr3(c),
        }
        statements.append(
            statement(
                "AttestedBrokerDecrypt",
                "Allow",
                ["kms:Decrypt"],
                {"AWS": parent_arn(c)},
                decrypt_condition,
            )
        )
    statements += [
        statement(
            "CiphertextOnlyProvisioning",
            "Allow",
            ["kms:GenerateDataKeyWithoutPlaintext"],
            {"AWS": c["provisioning_role_arn"]},
            context_condition,
        ),
    ]
    return {"Version": "2012-10-17", "Statement": statements}


def ref(name):
    return {"Ref": name}


def arn(name):
    return {"Fn::GetAtt": [name, "Arn"]}


def template(c):
    validate(c)
    resources = {}

    def add(name, kind, props, retained=False):
        item = {"Type": kind, "Properties": props}
        if retained:
            item.update(DeletionPolicy="Retain", UpdateReplacePolicy="Retain")
        resources[name] = item

    add(
        "WrappingKey",
        "AWS::KMS::Key",
        {
            "Description": "Covenant application key wrapping; attested decryption only",
            "KeySpec": "SYMMETRIC_DEFAULT",
            "KeyUsage": "ENCRYPT_DECRYPT",
            "Origin": "AWS_KMS",
            "MultiRegion": False,
            "EnableKeyRotation": True,
            "BypassPolicyLockoutSafetyCheck": False,
            "KeyPolicy": key_policy(c),
        },
        True,
    )
    resources["WrappingKey"]["DependsOn"] = "ParentRole"
    add(
        "Ciphertexts",
        "AWS::S3::Bucket",
        {
            "BucketName": c["name"]
            + "-ciphertexts-"
            + c["account_id"]
            + "-"
            + c["region"],
            "VersioningConfiguration": {"Status": "Enabled"},
            "ObjectLockEnabled": True,
            "BucketEncryption": {
                "ServerSideEncryptionConfiguration": [
                    {"ServerSideEncryptionByDefault": {"SSEAlgorithm": "AES256"}}
                ]
            },
            "OwnershipControls": {
                "Rules": [{"ObjectOwnership": "BucketOwnerEnforced"}]
            },
            "PublicAccessBlockConfiguration": {
                x: True
                for x in (
                    "BlockPublicAcls",
                    "BlockPublicPolicy",
                    "IgnorePublicAcls",
                    "RestrictPublicBuckets",
                )
            },
        },
        True,
    )
    bucket_objects = {"Fn::Join": ["", [arn("Ciphertexts"), "/*"]]}
    add(
        "CiphertextPolicy",
        "AWS::S3::BucketPolicy",
        {
            "Bucket": ref("Ciphertexts"),
            "PolicyDocument": {
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Sid": "TlsOnly",
                        "Effect": "Deny",
                        "Principal": "*",
                        "Action": "s3:*",
                        "Resource": [arn("Ciphertexts"), bucket_objects],
                        "Condition": {"Bool": {"aws:SecureTransport": "false"}},
                    },
                    {
                        "Sid": "NoParentMutation",
                        "Effect": "Deny",
                        "Principal": {"AWS": parent_arn(c)},
                        "Action": [
                            "s3:PutObject*",
                            "s3:DeleteObject*",
                            "s3:BypassGovernanceRetention",
                        ],
                        "Resource": bucket_objects,
                    },
                ],
            },
        },
    )
    resources["CiphertextPolicy"]["DependsOn"] = "ParentRole"
    add(
        "PolicyLedger",
        "AWS::DynamoDB::Table",
        {
            "BillingMode": "PAY_PER_REQUEST",
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "PointInTimeRecoverySpecification": {"PointInTimeRecoveryEnabled": True},
            "DeletionProtectionEnabled": True,
            "SSESpecification": {"SSEEnabled": True},
            "OnDemandThroughput": {
                "MaxReadRequestUnits": 10,
                "MaxWriteRequestUnits": 10,
            },
        },
        True,
    )
    add(
        "ParentRole",
        "AWS::IAM::Role",
        {
            "RoleName": c["parent_role_name"],
            "AssumeRolePolicyDocument": {
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Principal": {"Service": "ec2.amazonaws.com"},
                        "Action": "sts:AssumeRole",
                    }
                ],
            },
        },
    )
    add(
        "ParentPolicy",
        "AWS::IAM::Policy",
        {
            "PolicyName": "AttestedBrokerTransport",
            "Roles": [ref("ParentRole")],
            "PolicyDocument": {
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Action": ["kms:Decrypt"],
                        "Resource": arn("WrappingKey"),
                    },
                    {
                        "Effect": "Allow",
                        "Action": ["s3:GetObject", "s3:GetObjectVersion"],
                        "Resource": bucket_objects,
                    },
                    {
                        "Effect": "Allow",
                        "Action": ["dynamodb:GetItem"],
                        "Resource": arn("PolicyLedger"),
                    },
                ],
            },
        },
    )
    add("ParentProfile", "AWS::IAM::InstanceProfile", {"Roles": [ref("ParentRole")]})
    # Explicit egress rules suppress EC2's allow-all default; no public ingress or NAT.
    add(
        "EndpointGroup",
        "AWS::EC2::SecurityGroup",
        {
            "GroupDescription": "Private KMS HTTPS from broker parents",
            "VpcId": c["vpc_id"],
            "SecurityGroupEgress": [
                {
                    "IpProtocol": "tcp",
                    "FromPort": 443,
                    "ToPort": 443,
                    "CidrIp": "127.0.0.1/32",
                }
            ],
        },
    )
    add(
        "ParentGroup",
        "AWS::EC2::SecurityGroup",
        {
            "GroupDescription": "Private broker transport only",
            "VpcId": c["vpc_id"],
            "SecurityGroupIngress": [
                {
                    "IpProtocol": "tcp",
                    "FromPort": 443,
                    "ToPort": 443,
                    "SourceSecurityGroupId": c["ingress_security_group_id"],
                }
            ],
            "SecurityGroupEgress": [
                {
                    "IpProtocol": "tcp",
                    "FromPort": 443,
                    "ToPort": 443,
                    "DestinationSecurityGroupId": ref("EndpointGroup"),
                },
                {
                    "IpProtocol": "tcp",
                    "FromPort": 443,
                    "ToPort": 443,
                    "DestinationPrefixListId": c["s3_prefix_list_id"],
                },
                {
                    "IpProtocol": "tcp",
                    "FromPort": 443,
                    "ToPort": 443,
                    "DestinationPrefixListId": c["dynamodb_prefix_list_id"],
                },
            ],
        },
    )
    add(
        "EndpointIngress",
        "AWS::EC2::SecurityGroupIngress",
        {
            "GroupId": ref("EndpointGroup"),
            "IpProtocol": "tcp",
            "FromPort": 443,
            "ToPort": 443,
            "SourceSecurityGroupId": ref("ParentGroup"),
        },
    )
    add(
        "ProvisionerEndpointIngress",
        "AWS::EC2::SecurityGroupIngress",
        {
            "GroupId": ref("EndpointGroup"),
            "IpProtocol": "tcp",
            "FromPort": 443,
            "ToPort": 443,
            "SourceSecurityGroupId": c["ingress_security_group_id"],
        },
    )
    add(
        "KmsEndpoint",
        "AWS::EC2::VPCEndpoint",
        {
            "VpcId": c["vpc_id"],
            "VpcEndpointType": "Interface",
            "ServiceName": "com.amazonaws." + c["region"] + ".kms",
            "PrivateDnsEnabled": True,
            "SubnetIds": c["private_subnet_ids"],
            "SecurityGroupIds": [ref("EndpointGroup")],
            "PolicyDocument": {
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Principal": "*",
                        "Action": USE_ACTIONS,
                        "Resource": arn("WrappingKey"),
                        "Condition": {
                            "ArnEquals": {
                                "aws:PrincipalArn": [
                                    parent_arn(c),
                                    c["provisioning_role_arn"],
                                ]
                            }
                        },
                    }
                ],
            },
        },
    )
    for name, service, actions, target in (
        ("S3Endpoint", "s3", ["s3:GetObject", "s3:GetObjectVersion"], bucket_objects),
        ("DynamoEndpoint", "dynamodb", ["dynamodb:GetItem"], arn("PolicyLedger")),
    ):
        add(
            name,
            "AWS::EC2::VPCEndpoint",
            {
                "VpcId": c["vpc_id"],
                "VpcEndpointType": "Gateway",
                "ServiceName": "com.amazonaws." + c["region"] + "." + service,
                "RouteTableIds": c["private_route_table_ids"],
                "PolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [
                        {
                            "Effect": "Allow",
                            "Principal": "*",
                            "Action": actions,
                            "Resource": target,
                            "Condition": {
                                "ArnEquals": {"aws:PrincipalArn": parent_arn(c)}
                            },
                        }
                    ],
                },
            },
        )
    add(
        "ParentLaunchTemplate",
        "AWS::EC2::LaunchTemplate",
        {
            "LaunchTemplateData": {
                "ImageId": c["parent_ami_id"],
                "InstanceType": c["parent_instance_type"],
                "EnclaveOptions": {"Enabled": True},
                "MetadataOptions": {
                    "HttpTokens": "required",
                    "HttpPutResponseHopLimit": 1,
                    "HttpEndpoint": "enabled",
                },
                "IamInstanceProfile": {"Arn": arn("ParentProfile")},
                "NetworkInterfaces": [
                    {
                        "DeviceIndex": 0,
                        "AssociatePublicIpAddress": False,
                        "Groups": [ref("ParentGroup")],
                    }
                ],
                "BlockDeviceMappings": [
                    {
                        "DeviceName": "/dev/xvda",
                        "Ebs": {
                            "Encrypted": True,
                            "VolumeType": "gp3",
                            "VolumeSize": 20,
                            "DeleteOnTermination": True,
                        },
                    }
                ],
                "TagSpecifications": [
                    {
                        "ResourceType": "instance",
                        "Tags": [
                            {"Key": "Name", "Value": c["name"]},
                            {
                                "Key": "CovenantQualification",
                                "Value": "candidate-not-qualified",
                            },
                            {"Key": "CovenantPolicyPhase", "Value": c["policy_phase"]},
                        ]
                        + (
                            [{"Key": "CovenantEifSha256", "Value": c["eif_sha256"]}]
                            if c["eif_sha256"] is not None
                            else []
                        ),
                    }
                ],
            }
        },
    )
    add(
        "DormantParents",
        "AWS::AutoScaling::AutoScalingGroup",
        {
            "MinSize": "0",
            "MaxSize": "0",
            "DesiredCapacity": "0",
            "VPCZoneIdentifier": c["private_subnet_ids"],
            "LaunchTemplate": {
                "LaunchTemplateId": ref("ParentLaunchTemplate"),
                "Version": {
                    "Fn::GetAtt": ["ParentLaunchTemplate", "LatestVersionNumber"]
                },
            },
        },
    )
    return {
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "Covenant key broker candidate foundation: dormant compute, no activation path",
        "Rules": {
            "ExactAccountAndRegion": {
                "Assertions": [
                    {
                        "Assert": {
                            "Fn::Equals": [ref("AWS::AccountId"), c["account_id"]]
                        },
                        "AssertDescription": "Use the reviewed account",
                    },
                    {
                        "Assert": {"Fn::Equals": [ref("AWS::Region"), c["region"]]},
                        "AssertDescription": "Use the reviewed region",
                    },
                ]
            }
        },
        "Metadata": {
            "Qualification": "NOT_PRODUCTION_QUALIFIED",
            "PolicyPhase": c["policy_phase"],
            "ParentRolePcr3": pcr3(c),
        }
        | (
            {"ApprovedEifSha256": c["eif_sha256"], "ApprovedPcr0": c["approved_pcr0"]}
            if c["policy_phase"] != "provision_only"
            else {}
        ),
        "Resources": resources,
        "Outputs": {
            "WrappingKeyArn": {"Value": arn("WrappingKey")},
            "CiphertextBucket": {"Value": ref("Ciphertexts")},
            "PolicyLedger": {"Value": ref("PolicyLedger")},
            "ParentRoleArn": {"Value": parent_arn(c)},
        },
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    args = parser.parse_args()
    try:
        c = validate(json.loads(args.config.read_text()))
        outputs = {
            "key-policy.json": key_policy(c),
            "foundation.template.json": template(c),
            "review.json": {
                "status": "offline-candidate-not-authorized-or-qualified",
                "account_id": c["account_id"],
                "region": c["region"],
                "policy_phase": c["policy_phase"],
                "parent_role_pcr3": pcr3(c),
                "aws_calls": 0,
                "compute_instances": 0,
            },
        }
        args.output_dir.mkdir(parents=True, exist_ok=True)
        for filename, value in outputs.items():
            destination = args.output_dir / filename
            with destination.open("x") as handle:
                json.dump(value, handle, indent=2)
                handle.write("\n")
        print(
            "Rendered offline candidate; no AWS calls. Existing outputs are never overwritten."
        )
    except (ValueError, OSError) as exc:
        parser.error(str(exc))


if __name__ == "__main__":
    main()
