"""Offline tests only. No test can contact Vast, S3, or SSH."""
import base64
import copy
import fcntl
import io
import json
import os
from pathlib import Path
import struct
import tempfile
import unittest
import urllib.error
import urllib.parse
from unittest.mock import patch

import importlib.util

ADAPTER = Path(__file__).resolve().parents[1] / "compute-canary-vast.py"
SPEC = importlib.util.spec_from_file_location("compute_canary_vast", ADAPTER)
h = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(h)

JOB = "39912823-4b24-48f1-bf45-b01d55726b6d"
OTHER_JOB = "b4e8cbe6-53bf-4943-94bd-0575f4bc6211"
CHALLENGE = "ab" * 32
HOST = "ssh4.vast.ai"
PORT = 23456
KEY = base64.b64encode(struct.pack(">I", 11) + b"ssh-ed25519" + struct.pack(">I", 32) + bytes(range(32))).decode()
OTHER_KEY = base64.b64encode(struct.pack(">I", 11) + b"ssh-ed25519" + struct.pack(">I", 32) + bytes(range(1,33))).decode()
LOG_URL = "https://s3.amazonaws.com/vast.ai/instance_logs/abc012-def.log"


def marker(job=JOB, challenge=CHALLENGE, key=KEY):
    return f"{h.MARKER} {job} {challenge} ssh-ed25519 {key}\n".encode()


def details():
    return {"id": 123, "label": "covenant-workload-" + JOB, "machine_id": 456,
            "actual_status": "running", "gpu_name": "L40S", "dph_total": 0.51,
            "ssh_host": HOST, "ssh_port": PORT}


class FakeTransport:
    def __init__(self):
        self.calls = []
        self.lookup = {"success": True, "instances": [{"id":123,"label":"covenant-workload-"+JOB}]}
        self.instance = details()
        self.second_instance = None
        self.log_response = {"success":True,"result_url":LOG_URL}
        self.log_bytes = b"ordinary startup log\n" + marker()
        self.detail_reads = 0

    def request(self, method, url, headers, data=None):
        self.calls.append((method,url,headers,data))
        if url.startswith(h.API_ORIGIN + "/api/v1/instances/?"):
            return json.dumps(self.lookup).encode()
        if url == h.API_ORIGIN + "/api/v0/instances/123/":
            self.detail_reads += 1
            row = self.second_instance if self.detail_reads > 1 and self.second_instance else self.instance
            return json.dumps({"instances":row}).encode()
        if url == h.API_ORIGIN + "/api/v0/instances/request_logs/123":
            return json.dumps(self.log_response).encode()
        if url == self.log_response.get("result_url"):
            return self.log_bytes
        raise AssertionError("Unexpected request; offline test refuses unknown endpoint")


class AcceptanceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.output = Path(self.temp.name) / "pin"
        self.transport = FakeTransport()
        self.provider = h.Provider("offline-fixture-secret", self.transport)
        self.network_block = patch.object(h.Transport, "request", side_effect=AssertionError("network forbidden in tests"))
        self.network_block.start()

    def tearDown(self):
        self.network_block.stop()
        self.temp.cleanup()

    def verify(self):
        return h.verify(self.provider, JOB, CHALLENGE, HOST, PORT, self.output, "L40S", 1_000_000)

    def assert_refusal_without_pin(self):
        with self.assertRaises(h.Refusal):
            self.verify()
        self.assertFalse((self.output / "known_hosts").exists())
        self.assertFalse((self.output / "host-key-evidence.json").exists())

    def test_complete_verification_and_replay_preserves_exact_private_pin(self):
        first = self.verify()
        self.assertEqual(first["machineId"],456)
        self.assertFalse(first["hardwareAttestation"])
        expected = f"[{HOST}]:{PORT} ssh-ed25519 {KEY}\n".encode()
        self.assertEqual((self.output/"known_hosts").read_bytes(),expected)
        evidence = (self.output/"host-key-evidence.json").read_bytes()
        self.assertNotIn(b"offline-fixture-secret",evidence)
        self.assertNotIn(b"abc012-def.log",evidence)
        self.assertNotIn(b"ordinary startup",evidence)
        self.assertEqual(os.stat(self.output).st_mode & 0o777,0o700)
        self.assertEqual(os.stat(self.output/"known_hosts").st_mode & 0o777,0o600)
        self.assertEqual(os.stat(self.output/"host-key-evidence.json").st_mode & 0o777,0o600)
        self.verify()
        self.assertEqual((self.output/"host-key-evidence.json").read_bytes(),evidence)

    def test_exact_lookup_filter_and_container_log_request_no_bearer_to_s3(self):
        self.verify()
        calls=self.transport.calls
        query=urllib.parse.parse_qs(urllib.parse.urlsplit(calls[0][1]).query)
        self.assertEqual(json.loads(query['select_filters'][0]),{'label':{'eq':'covenant-workload-'+JOB}})
        self.assertEqual(json.loads(query['select_cols'][0]),['id','label'])
        self.assertEqual(query['limit'],['1000'])
        self.assertEqual(calls[2][0],'PUT')
        self.assertEqual(json.loads(calls[2][3]),{'tail':'1000'})
        self.assertEqual(calls[3][2],{'Accept-Encoding':'identity'})
        self.assertNotIn('Authorization',calls[3][2])
        self.assertEqual(self.transport.detail_reads,2)

    def test_host_substitution(self):
        self.transport.instance['ssh_host']='attacker.invalid'
        self.assert_refusal_without_pin()
        self.assertEqual(len(self.transport.calls),2)

    def test_port_substitution(self):
        self.transport.instance['ssh_port']=22
        self.assert_refusal_without_pin()

    def test_provider_fields_fail_closed(self):
        mutations=[('id',True),('id',124),('label','other'),('machine_id',0),
                   ('machine_id',True),('gpu_name','RTX 4090'),('actual_status','stopped'),
                   ('dph_total',float('nan')),('dph_total',-1),('dph_total',1.000001)]
        for name,value in mutations:
            with self.subTest(name=name,value=value):
                self.transport.instance=details();self.transport.instance[name]=value
                self.assert_refusal_without_pin()

    def test_changed_machine_during_logs(self):
        self.transport.second_instance=details();self.transport.second_instance['machine_id']=789
        self.assert_refusal_without_pin()

    def test_ambiguous_or_truncated_list(self):
        original=copy.deepcopy(self.transport.lookup)
        variants=[{'next_token':'more'},{'truncated':True},{'has_more':True},{'success':False},
                  {'instances':[]},{'instances':original['instances']*2},
                  {'instances':[{'id':123,'label':'other'}]},
                  {'instances':[{'id':True,'label':'covenant-workload-'+JOB}]}]
        for mutation in variants:
            with self.subTest(mutation=mutation):
                self.transport.lookup=copy.deepcopy(original);self.transport.lookup.update(mutation)
                self.assert_refusal_without_pin()

    def test_wrong_job_and_wrong_challenge_even_with_valid_marker(self):
        for bad in [marker(job=OTHER_JOB),marker(challenge='cd'*32)]:
            self.transport.log_bytes=marker()+bad
            self.assert_refusal_without_pin()

    def test_conflicting_public_keys(self):
        self.transport.log_bytes=marker()+marker(key=OTHER_KEY)
        self.assert_refusal_without_pin()

    def test_duplicate_same_marker_is_accepted(self):
        self.transport.log_bytes=marker()+marker()
        self.verify()

    def test_missing_malformed_prefixed_marker_and_oversized_logs(self):
        for blob in [b'no marker\n',b'2026-01-01 '+marker(),b'COVENANT_SSH_HOST_KEY_V1 unavailable\n',
                     marker().replace(b'ssh-ed25519',b'ssh-rsa'),marker()+b'\xff',b'x'*(h.MAX_BYTES+1)]:
            with self.subTest(size=len(blob)):
                self.transport.log_bytes=blob
                self.assert_refusal_without_pin()

    def test_log_url_rejected_before_external_fetch(self):
        urls=['http://s3.amazonaws.com/vast.ai/instance_logs/abc.log',
              'https://s3.amazonaws.com.evil.invalid/vast.ai/instance_logs/abc.log',
              'https://user@s3.amazonaws.com/vast.ai/instance_logs/abc.log',
              'https://s3.amazonaws.com:443/vast.ai/instance_logs/abc.log',
              'https://s3.amazonaws.com/other/instance_logs/abc.log',
              'https://s3.amazonaws.com/vast.ai/instance_logs/../secret.log',
              'https://s3.amazonaws.com/vast.ai/instance_logs/%2e%2e.log',
              LOG_URL+'?token=secret',LOG_URL+'#fragment','file:///tmp/log',
              'https://s3.amazonaws.com/vast.ai/instance_logs/nested/abc.log',
              LOG_URL+'\n']
        for url in urls:
            with self.subTest(url=url):
                self.transport.calls=[];self.transport.log_response['result_url']=url
                self.assert_refusal_without_pin()
                self.assertEqual(len(self.transport.calls),3)

    def test_authenticated_public_vast_bucket_exact_shape_and_same_origin(self):
        public_url = "https://s3.amazonaws.com/public.vast.ai/instance_logs/" + "a"*64 + ".log"
        self.transport.log_response['result_url'] = public_url
        result = self.verify()
        self.assertEqual(result['sourceLogOrigin'], h.LOG_ORIGIN)
        download = next(c for c in self.transport.calls if c[1] == public_url)
        self.assertEqual(download[0], 'GET')
        self.assertEqual(download[2], {'Accept-Encoding':'identity'})
        self.assertNotIn('Authorization', download[2])

    def test_public_vast_bucket_near_misses_refused_before_download(self):
        prefix = "https://s3.amazonaws.com/public.vast.ai/instance_logs/"
        urls = [prefix+"a"*63+".log", prefix+"A"*64+".log",
                prefix+"a"*65+".log", prefix+"a"*64+".log?signature=secret",
                prefix+"a"*64+".log#fragment", prefix+"%61"*64+".log",
                prefix+"../"+"a"*64+".log", prefix+"nested/"+"a"*64+".log",
                prefix.replace('/public.vast.ai/', '/public.vast.ai.evil/')+"a"*64+".log",
                prefix.replace('s3.amazonaws.com/', 's3.amazonaws.com.evil/')+"a"*64+".log",
                prefix.replace('s3.amazonaws.com/', 's3.amazonaws.com:443/')+"a"*64+".log",
                prefix.replace('s3.amazonaws.com/', 'user@s3.amazonaws.com/')+"a"*64+".log"]
        for url in urls:
            with self.subTest(url=url):
                self.transport.calls=[]; self.transport.log_response['result_url']=url
                self.assert_refusal_without_pin()
                self.assertEqual(len(self.transport.calls),3)

    def test_invalid_wire_keys(self):
        wire=base64.b64decode(KEY)
        keys=['x',KEY+'=',base64.b64encode(wire[:-1]).decode(),
              base64.b64encode(wire[:15]+struct.pack('>I',31)+wire[19:]).decode(),
              base64.b64encode(wire[:19]+bytes(32)).decode(),
              base64.b64encode(wire.replace(b'ssh-ed25519',b'ssh-invalid')).decode()]
        for key in keys:
            with self.subTest(key=key):
                self.transport.log_bytes=marker(key=key)
                self.assert_refusal_without_pin()

    def test_existing_pin_cannot_be_replaced(self):
        self.verify();original=(self.output/'known_hosts').read_bytes()
        self.transport.log_bytes=marker(key=OTHER_KEY)
        with self.assertRaises(h.Refusal):self.verify()
        self.assertEqual((self.output/'known_hosts').read_bytes(),original)

    def test_concurrent_verification_refused_without_network(self):
        h.private_directory(self.output)
        with h.output_lock(self.output):
            self.assert_refusal_without_pin()
        self.assertEqual(self.transport.calls,[])

    def test_symlink_pin_is_refused_without_overwrite(self):
        h.private_directory(self.output)
        target=Path(self.temp.name)/'target';target.write_text('preserve')
        (self.output/'known_hosts').symlink_to(target)
        with self.assertRaises(h.Refusal):self.verify()
        self.assertEqual(target.read_text(),'preserve')

    def test_inputs_rejected_before_network(self):
        variants=[(JOB.upper(),CHALLENGE,HOST,PORT),(JOB,'00'*32,HOST,PORT),
                  (JOB,CHALLENGE,HOST+'\nother',PORT),(JOB,CHALLENGE,'*.vast.ai',PORT),
                  (JOB,CHALLENGE,'fe80::1%eth0',PORT),(JOB,CHALLENGE,'fe80::1%a\nb',PORT),
                  (JOB,CHALLENGE,HOST,True),(JOB,CHALLENGE,HOST,0)]
        for job,challenge,host,port in variants:
            with self.assertRaises(h.Refusal):h.verify(self.provider,job,challenge,host,port,self.output)
        self.assertEqual(self.transport.calls,[])

    def test_decimal_price_ceil_and_default_port_known_host(self):
        self.assertEqual(h.price_micros('0.0000001'),1)
        self.assertEqual(h.known_hosts_line(HOST,22,KEY),f'{HOST} ssh-ed25519 {KEY}\n'.encode())

    def test_duplicate_json_fields_refused(self):
        with self.assertRaises(h.Refusal):h.parse_json(b'{"success":true,"success":false}')


