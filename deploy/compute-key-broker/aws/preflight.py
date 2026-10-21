#!/usr/bin/env python3
"""Validate previously captured read-only AWS inventory; no network/AWS calls."""
import argparse
import json
from pathlib import Path
from render import parent_arn, validate


def check(c, inventory):
    validate(c)
    if (
        inventory["caller"]["Account"] != c["account_id"]
        or inventory["region"] != c["region"]
    ):
        raise ValueError("inventory account/region mismatch")
    instances = inventory["instance_types"]["InstanceTypes"]
    chosen = [
        item for item in instances if item["InstanceType"] == c["parent_instance_type"]
    ]
    if len(chosen) != 1 or chosen[0].get("NitroEnclavesSupport") != "supported":
        raise ValueError("instance type must explicitly support Nitro Enclaves")
    images = [
        item
        for item in inventory["images"]["Images"]
        if item["ImageId"] == c["parent_ami_id"]
    ]
    if (
        len(images) != 1
        or images[0]["State"] != "available"
        or images[0]["Architecture"]
        not in chosen[0]["ProcessorInfo"]["SupportedArchitectures"]
    ):
        raise ValueError("AMI absent, unavailable or architecture mismatch")
    if images[0].get("RootDeviceName") != "/dev/xvda":
        raise ValueError("review and adjust root device mapping for this AMI")
    subnets = [
        item
        for item in inventory["subnets"]["Subnets"]
        if item["SubnetId"] in c["private_subnet_ids"]
    ]
    if len(subnets) != len(c["private_subnet_ids"]) or len(
        {item["AvailabilityZone"] for item in subnets}
    ) != len(subnets):
        raise ValueError("subnets must exist in distinct availability zones")
    if any(
        item["VpcId"] != c["vpc_id"] or item.get("MapPublicIpOnLaunch", True)
        for item in subnets
    ):
        raise ValueError("subnets must be private and in the reviewed VPC")
    tables = [
        item
        for item in inventory["route_tables"]["RouteTables"]
        if item["RouteTableId"] in c["private_route_table_ids"]
    ]
    if len(tables) != len(c["private_route_table_ids"]) or any(
        item["VpcId"] != c["vpc_id"] for item in tables
    ):
        raise ValueError("route tables missing or wrong VPC")
    associated = {a.get("SubnetId") for t in tables for a in t.get("Associations", [])}
    if not set(c["private_subnet_ids"]).issubset(associated):
        raise ValueError("require explicit private subnet route-table associations")
    for table in tables:
        for route in table.get("Routes", []):
            if (
                route.get("GatewayId", "").startswith("igw-")
                or "NatGatewayId" in route
                or "EgressOnlyInternetGatewayId" in route
            ):
                raise ValueError(
                    "candidate broker subnets must not route through internet/NAT gateways"
                )
    prefix_lists = {
        x["PrefixListId"]: x["PrefixListName"]
        for x in inventory["prefix_lists"]["PrefixLists"]
    }
    for service in ("s3", "dynamodb"):
        if (
            prefix_lists.get(c[service + "_prefix_list_id"])
            != "com.amazonaws." + c["region"] + "." + service
        ):
            raise ValueError("wrong regional service prefix list")
    roles = {x["Role"]["Arn"] for x in inventory["roles"]}
    if not {c["key_admin_role_arn"], c["provisioning_role_arn"]}.issubset(roles):
        raise ValueError("required pre-existing admin/provisioner roles absent")
    if parent_arn(c) in roles:
        raise ValueError(
            "parent role name already exists; do not overwrite an existing identity"
        )
    return {
        "status": "read-only-inventory-checked-not-production-qualified",
        "remaining_gates": [
            "review AMI owner/provenance and immutable EIF contents",
            "account plan eligibility and EC2/KMS quotas",
            "IAM trust policies, SCPs, permission boundaries and CloudFormation execution role",
            "approved TLS ingress, vsock proxy and collateral services",
            "CloudTrail independent retention and alert delivery",
            "live AWS authorization and hardware tests",
            "independent security review",
            "explicit deployment and spending approval",
        ],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("--inventory", required=True, type=Path)
    args = parser.parse_args()
    try:
        result = check(
            json.loads(args.config.read_text()), json.loads(args.inventory.read_text())
        )
    except (ValueError, KeyError, TypeError, OSError) as exc:
        parser.error(str(exc))
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
