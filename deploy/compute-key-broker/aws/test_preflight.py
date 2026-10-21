"""Read-only inventory boundaries; no AWS calls or real account data."""

import copy
import unittest
from preflight import check
from test_policy import fixture


def inventory(c):
    return {
        "caller": {"Account": c["account_id"]},
        "region": c["region"],
        "instance_types": {
            "InstanceTypes": [
                {
                    "InstanceType": c["parent_instance_type"],
                    "NitroEnclavesSupport": "supported",
                    "ProcessorInfo": {"SupportedArchitectures": ["x86_64"]},
                }
            ]
        },
        "images": {
            "Images": [
                {
                    "ImageId": c["parent_ami_id"],
                    "State": "available",
                    "Architecture": "x86_64",
                    "RootDeviceName": "/dev/xvda",
                }
            ]
        },
        "subnets": {
            "Subnets": [
                {
                    "SubnetId": subnet,
                    "AvailabilityZone": c["region"] + chr(97 + i),
                    "VpcId": c["vpc_id"],
                    "MapPublicIpOnLaunch": False,
                }
                for i, subnet in enumerate(c["private_subnet_ids"])
            ]
        },
        "route_tables": {
            "RouteTables": [
                {
                    "RouteTableId": route,
                    "VpcId": c["vpc_id"],
                    "Associations": [{"SubnetId": c["private_subnet_ids"][i]}],
                    "Routes": [{"GatewayId": "local"}],
                }
                for i, route in enumerate(c["private_route_table_ids"])
            ]
        },
        "prefix_lists": {
            "PrefixLists": [
                {
                    "PrefixListId": c[service + "_prefix_list_id"],
                    "PrefixListName": "com.amazonaws." + c["region"] + "." + service,
                }
                for service in ("s3", "dynamodb")
            ]
        },
        "roles": [
            {"Role": {"Arn": c[name]}}
            for name in ("key_admin_role_arn", "provisioning_role_arn")
        ],
    }


class PreflightTests(unittest.TestCase):
    def setUp(self):
        self.c = fixture()
        self.i = inventory(self.c)

    def test_reviewed_shape_remains_unqualified(self):
        result = check(self.c, self.i)
        self.assertIn("not-production-qualified", result["status"])
        self.assertIn(
            "live AWS authorization and hardware tests", result["remaining_gates"]
        )

    def test_wrong_account_and_region(self):
        for field in ("account", "region"):
            bad = copy.deepcopy(self.i)
            if field == "account":
                bad["caller"]["Account"] = "000000000000"
            else:
                bad["region"] = "us-east-1"
            with self.assertRaises(ValueError):
                check(self.c, bad)

    def test_non_nitro_or_wrong_architecture(self):
        bad = copy.deepcopy(self.i)
        bad["instance_types"]["InstanceTypes"][0][
            "NitroEnclavesSupport"
        ] = "unsupported"
        with self.assertRaises(ValueError):
            check(self.c, bad)
        bad = copy.deepcopy(self.i)
        bad["images"]["Images"][0]["Architecture"] = "arm64"
        with self.assertRaises(ValueError):
            check(self.c, bad)

    def test_public_or_single_az_subnets(self):
        for field, value in (
            ("MapPublicIpOnLaunch", True),
            ("VpcId", "vpc-wrong"),
            ("AvailabilityZone", self.i["subnets"]["Subnets"][1]["AvailabilityZone"]),
        ):
            bad = copy.deepcopy(self.i)
            bad["subnets"]["Subnets"][0][field] = value
            with self.assertRaises(ValueError):
                check(self.c, bad)

    def test_public_routes_and_missing_associations(self):
        for route in (
            {"GatewayId": "igw-0123456789abcdef0"},
            {"NatGatewayId": "nat-0123456789abcdef0"},
            {"EgressOnlyInternetGatewayId": "eigw-0123456789abcdef0"},
        ):
            bad = copy.deepcopy(self.i)
            bad["route_tables"]["RouteTables"][0]["Routes"].append(route)
            with self.assertRaises(ValueError):
                check(self.c, bad)
        bad = copy.deepcopy(self.i)
        bad["route_tables"]["RouteTables"][0]["Associations"] = []
        with self.assertRaises(ValueError):
            check(self.c, bad)

    def test_wrong_endpoint_region(self):
        bad = copy.deepcopy(self.i)
        bad["prefix_lists"]["PrefixLists"][0][
            "PrefixListName"
        ] = "com.amazonaws.us-east-1.s3"
        with self.assertRaises(ValueError):
            check(self.c, bad)


if __name__ == "__main__":
    unittest.main()