class HttpTransportTests(unittest.TestCase):
    def test_redirect_handler_always_refuses(self):
        for status in [301,302,303,307,308]:
            with self.assertRaises(h.Refusal):
                h.NoRedirect().redirect_request(None,None,status,'',{},'https://attacker.invalid')

    def test_redirect_and_oversize_responses_fail_without_following(self):
        transport=h.Transport()
        class Opener:
            def __init__(self,response=None,error=None):self.response=response;self.error=error;self.calls=0
            def open(self,*a,**k):
                self.calls+=1
                if self.error:raise self.error
                return self.response
        class Response(io.BytesIO):
            status=200
            headers={}
            def geturl(self):return LOG_URL
        for status in [301,302,307,308]:
            fake=Opener(error=urllib.error.HTTPError(LOG_URL,status,'redirect',{'Location':'https://attacker.invalid'},None))
            transport.opener=fake
            with self.assertRaises(h.Refusal):transport.request('GET',LOG_URL,{})
            self.assertEqual(fake.calls,1)
        response=Response(b'x'*(h.MAX_BYTES+1));transport.opener=Opener(response)
        with self.assertRaises(h.Refusal):transport.request('GET',LOG_URL,{})
        response=Response(b'');response.headers={'Content-Length':str(h.MAX_BYTES+1)}
        transport.opener=Opener(response)
        with self.assertRaises(h.Refusal):transport.request('GET',LOG_URL,{})




BUYER = "4wBqpZM9xaSheZzJSMawUKKwhdpChKbZ5eu5ky4Vigw"
VECTOR_JOB = "10000000-0000-4000-8000-000000000001"
VECTOR = "6b5947310f148030fa86cd51ba0cbebb5e82b93c2e09993cace83c01b7c6fca1"


def offer():
    return {"gpu_name": "L40S", "num_gpus": 1, "gpu_ram": 48000,
            "verification": "verified", "reliability": 0.999,
            "rentable": True, "rented": False, "direct_port_count": 1,
            "cuda_max_good": 12.8, "gpu_arch": "nvidia", "cpu_arch": "amd64",
            "dph_total": "0.51", "inet_up_cost": "0.0001", "inet_down_cost": "0.0001"}


class CanaryTransport(FakeTransport):
    def __init__(self):
        super().__init__()
        self.account = {"credit": "5.00"}
        self.offers = {"offers": [offer()]}
        self.inventory_error = None
        self.log_bytes = marker(challenge=h.canary_challenge(BUYER, JOB))

    def request(self, method, url, headers, data=None):
        if url == h.API_ORIGIN + "/api/v0/users/current/":
            self.calls.append((method, url, headers, data))
            return json.dumps(self.account).encode()
        if url == h.API_ORIGIN + "/api/v0/bundles/":
            self.calls.append((method, url, headers, data))
            return json.dumps(self.offers).encode()
        if url.startswith(h.API_ORIGIN + "/api/v1/instances/?") and self.inventory_error:
            self.calls.append((method, url, headers, data))
            raise self.inventory_error
        return super().request(method, url, headers, data)


class CanaryAdapterTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.directory = Path(self.temp.name) / "provider-evidence"
        self.config = {"buyer": BUYER, "directory": self.directory, "gpu": "L40S",
                       "max_hourly": 1_000_000, "reservation": 350_000,
                       "max_network_per_gb": 4000, "coordinator": "https://compute-api.opencovenant.org"}
        self.transport = CanaryTransport()
        self.provider = h.Provider("offline-fixture-secret", self.transport)
        self.network_block = patch.object(h.Transport, "request", side_effect=AssertionError("network forbidden in tests"))
        self.network_block.start()

    def tearDown(self):
        self.network_block.stop()
        self.temp.cleanup()

    def request(self, operation="access", **fields):
        return {"version": 1, "operation": operation,
                "coordinator_url": self.config["coordinator"], "job_id": JOB,
                "lease_view": {"job_id": JOB, "access": {"endpoint": f"ssh root@{HOST} -p {PORT}"}}, **fields}

    def run_adapter(self, request=None):
        return h.adapter(request or self.request(), self.provider, self.config, 1802908860)

    def mapping_path(self):
        return self.directory / JOB / "instance.json"

    def test_buyer_job_challenge_matches_broker_cross_language_vector(self):
        self.assertEqual(h.canary_challenge(BUYER, VECTOR_JOB), VECTOR)
        self.assertNotEqual(h.canary_challenge(BUYER, JOB), VECTOR)
        self.assertNotEqual(h.canary_challenge("1" * 31 + "2", VECTOR_JOB), VECTOR)
        for buyer, job in [("1" * 32, VECTOR_JOB), (BUYER, "00000000-0000-0000-0000-000000000000"),
                           (BUYER, JOB.upper()), (BUYER + "0", VECTOR_JOB), (None, VECTOR_JOB),
                           (BUYER, None), ("1" * 31, VECTOR_JOB), ("1" * 33, VECTOR_JOB)]:
            with self.subTest(buyer=buyer, job=job), self.assertRaises(h.Refusal):
                h.canary_challenge(buyer, job)

    def test_preflight_requires_empty_inventory_credit_and_bounded_capacity(self):
        self.transport.lookup["instances"] = []
        result = self.run_adapter(self.request("preflight"))
        self.assertEqual(result, {"version": 1, "operation": "preflight", "host_key_binding": "authenticated",
                                 "cleanup": "provider_readback", "provider_limits_checked": True,
                                 "provider_reservation_micro_usd": 350000, "max_provider_hourly_micro_usd": 1000000})
        self.assertFalse(self.directory.exists())
        search = next(call for call in self.transport.calls if call[0] == "POST")
        query = json.loads(search[3])
        self.assertEqual(query["limit"], 64)
        self.assertEqual(query["allocated_storage"], 16)
        self.assertEqual(query["gpu_name"], {"in": ["L40S"]})
        self.assertEqual(query["inet_up_cost"], {"lte": .004})
        self.assertEqual(query["inet_down_cost"], {"lte": .004})
        self.assertEqual(query["direct_port_count"], {"gte": 1})

    def test_preflight_existing_instances_stop_before_credit_or_offer_request(self):
        with self.assertRaisesRegex(h.Refusal, "active instances"):
            self.run_adapter(self.request("preflight"))
        self.assertEqual(len(self.transport.calls), 1)
        self.assertFalse(self.directory.exists())

    def test_actual_market_network_price_is_within_pilot_admission_ceiling(self):
        self.transport.lookup["instances"] = []
        self.transport.offers["offers"][0].update(inet_up_cost="0.00390625", inet_down_cost="0.00390625")
        self.assertTrue(self.run_adapter(self.request("preflight"))["provider_limits_checked"])
        self.transport.offers["offers"][0]["inet_down_cost"] = "0.004001"
        with self.assertRaisesRegex(h.Refusal, "bounded provider capacity"):
            self.run_adapter(self.request("preflight"))

    def test_preflight_no_credit_stops_before_capacity_search(self):
        self.transport.lookup["instances"] = []
        self.transport.account["credit"] = "0.349999"
        with self.assertRaisesRegex(h.Refusal, "credit insufficient"):
            self.run_adapter(self.request("preflight"))
        self.assertFalse(any(call[0] == "POST" for call in self.transport.calls))

    def test_preflight_empty_and_unbounded_offers_do_not_admit(self):
        self.transport.lookup["instances"] = []
        mutations = [{"gpu_name": "RTX 4090"}, {"num_gpus": 2}, {"gpu_ram": 39999},
                     {"verification": "unverified"}, {"reliability": .98},
                     {"rentable": False}, {"rented": True}, {"direct_port_count": 0},
                     {"cuda_max_good": 12.3}, {"gpu_arch": "amd"}, {"cpu_arch": "arm64"},
                     {"dph_total": "1.000001"}, {"dph_total": 0}, {"dph_total": "NaN"},
                     {"inet_up_cost": "0.004001"}, {"inet_down_cost": "0.004001"}]
        lists = [[], None, [offer()] * 65, [{}], [None], ["not an offer"]]
        lists += [[offer() | mutation] for mutation in mutations]
        for rows in lists:
            with self.subTest(rows=rows), self.assertRaises(h.Refusal):
                self.transport.offers = {"offers": rows}
                self.run_adapter(self.request("preflight"))
        self.assertFalse(self.directory.exists())

    def test_mapping_and_pin_are_write_once_and_replay_exactly(self):
        first = self.run_adapter()
        mapping = self.mapping_path().read_bytes()
        pinned = (self.directory / JOB / "host" / "known_hosts").read_bytes()
        self.assertEqual(json.loads(mapping), {"buyer": BUYER, "job_id": JOB, "instance_id": 123,
                                              "challenge": h.canary_challenge(BUYER, JOB)})
        self.assertEqual(first["provider_instance_id"], "vast:123")
        self.assertEqual(first["ssh"]["public_key"], "ssh-ed25519 " + KEY)
        self.assertEqual(first["binding"], {"verified": True, "source": "provider_authenticated", "job_id": JOB})
        self.assertEqual(self.mapping_path().stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.run_adapter(), first)
        self.assertEqual(self.mapping_path().read_bytes(), mapping)
        self.assertEqual((self.directory / JOB / "host" / "known_hosts").read_bytes(), pinned)
        self.transport.lookup["instances"][0]["id"] = 124
        with self.assertRaisesRegex(h.Refusal, "replacement refused"):
            self.run_adapter()
        self.assertEqual(self.mapping_path().read_bytes(), mapping)

    def test_authenticated_empty_inventory_proves_cleanup_of_bound_instance(self):
        self.run_adapter()
        self.transport.calls.clear()
        self.transport.lookup["instances"] = []
        result = self.run_adapter(self.request("cleanup", provider_instance_id="vast:123"))
        self.assertEqual(result["exists"], False)
        self.assertEqual(result["provider_instance_id"], "vast:123")
        self.assertTrue(result["binding_verified"])
        self.assertEqual(result["source"], "provider_readback")
        self.assertEqual(len(self.transport.calls), 1)
        self.assertEqual(self.transport.calls[0][0], "GET")
        self.assertTrue(self.mapping_path().exists())

    def test_instance_still_present_or_relabelled_is_not_deleted(self):
        self.run_adapter()
        for label in ("covenant-workload-" + JOB, "other-label"):
            self.transport.lookup["instances"] = [{"id": 123, "label": label}]
            self.assertTrue(self.run_adapter(self.request("cleanup", provider_instance_id="vast:123"))["exists"])

    def test_empty_inventory_without_authenticated_mapping_does_not_prove_deletion(self):
        self.transport.lookup["instances"] = []
        with self.assertRaisesRegex(h.Refusal, "mapping unavailable"):
            self.run_adapter(self.request("cleanup"))
        self.assertFalse(self.mapping_path().exists())

    def test_404_or_provider_failure_never_counts_as_deletion(self):
        self.run_adapter()
        for error in (h.Refusal("provider request returned HTTP 404"), h.Refusal("provider request failed")):
            self.transport.inventory_error = error
            with self.assertRaises(h.Refusal):
                self.run_adapter(self.request("cleanup", provider_instance_id="vast:123"))

    def test_truncated_invalid_or_ambiguous_inventory_never_proves_deletion(self):
        self.run_adapter()
        original = copy.deepcopy(self.transport.lookup)
        mutations = [{"success": False}, {"next_token": "more"}, {"has_more": True}, {"truncated": True},
                     {"instances": [{}]}, {"instances": [{"id": True}]},
                     {"instances": [{"id": 123}, {"id": 123}]}, {"instances": [{"id": i + 1} for i in range(h.PAGE_LIMIT)]},
                     {"instances": [{"id": 123, "label": "covenant-workload-" + JOB}, {"id": 124, "label": "covenant-workload-" + JOB}]}]
        for mutation in mutations:
            self.transport.lookup = copy.deepcopy(original) | mutation
            with self.subTest(mutation=mutation), self.assertRaises(h.Refusal):
                self.run_adapter(self.request("cleanup", provider_instance_id="vast:123"))

    def test_cleanup_refuses_request_and_persisted_identity_substitution(self):
        self.run_adapter()
        self.transport.lookup["instances"] = []
        with self.assertRaisesRegex(h.Refusal, "instance substitution"):
            self.run_adapter(self.request("cleanup", provider_instance_id="vast:999"))
        original = self.mapping_path().read_bytes()
        for mutation in ({"buyer": "1" * 31 + "2"}, {"job_id": OTHER_JOB}, {"challenge": "ff" * 32}, {"instance_id": True}):
            self.mapping_path().write_text(json.dumps(json.loads(original) | mutation))
            with self.subTest(mutation=mutation), self.assertRaisesRegex(h.Refusal, "mapping unavailable"):
                self.run_adapter(self.request("cleanup"))
        self.mapping_path().write_bytes(original)

    def test_endpoint_argument_orders_are_equivalent_and_extra_options_refused(self):
        for endpoint in (f"ssh root@{HOST} -p {PORT}", f"ssh -p {PORT} root@{HOST}"):
            request = self.request(lease_view={"job_id": JOB, "access": {"endpoint": endpoint}})
            self.assertEqual(self.run_adapter(request)["ssh"]["host"], HOST)
        for endpoint in (f"ssh other@{HOST} -p {PORT}", f"ssh root@{HOST} -p {PORT} -o ProxyCommand=evil",
                         f"ssh -o ProxyCommand=evil root@{HOST}", f"ssh root@other.invalid -p {PORT}"):
            with self.subTest(endpoint=endpoint), self.assertRaises(h.Refusal):
                self.run_adapter(self.request(lease_view={"job_id": JOB, "access": {"endpoint": endpoint}}))

    def test_coordinator_and_lease_identity_must_match_before_host_pin(self):
        with self.assertRaisesRegex(h.Refusal, "coordinator binding mismatch"):
            self.run_adapter(self.request(coordinator_url="https://attacker.invalid"))
        self.assertFalse(self.transport.calls)
        with self.assertRaisesRegex(h.Refusal, "lease view mismatch"):
            self.run_adapter(self.request(lease_view={"job_id": OTHER_JOB}))
        self.assertFalse((self.directory / JOB / "host" / "known_hosts").exists())

    def test_provider_mutating_methods_and_routes_are_refused_before_transport(self):
        attempts = [("DELETE", "/api/v0/instances/123/"), ("PUT", "/api/v0/asks/123/"),
                    ("PUT", "/api/v0/instances/123/"), ("POST", "/api/v0/instances/123/execute/"),
                    ("POST", "/api/v0/users/current/"), ("PUT", "/api/v0/instances/request_logs/123/../456"),
                    ("PUT", "/api/v0/instances/request_logs/123?destroy=true"),
                    ("GET", "https://attacker.invalid/api/v1/instances/"), ("POST", "/api/v0/bundles/#other")]
        for method, path in attempts:
            with self.subTest(method=method, path=path), self.assertRaises(h.Refusal):
                self.provider.api(method, path, {"command": "must never run"})
        self.assertFalse(self.transport.calls)


if __name__ == "__main__":
    unittest.main()
