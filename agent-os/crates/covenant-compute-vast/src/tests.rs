use std::io::Write;

use serde_json::json;
use tempfile::NamedTempFile;
use wiremock::{
    matchers::{body_json, body_partial_json, header, method, path, query_param},
    Mock, MockServer, ResponseTemplate,
};

use super::*;

const TOKEN: &str = "vast-test-token-not-a-real-secret";
const SSH_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMockedPublicKeyMaterial test";
const IMAGE: &str = WORKSPACE_IMAGE;

fn client(server: &MockServer, max_hourly_micros: u64) -> VastClient {
    VastClient::new(
        VastConfig {
            api_url: Url::parse(&format!("{}/api/v0/", server.uri())).unwrap(),
            max_hourly_micros,
            ..VastConfig::default()
        },
        ApiToken::new(TOKEN).unwrap(),
    )
    .unwrap()
}

fn offer(id: u64, machine_id: u64, price: f64) -> serde_json::Value {
    json!({
        "id": id,
        "machine_id": machine_id,
        "gpu_name": "L40S",
        "gpu_ram": 46068,
        "dph_total": price,
        "inet_down_cost": 0.0,
        "inet_up_cost": 0.0,
        "verification": "verified",
        "reliability": 0.999,
        "rentable": true,
        "rented": false,
        "direct_port_count": 2,
        "cuda_max_good": 12.4,
        "num_gpus": 1,
        "gpu_arch": "nvidia",
        "cpu_arch": "amd64"
    })
}

fn quoted_offer(id: u64, machine_id: u64, hourly_micros: u64) -> OfferQuote {
    OfferQuote {
        id,
        machine_id,
        gpu_model: "L40S".to_owned(),
        gpu_memory_mib: 46_068,
        hourly_micros,
    }
}

fn launch_request(cap: u64, required_offer: OfferQuote) -> LaunchRequest {
    LaunchRequest {
        workload_id: "job_123".to_owned(),
        image: IMAGE.to_owned(),
        max_hourly_micros: cap,
        ssh_public_key: SSH_KEY.to_owned(),
        rejected_machine_ids: Vec::new(),
        required_offer,
    }
}

fn workspace_launch() -> WorkspaceLaunch {
    WorkspaceLaunch {
        instance_id: 99,
        label: "covenant-workload-job_123".to_owned(),
        offer: quoted_offer(7, 70, 590_000),
        image: IMAGE.to_owned(),
    }
}

fn workspace_instance() -> serde_json::Value {
    json!({
        "instances": {
            "id": 99,
            "label": "covenant-workload-job_123",
            "actual_status": "running",
            "image_uuid": IMAGE,
            "image_runtype": "jupyter_direct",
            "gpu_name": "L40S",
            "gpu_ram": 46068,
            "verification": "verified",
            "dph_total": 0.59,
            "machine_id": 70,
            "bundle_id": 7,
            "public_ipaddr": "203.0.113.7",
            "ports": {"8080/tcp": [{"HostIp": "0.0.0.0", "HostPort": "32123"}]},
            "jupyter_token": "0123456789abcdef0123456789abcdef"
        }
    })
}

#[test]
fn tokens_are_loaded_safely_and_redacted() {
    let token = ApiToken::new(TOKEN).unwrap();
    assert_eq!(format!("{token:?}"), "ApiToken([REDACTED])");
    assert!(!format!("{token:?}").contains(TOKEN));

    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "{TOKEN}").unwrap();
    assert_eq!(ApiToken::from_file(file.path()).unwrap().expose(), TOKEN);

    let mut invalid = NamedTempFile::new().unwrap();
    writeln!(invalid, "two tokens").unwrap();
    assert!(matches!(
        ApiToken::from_file(invalid.path()),
        Err(VastError::InvalidCredential)
    ));
}

#[test]
fn only_https_and_loopback_http_urls_are_allowed() {
    for allowed in [
        DEFAULT_API_URL,
        "http://127.0.0.1:8080/api/v0/",
        "http://[::1]:8080/api/v0/",
        "http://localhost:8080/api/v0/",
    ] {
        assert!(VastConfig {
            api_url: Url::parse(allowed).unwrap(),
            ..VastConfig::default()
        }
        .validate()
        .is_ok());
    }
    for denied in [
        "http://example.com/api/v0/",
        "ftp://example.com/api/v0/",
        "https://user:pass@example.com/api/v0/",
        "https://example.com/api/v0/?token=secret",
        "https://example.com/api/v0",
    ] {
        assert!(VastConfig {
            api_url: Url::parse(denied).unwrap(),
            ..VastConfig::default()
        }
        .validate()
        .is_err());
    }
}

#[tokio::test]
async fn ranks_only_admitted_offers_under_both_caps() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/bundles/"))
        .and(header("authorization", format!("Bearer {TOKEN}")))
        .and(body_partial_json(json!({
            "reliability": {"gte": 0.99},
            "verified": {"eq": true},
            "rentable": {"eq": true},
            "rented": {"eq": false},
            "direct_port_count": {"gte": 1},
            "disk_space": {"gte": 16},
            "allocated_storage": 16,
            "inet_down_cost": {"lte": 0.05},
            "inet_up_cost": {"lte": 0.05},
            "cuda_max_good": {"gte": 12.4},
            "gpu_arch": {"eq": "nvidia"},
            "cpu_arch": {"eq": "amd64"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "offers": [
                offer(1, 10, 0.61),
                offer(2, 20, 0.59),
                offer(3, 30, 0.70),
                {
                    "id": 4, "machine_id": 40, "gpu_name": "RTX 4090",
                    "gpu_ram": 24576, "dph_total": 0.40,
                    "inet_down_cost": 0.0, "inet_up_cost": 0.0,
                    "verification": "verified", "reliability": 0.999,
                    "rentable": true, "rented": false,
                    "direct_port_count": 2, "cuda_max_good": 12.4,
                    "num_gpus": 1, "gpu_arch": "nvidia", "cpu_arch": "amd64"
                }
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let ranked = client(&server, 640_000)
        .ranked_offers(8, &[20], 700_000)
        .await
        .unwrap();
    let offers = ranked.offers;
    assert_eq!(
        offers.iter().map(|offer| offer.id).collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(offers[0].verification, "verified");
    assert_eq!(offers[0].reliability, 0.999);
    assert!(offers[0].rentable);
    assert!(!offers[0].rented);
    assert_eq!(offers[0].direct_port_count, 2);
    assert_eq!(
        offers[0].cuda_max_good,
        CudaVersion {
            major: 12,
            minor: 4
        }
    );
    assert_eq!(
        ranked.survey,
        OfferSurvey {
            returned: 4,
            gpu_class: 1,
            price_ceiling: 1,
            rejected_machine: 1,
            admitted: 1,
            ..OfferSurvey::default()
        }
    );
}

#[tokio::test]
async fn an_empty_search_reports_which_constraint_emptied_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/bundles/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "offers": [offer(1, 10, 0.80), offer(2, 20, 0.81)]
        })))
        .mount(&server)
        .await;

    // The market the operator complained about: every offer priced above the
    // configured hourly ceiling.
    let ranked = client(&server, 640_000)
        .ranked_offers(8, &[], 640_000)
        .await
        .unwrap();

    assert!(ranked.offers.is_empty());
    assert_eq!(ranked.survey.returned, 2);
    assert_eq!(ranked.survey.price_ceiling, 2);
    assert_eq!(ranked.survey.admitted, 0);
}

#[test]
fn shipped_defaults_clear_the_current_forty_gigabyte_market() {
    let config = VastConfig::default();
    assert_eq!(config.max_hourly_micros, 1_000_000);
    assert_eq!(config.min_gpu_memory_mib, 40_000);
    assert_eq!(config.max_inet_cost_micros, 50_000);
    assert!(config.gpu_models.len() > 1);
    assert!(config.gpu_models.iter().any(|model| model == "L40S"));
    config.validate().unwrap();
}

#[tokio::test]
async fn honors_the_configurable_bandwidth_ceiling() {
    let bw_offer = |id: u64, inet: f64| {
        json!({
            "id": id, "machine_id": id * 10, "gpu_name": "L40S", "gpu_ram": 46068,
            "dph_total": 0.50, "inet_down_cost": inet, "inet_up_cost": inet,
            "verification": "verified", "reliability": 0.999, "rentable": true,
            "rented": false, "direct_port_count": 2, "cuda_max_good": 12.4,
            "num_gpus": 1, "gpu_arch": "nvidia", "cpu_arch": "amd64"
        })
    };
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/bundles/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            // both within the $0.05 ceiling; the old free-bandwidth-only rule
            // would have rejected the second, non-zero one.
            "offers": [bw_offer(1, 0.0), bw_offer(2, 0.04)]
        })))
        .mount(&server)
        .await;

    let vast = VastClient::new(
        VastConfig {
            api_url: Url::parse(&format!("{}/api/v0/", server.uri())).unwrap(),
            max_inet_cost_micros: 50_000,
            ..VastConfig::default()
        },
        ApiToken::new(TOKEN).unwrap(),
    )
    .unwrap();
    let offers = vast.offers().await.unwrap();
    assert_eq!(offers.iter().map(|o| o.id).collect::<Vec<_>>(), vec![1, 2]);

    // A host over the ceiling drops out of the search and is refused outright
    // once it is the offer being paid for.
    let over = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/bundles/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "offers": [bw_offer(1, 0.0), bw_offer(2, 0.10)]
        })))
        .mount(&over)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v0/asks/2/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"new_contract": 99})))
        .expect(0)
        .mount(&over)
        .await;
    let vast = VastClient::new(
        VastConfig {
            api_url: Url::parse(&format!("{}/api/v0/", over.uri())).unwrap(),
            max_inet_cost_micros: 50_000,
            ..VastConfig::default()
        },
        ApiToken::new(TOKEN).unwrap(),
    )
    .unwrap();
    let offers = vast.offers().await.unwrap();
    assert_eq!(offers.iter().map(|o| o.id).collect::<Vec<_>>(), vec![1]);

    let refused = vast
        .launch_workspace(WorkspaceLaunchRequest {
            workload_id: "job_123".to_owned(),
            image: IMAGE.to_owned(),
            max_hourly_micros: 640_000,
            rejected_machine_ids: Vec::new(),
            required_offer: quoted_offer(2, 20, 500_000),
        })
        .await
        .unwrap_err();
    assert!(matches!(refused, VastError::InvalidResponse { .. }));
    over.verify().await;
}

#[tokio::test]
async fn one_nonconforming_offer_does_not_empty_the_search() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/bundles/"))
        .and(body_partial_json(json!({"order": [["dph_total", "asc"]]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "offers": [offer(1, 10, 0.59), {"id": 2, "machine_id": 20, "gpu_name": "L40S",
                "gpu_ram": 46068, "dph_total": 0.50, "inet_down_cost": 0.10,
                "inet_up_cost": 0.0, "verification": "verified", "reliability": 0.999,
                "rentable": true, "rented": false, "direct_port_count": 2,
                "cuda_max_good": 12.4, "num_gpus": 1, "gpu_arch": "nvidia",
                "cpu_arch": "amd64"}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let offers = client(&server, 640_000).offers().await.unwrap();
    assert_eq!(offers.iter().map(|o| o.id).collect::<Vec<_>>(), vec![1]);
    server.verify().await;
}

#[tokio::test]
async fn an_offer_hiding_its_bandwidth_cost_is_dropped_from_the_search() {
    // Bandwidth cost must be stated and within the ceiling. An offer that
    // omits it entirely is refused, not admitted as if transfer were free —
    // otherwise a host could hide a per-GB charge to undercut the ranked
    // hourly price. The conforming offer beside it still survives, so one
    // hidden cost does not empty the search.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/bundles/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "offers": [offer(1, 10, 0.59), {
                "id": 2, "machine_id": 20, "gpu_name": "L40S", "gpu_ram": 46068,
                "dph_total": 0.50, "verification": "verified", "reliability": 0.999,
                "rentable": true, "rented": false, "direct_port_count": 2,
                "cuda_max_good": 12.4, "num_gpus": 1, "gpu_arch": "nvidia",
                "cpu_arch": "amd64"
            }]
        })))
        .mount(&server)
        .await;

    let offers = client(&server, 640_000).offers().await.unwrap();
    assert_eq!(offers.iter().map(|o| o.id).collect::<Vec<_>>(), vec![1]);
}

#[tokio::test]
async fn rejects_nonconforming_offer_facts_before_spending() {
    let cases = [
        ("verification", Some(json!("unverified"))),
        ("verification", None),
        ("reliability", Some(json!(0.989_999))),
        ("reliability", Some(json!(1.000_001))),
        ("reliability", None),
        ("rentable", Some(json!(false))),
        ("rentable", None),
        ("rented", Some(json!(true))),
        ("rented", None),
        ("direct_port_count", Some(json!(0))),
        ("direct_port_count", None),
        ("cuda_max_good", Some(json!(12.3))),
        ("cuda_max_good", None),
        ("num_gpus", Some(json!(2))),
        ("gpu_arch", Some(json!("amd"))),
        ("cpu_arch", Some(json!("arm64"))),
        ("inet_down_cost", Some(json!(0.06))),
        ("inet_down_cost", None),
        ("inet_up_cost", Some(json!(0.06))),
        ("inet_up_cost", None),
    ];

    for (field, value) in cases {
        let server = MockServer::start().await;
        let mut candidate = offer(7, 70, 0.59);
        match value {
            Some(value) => candidate[field] = value,
            None => {
                candidate.as_object_mut().unwrap().remove(field);
            }
        }
        Mock::given(method("POST"))
            .and(path("/api/v0/bundles/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"offers": [candidate]})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/api/v0/asks/7/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"new_contract": 99})))
            .expect(0)
            .mount(&server)
            .await;

        let result = client(&server, 640_000)
            .launch_workspace(WorkspaceLaunchRequest {
                workload_id: "job_123".to_owned(),
                image: IMAGE.to_owned(),
                max_hourly_micros: 640_000,
                rejected_machine_ids: Vec::new(),
                required_offer: quoted_offer(7, 70, 590_000),
            })
            .await;
        assert!(
            matches!(result, Err(VastError::InvalidResponse { .. })),
            "nonconforming {field} was admitted: {result:?}"
        );
        server.verify().await;
    }
}

#[tokio::test]
async fn unregistered_workspace_images_fail_before_provider_requests() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/bundles/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"offers": []})))
        .expect(0)
        .mount(&server)
        .await;
    let image =
        "registry.example/other@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    let error = client(&server, 640_000)
        .launch_workspace(WorkspaceLaunchRequest {
            workload_id: "job_123".to_owned(),
            image: image.to_owned(),
            max_hourly_micros: 640_000,
            rejected_machine_ids: Vec::new(),
            required_offer: quoted_offer(7, 70, 590_000),
        })
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        VastError::InvalidRequest("workspace image CUDA compatibility is not registered")
    ));
    server.verify().await;
}

#[tokio::test]
async fn enforces_the_cap_before_any_create_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/bundles/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "offers": [offer(1, 10, 0.640001)]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v0/asks/1/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"new_contract": 99})))
        .expect(0)
        .mount(&server)
        .await;

    let error = client(&server, 900_000)
        .launch(launch_request(640_000, quoted_offer(1, 10, 640_001)))
        .await
        .unwrap_err();
    assert!(matches!(error, VastError::NoCapacity));
    server.verify().await;
}

#[tokio::test]
async fn refuses_a_changed_quote_before_create() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/bundles/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "offers": [offer(7, 70, 0.60)]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v0/asks/7/"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let error = client(&server, 640_000)
        .launch(launch_request(640_000, quoted_offer(7, 70, 590_000)))
        .await
        .unwrap_err();
    assert!(matches!(error, VastError::OfferChanged));
    server.verify().await;
}

#[tokio::test]
async fn launches_digest_pinned_ssh_direct_capacity_and_attaches_the_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/bundles/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "offers": [offer(7, 70, 0.59)]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v0/asks/7/"))
        .and(body_json(json!({
            "image": IMAGE,
            "label": "covenant-workload-job_123",
            "disk": 16,
            "runtype": "ssh_direct",
            "cancel_unavail": true
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"new_contract": 99})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v0/instances/99/ssh/"))
        .and(body_json(json!({"ssh_key": SSH_KEY})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let launch = client(&server, 640_000)
        .launch(launch_request(640_000, quoted_offer(7, 70, 590_000)))
        .await
        .unwrap();
    assert_eq!(launch.instance_id, 99);
    assert_eq!(launch.offer.id, 7);
    assert_eq!(launch.label, "covenant-workload-job_123");
    server.verify().await;
}

#[tokio::test]
async fn cleans_up_when_key_attachment_fails() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/bundles/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "offers": [offer(7, 70, 0.59)]
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v0/asks/7/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"new_contract": 99})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v0/instances/99/ssh/"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v0/instances/99/"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let error = client(&server, 640_000)
        .launch(launch_request(640_000, quoted_offer(7, 70, 590_000)))
        .await
        .unwrap_err();
    assert!(matches!(error, VastError::SshKeyAttachment(_)));
    server.verify().await;
}

#[tokio::test]
async fn a_failed_cleanup_after_a_failed_attach_names_the_billing_instance() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/bundles/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "offers": [offer(7, 70, 0.59)]
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v0/asks/7/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"new_contract": 99})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v0/instances/99/ssh/"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v0/instances/99/"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;

    let error = client(&server, 640_000)
        .launch(launch_request(640_000, quoted_offer(7, 70, 590_000)))
        .await
        .unwrap_err();
    // A create we could neither key nor destroy is a certain billing leak;
    // the error must name the instance so it can be reconciled by id.
    assert!(matches!(
        error,
        VastError::AttachAndCleanupFailed {
            instance_id: 99,
            ..
        }
    ));
    assert!(error.to_string().contains("99"));
    server.verify().await;
}

#[tokio::test]
async fn launches_the_explicit_jupyter_workspace_profile() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/bundles/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "offers": [offer(7, 70, 0.59)]
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v0/asks/7/"))
        .and(body_json(json!({
            "image": IMAGE,
            "label": "covenant-workload-job_123",
            "disk": 16,
            "runtype": "jupyter_direct",
            "use_jupyter_lab": true,
            "jupyter_dir": "/workspace",
            "cancel_unavail": true
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "new_contract": 99
        })))
        .expect(1)
        .mount(&server)
        .await;

    let launch = client(&server, 640_000)
        .launch_workspace(WorkspaceLaunchRequest {
            workload_id: "job_123".to_owned(),
            image: IMAGE.to_owned(),
            max_hourly_micros: 640_000,
            rejected_machine_ids: Vec::new(),
            required_offer: quoted_offer(7, 70, 590_000),
        })
        .await
        .unwrap();
    assert_eq!(launch, workspace_launch());
    server.verify().await;
}

#[tokio::test]
async fn exposes_a_redacted_workspace_access_url_only_when_ready() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v0/instances/99/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(workspace_instance()))
        .mount(&server)
        .await;

    let facts = client(&server, 640_000)
        .workspace(&workspace_launch())
        .await
        .unwrap();
    assert!(facts.ready);
    let access = facts.access.unwrap();
    assert_eq!(format!("{access:?}"), "WorkspaceAccessUrl([REDACTED])");
    assert_eq!(access.to_string(), "[REDACTED]");
    assert_eq!(
        access.expose_secret().as_str(),
        "https://203.0.113.7:32123/lab?token=0123456789abcdef0123456789abcdef"
    );
}

#[tokio::test]
async fn workspace_is_ready_without_bundle_id() {
    // Vast stopped returning bundle_id on instance show (null since 2026-08);
    // readiness must not depend on it.
    let server = MockServer::start().await;
    let mut body = workspace_instance();
    body["instances"]["bundle_id"] = serde_json::Value::Null;
    Mock::given(method("GET"))
        .and(path("/api/v0/instances/99/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let facts = client(&server, 640_000)
        .workspace(&workspace_launch())
        .await
        .unwrap();
    assert!(facts.ready);
    assert!(facts.access.is_some());
}

#[tokio::test]
async fn missing_workspace_access_facts_remain_loading() {
    for field in ["public_ipaddr", "jupyter_token"] {
        let server = MockServer::start().await;
        let mut body = workspace_instance();
        body["instances"].as_object_mut().unwrap().remove(field);
        Mock::given(method("GET"))
            .and(path("/api/v0/instances/99/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let facts = client(&server, 640_000)
            .workspace(&workspace_launch())
            .await
            .unwrap();
        assert_eq!(facts.status, "loading", "missing {field}");
        assert!(!facts.ready, "missing {field}");
        assert!(facts.access.is_none(), "missing {field}");
    }
}

#[tokio::test]
async fn falls_back_to_v1_for_v0_port_arrays_without_inferring_a_port() {
    let server = MockServer::start().await;
    let mut v0 = workspace_instance();
    v0["instances"]["ports"] = json!([8080, 22]);
    Mock::given(method("GET"))
        .and(path("/api/v0/instances/99/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(v0))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/instances/"))
        .and(query_param("select_filters", r#"{"id":{"eq":99}}"#))
        .and(query_param("select_cols", r#"["*"]"#))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "instances": [workspace_instance()["instances"].clone()]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let facts = client(&server, 640_000)
        .workspace(&workspace_launch())
        .await
        .unwrap();
    assert!(facts.ready);
    assert_eq!(facts.access.unwrap().expose_secret().port(), Some(32123));
    server.verify().await;
}

#[tokio::test]
async fn v1_fallback_requires_exact_identity_and_a_real_mapping() {
    for mutation in ["wrong_label", "missing_mapping"] {
        let server = MockServer::start().await;
        let mut v0 = workspace_instance();
        v0["instances"]["ports"] = json!([8080, 22]);
        Mock::given(method("GET"))
            .and(path("/api/v0/instances/99/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(v0))
            .mount(&server)
            .await;

        let mut v1_instance = workspace_instance()["instances"].clone();
        if mutation == "wrong_label" {
            v1_instance["label"] = json!("covenant-workload-other");
        } else {
            v1_instance.as_object_mut().unwrap().remove("ports");
        }
        Mock::given(method("GET"))
            .and(path("/api/v1/instances/"))
            .and(query_param("select_filters", r#"{"id":{"eq":99}}"#))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "instances": [v1_instance]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let result = client(&server, 640_000)
            .workspace(&workspace_launch())
            .await;
        if mutation == "wrong_label" {
            assert!(matches!(
                result.unwrap_err(),
                VastError::InvalidResponse { .. }
            ));
        } else {
            let facts = result.unwrap();
            assert_eq!(facts.status, "loading");
            assert!(!facts.ready);
            assert!(facts.access.is_none());
        }
        server.verify().await;
    }
}

#[tokio::test]
async fn malformed_workspace_access_facts_fail_closed_without_leaking_tokens() {
    let cases = [
        ("public_ipaddr", json!("not-an-ip")),
        ("ports", json!({"8080/tcp": [{"HostPort": "70000"}]})),
        ("jupyter_token", json!("secret token must not appear")),
    ];
    for (field, value) in cases {
        let server = MockServer::start().await;
        let mut body = workspace_instance();
        body["instances"][field] = value;
        Mock::given(method("GET"))
            .and(path("/api/v0/instances/99/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let rendered = format!(
            "{:#}",
            client(&server, 640_000)
                .workspace(&workspace_launch())
                .await
                .unwrap_err()
        );
        assert!(!rendered.contains("secret token must not appear"));
        assert!(rendered.contains("invalid"));
    }
}

#[tokio::test]
async fn workspace_identity_mismatches_fail_closed() {
    for (field, value) in [
        ("label", json!("covenant-workload-other")),
        ("bundle_id", json!(8)),
        ("image_uuid", json!("registry.example/other@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")),
        ("image_runtype", json!("ssh_direct")),
        ("gpu_name", json!("RTX 4090")),
        ("dph_total", json!(0.60)),
    ] {
        let server = MockServer::start().await;
        let mut body = workspace_instance();
        body["instances"][field] = value;
        Mock::given(method("GET"))
            .and(path("/api/v0/instances/99/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        assert!(matches!(
            client(&server, 640_000)
                .workspace(&workspace_launch())
                .await
                .unwrap_err(),
            VastError::InvalidResponse { .. }
        ));
    }
}

#[tokio::test]
async fn exposes_booting_and_ready_access_facts() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v0/instances/11/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "instances": {"id": 11, "actual_status": "running", "gpu_name": "L40S"}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v0/instances/12/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "instances": {
                "actual_status": "running", "gpu_name": "L40S", "gpu_ram": 46068,
                "verification": "verified", "dph_total": 0.5363,
                "ssh_host": "ssh1.vast.ai", "ssh_port": 18004,
                "direct_port_start": -1, "machine_id": 24733
            }
        })))
        .mount(&server)
        .await;

    let client = client(&server, 640_000);
    let loading = client.instance(11).await.unwrap();
    assert_eq!(loading.status, "loading");
    assert!(!loading.ready);

    let ready = client.instance(12).await.unwrap();
    assert!(ready.ready);
    assert_eq!(
        ready.ssh,
        Some(SshAccess {
            host: "ssh1.vast.ai".to_owned(),
            port: 18004
        })
    );
    assert_eq!(ready.direct_ports_available, Some(false));
}

#[tokio::test]
async fn recovers_exact_labels_newest_first() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/instances/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "instances": [
                {"id": 8, "label": "covenant-workload-job_123"},
                {"id": 12, "label": "covenant-workload-job_123"},
                {"id": 9, "label": "other"}
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let found = client(&server, 640_000).recover("job_123").await.unwrap();
    assert_eq!(found, vec![12, 8]);
}

#[tokio::test]
async fn recovery_rejects_an_unsuccessful_provider_lookup() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/instances/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": false,
            "instances": []
        })))
        .expect(1)
        .mount(&server)
        .await;

    assert!(matches!(
        client(&server, 640_000)
            .recover("job_123")
            .await
            .unwrap_err(),
        VastError::InvalidResponse { .. }
    ));
    server.verify().await;
}

#[tokio::test]
async fn destroy_is_idempotent_for_missing_or_gone_instances() {
    let server = MockServer::start().await;
    for (instance_id, status) in [(40, 404), (41, 410), (42, 204)] {
        Mock::given(method("DELETE"))
            .and(path(format!("/api/v0/instances/{instance_id}/")))
            .respond_with(ResponseTemplate::new(status))
            .expect(1)
            .mount(&server)
            .await;
    }
    let client = client(&server, 640_000);
    client.destroy(40).await.unwrap();
    client.destroy(41).await.unwrap();
    client.destroy(42).await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn a_refused_destroy_is_not_reported_as_destroyed() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v0/instances/40/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": false, "msg": "instance is not owned by this account"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v0/instances/41/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": true})))
        .expect(1)
        .mount(&server)
        .await;

    let client = client(&server, 640_000);
    assert!(matches!(
        client.destroy(40).await.unwrap_err(),
        VastError::InvalidResponse {
            reason: "provider did not destroy the instance",
            ..
        }
    ));
    client.destroy(41).await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn a_refused_ssh_attachment_destroys_the_instance() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/bundles/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "offers": [offer(7, 70, 0.59)]
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v0/asks/7/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"new_contract": 99})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v0/instances/99/ssh/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": false})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v0/instances/99/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": true})))
        .expect(1)
        .mount(&server)
        .await;

    let error = client(&server, 640_000)
        .launch(launch_request(640_000, quoted_offer(7, 70, 590_000)))
        .await
        .unwrap_err();
    assert!(matches!(error, VastError::SshKeyAttachment(_)));
    server.verify().await;
}

#[tokio::test]
async fn a_refused_create_is_a_provider_refusal_not_a_decode_failure() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/bundles/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "offers": [offer(7, 70, 0.59)]
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v0/asks/7/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": false, "msg": "no such ask"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let error = client(&server, 640_000)
        .launch_workspace(WorkspaceLaunchRequest {
            workload_id: "job_123".to_owned(),
            image: IMAGE.to_owned(),
            max_hourly_micros: 640_000,
            rejected_machine_ids: Vec::new(),
            required_offer: quoted_offer(7, 70, 590_000),
        })
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        VastError::Refused {
            operation: "workspace creation"
        }
    ));
    server.verify().await;
}

#[tokio::test]
async fn a_lost_create_response_is_flagged_as_uncertain_by_label() {
    // A create whose reply is lost may have provisioned a billing instance
    // whose id we never learned. Each of these is a 2xx that leaves the id
    // unknown, so the launch fails naming the label to reconcile the orphan by
    // rather than reporting that nothing was rented.
    let lost = [
        ResponseTemplate::new(200).set_body_string("not json at all"),
        ResponseTemplate::new(200).set_body_json(json!({"success": true})),
        ResponseTemplate::new(200).set_body_json(json!({})),
        ResponseTemplate::new(200).set_body_bytes(vec![b'x'; MAX_RESPONSE_BYTES + 1]),
    ];
    for response in lost {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v0/bundles/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "offers": [offer(7, 70, 0.59)]
            })))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/api/v0/asks/7/"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;

        let error = client(&server, 640_000)
            .launch(launch_request(640_000, quoted_offer(7, 70, 590_000)))
            .await
            .unwrap_err();
        assert!(
            matches!(&error, VastError::CreateUncertain { label } if label == "covenant-workload-job_123"),
            "a lost create response must flag the billing label: {error:?}"
        );
        server.verify().await;
    }
}

#[tokio::test]
async fn a_client_error_on_create_created_nothing_and_is_not_uncertain() {
    // A 4xx is the server rejecting the request before acting on it — a bad
    // body, a stale offer, a throttle. Nothing was created, so it keeps its own
    // status rather than being dressed as a maybe-billing leak. A genuine
    // capacity refusal is a 200 with `success: false`, handled elsewhere.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/bundles/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "offers": [offer(7, 70, 0.59)]
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v0/asks/7/"))
        .respond_with(ResponseTemplate::new(400))
        .expect(1)
        .mount(&server)
        .await;

    let error = client(&server, 640_000)
        .launch(launch_request(640_000, quoted_offer(7, 70, 590_000)))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            VastError::UnexpectedStatus {
                operation: "instance creation",
                ..
            }
        ),
        "got: {error:?}"
    );
    server.verify().await;
}

#[tokio::test]
async fn a_server_error_on_create_is_uncertain_and_flags_the_billing_label() {
    // A 5xx is not a rejection: the origin may have committed the instance
    // before failing, or a gateway timed out on an origin that already did. The
    // machine could be billing with its id unknown to us, so the create surfaces
    // as `CreateUncertain` naming the workload label — the trace an operator
    // reconciles by — rather than a clean failure that walks on to rent a
    // second one.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/bundles/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "offers": [offer(7, 70, 0.59)]
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v0/asks/7/"))
        .respond_with(ResponseTemplate::new(502))
        .expect(1)
        .mount(&server)
        .await;

    let error = client(&server, 640_000)
        .launch(launch_request(640_000, quoted_offer(7, 70, 590_000)))
        .await
        .unwrap_err();
    assert!(
        matches!(&error, VastError::CreateUncertain { label } if label == "covenant-workload-job_123"),
        "a 5xx create must flag the billing label: {error:?}"
    );
    server.verify().await;
}

#[test]
fn create_response_lost_splits_client_and_server_errors() {
    // The classification the two create paths lean on: a 4xx acted on nothing,
    // every 5xx (including the gateway codes a proxy in front of the origin
    // returns after a committed create) is a possible orphan.
    let lost = |code: u16| {
        create_response_lost(&VastError::UnexpectedStatus {
            operation: "instance creation",
            status: reqwest::StatusCode::from_u16(code).unwrap(),
        })
    };
    for created_nothing in [400, 401, 403, 404, 409, 422, 429] {
        assert!(!lost(created_nothing), "{created_nothing} created nothing");
    }
    for uncertain in [500, 502, 503, 504] {
        assert!(lost(uncertain), "{uncertain} may have provisioned");
    }
}

#[tokio::test]
async fn a_create_connect_failure_created_nothing_and_is_not_uncertain() {
    // A connect failure never reached the server, so there is no orphan to
    // reconcile. `launch` surveys before it creates, so `create` is exercised
    // directly here to isolate the connect leg from the survey.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let client = VastClient::new(
        VastConfig {
            api_url: Url::parse(&format!("http://127.0.0.1:{port}/api/v0/")).unwrap(),
            ..VastConfig::default()
        },
        ApiToken::new(TOKEN).unwrap(),
    )
    .unwrap();

    let error = client
        .create(7, IMAGE, "covenant-workload-job_123")
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            VastError::Transport {
                operation: "instance creation",
                ..
            }
        ),
        "got: {error:?}"
    );
    assert!(
        !create_response_lost(&error),
        "a connect failure created nothing to reconcile"
    );
}

#[tokio::test]
async fn a_vanished_instance_reads_as_terminally_absent() {
    for body in [json!({"instances": null}), json!({})] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v0/instances/99/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = client(&server, 640_000);
        let facts = client.workspace(&workspace_launch()).await.unwrap();
        assert_eq!(facts.status, "deleted");
        assert!(!facts.ready);
        assert!(facts.access.is_none());

        let facts = client.instance(99).await.unwrap();
        assert_eq!(facts.status, "deleted");
        assert!(!facts.ready);
    }

    let missing = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v0/instances/99/"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&missing)
        .await;
    let facts = client(&missing, 640_000)
        .workspace(&workspace_launch())
        .await
        .unwrap();
    assert_eq!(facts.status, "deleted");
    assert!(!facts.ready);
}

#[tokio::test]
async fn a_non_object_instance_payload_is_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v0/instances/99/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"instances": []})))
        .mount(&server)
        .await;

    assert!(matches!(
        client(&server, 640_000)
            .workspace(&workspace_launch())
            .await
            .unwrap_err(),
        VastError::InvalidResponse {
            reason: "instance is not an object",
            ..
        }
    ));
}

#[tokio::test]
async fn recovery_reads_one_bounded_page_and_fails_closed_when_truncated() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/instances/"))
        .and(query_param("limit", "25"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "next_token": "eyJpZCI6IDh9",
            "instances": [{"id": 8, "label": "covenant-workload-job_123"}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    assert!(matches!(
        client(&server, 640_000)
            .recover("job_123")
            .await
            .unwrap_err(),
        VastError::InvalidResponse {
            reason: "instance list was truncated",
            ..
        }
    ));
    server.verify().await;
}

#[tokio::test]
async fn a_harmless_reprice_and_a_rewritten_registry_keep_the_workspace() {
    let server = MockServer::start().await;
    let mut body = workspace_instance();
    body["instances"]["dph_total"] = json!(0.50);
    body["instances"]["image_uuid"] = json!(IMAGE.trim_start_matches("docker.io/"));
    Mock::given(method("GET"))
        .and(path("/api/v0/instances/99/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let facts = client(&server, 640_000)
        .workspace(&workspace_launch())
        .await
        .unwrap();
    assert!(facts.ready);
    assert_eq!(facts.hourly_micros, 500_000);
}

#[tokio::test]
async fn a_reprice_above_the_quote_is_refused_as_an_overcharge() {
    // A reprice below the quote is harmless (above); a reprice above it is an
    // overcharge. Here the instance comes back at 600_000/hr — under the
    // client's 640_000 ceiling but over the 590_000 the customer was quoted —
    // so the readiness read fails closed rather than hand back an access URL
    // for a machine that bills more than the customer agreed to.
    let server = MockServer::start().await;
    let mut body = workspace_instance();
    body["instances"]["dph_total"] = json!(0.60);
    Mock::given(method("GET"))
        .and(path("/api/v0/instances/99/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    assert!(matches!(
        client(&server, 640_000)
            .workspace(&workspace_launch())
            .await
            .unwrap_err(),
        VastError::InvalidResponse {
            reason: "workspace price is above the quote",
            ..
        }
    ));
}

#[tokio::test]
async fn readiness_does_not_require_direct_ports() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v0/instances/13/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "instances": {
                "actual_status": "running", "gpu_name": "L40S", "gpu_ram": 46068,
                "verification": "verified", "dph_total": 0.5363,
                "ssh_host": "ssh1.vast.ai", "ssh_port": 18004, "machine_id": 24733
            }
        })))
        .mount(&server)
        .await;

    let facts = client(&server, 640_000).instance(13).await.unwrap();
    assert!(facts.ready);
    assert_eq!(facts.direct_ports_available, None);
}

#[tokio::test]
async fn an_instance_running_without_an_ssh_address_is_not_yet_ready() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v0/instances/14/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "instances": {
                "actual_status": "running", "gpu_name": "L40S", "gpu_ram": 46068,
                "verification": "verified", "dph_total": 0.5363, "machine_id": 24733
            }
        })))
        .mount(&server)
        .await;

    let facts = client(&server, 640_000).instance(14).await.unwrap();
    // Running but no ssh_host/ssh_port yet: the box is still coming up.
    // Reporting it ready would make a broker abandon a live, billing rental.
    assert!(!facts.ready);
    assert_eq!(facts.status, "loading");
    assert_eq!(facts.ssh, None);
}

#[tokio::test]
async fn a_mismatched_instance_id_is_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v0/instances/15/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "instances": {
                "id": 16, "actual_status": "running", "gpu_name": "L40S", "gpu_ram": 46068,
                "verification": "verified", "dph_total": 0.5363,
                "ssh_host": "ssh1.vast.ai", "ssh_port": 18004, "machine_id": 24733
            }
        })))
        .mount(&server)
        .await;

    let error = client(&server, 640_000).instance(15).await.unwrap_err();
    assert!(matches!(
        error,
        VastError::InvalidResponse {
            reason: "instance ID does not match",
            ..
        }
    ));
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/bundles/"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/captured", server.uri())),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/captured"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"offers": []})))
        .expect(0)
        .mount(&server)
        .await;

    let error = client(&server, 640_000).offers().await.unwrap_err();
    assert!(matches!(
        error,
        VastError::UnexpectedStatus {
            status: StatusCode::FOUND,
            ..
        }
    ));
    server.verify().await;
}

#[tokio::test]
async fn errors_never_include_credentials_or_provider_bodies() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/bundles/"))
        .respond_with(
            ResponseTemplate::new(500)
                .set_body_string(format!("provider echoed credential: {TOKEN}")),
        )
        .mount(&server)
        .await;
    // A decode failure carries provider values, so it is asserted on {:?}:
    // thiserror ignores the alternate flag and would hide a source-borne leak.
    Mock::given(method("GET"))
        .and(path("/api/v0/instances/99/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "instances": {"jupyter_token": 12345, "label": "secret token must not appear"}
        })))
        .mount(&server)
        .await;

    let client = client(&server, 640_000);
    let rendered = format!("{:?}", client.offers().await.unwrap_err());
    assert!(!rendered.contains(TOKEN));
    assert!(!rendered.contains("provider echoed"));
    assert!(!format!("{client:?}").contains(TOKEN));

    let decode = client.workspace(&workspace_launch()).await.unwrap_err();
    assert!(matches!(decode, VastError::Decode { .. }));
    assert!(!format!("{decode:?}").contains("secret token must not appear"));
}

#[tokio::test]
async fn malformed_offers_are_dropped_from_the_search() {
    let cases = [
        json!({"offers": [offer(0, 10, 0.50)]}),
        json!({"offers": [offer(1, 0, 0.50)]}),
        json!({"offers": [{
            "id": 1, "machine_id": 10, "gpu_name": "", "gpu_ram": 46068,
            "dph_total": 0.50
        }]}),
        json!({"offers": [offer(1, 10, -0.50)]}),
    ];
    for body in cases {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v0/bundles/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        assert!(client(&server, 640_000).offers().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn rejects_invalid_access_facts_and_oversized_responses() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v0/instances/11/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "instances": {
                "actual_status": "running", "ssh_host": "ssh.vast.ai;invalid",
                "ssh_port": 22
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v0/instances/12/"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; MAX_RESPONSE_BYTES + 1]))
        .mount(&server)
        .await;

    let client = client(&server, 640_000);
    assert!(matches!(
        client.instance(11).await.unwrap_err(),
        VastError::InvalidResponse { .. }
    ));
    assert!(matches!(
        client.instance(12).await.unwrap_err(),
        VastError::ResponseTooLarge { .. }
    ));
}

#[test]
fn rejects_mutable_images_unsafe_ids_and_invalid_keys() {
    assert!(validate_digest_pinned_image("registry.example/app:latest").is_err());
    assert!(validate_digest_pinned_image(IMAGE).is_ok());
    assert!(workspace_label("../job").is_err());
    assert_eq!(
        workspace_label("job_123").unwrap(),
        "covenant-workload-job_123"
    );
    assert!(validate_ssh_public_key("ssh-ed25519 short").is_err());
    assert!(validate_ssh_public_key(SSH_KEY).is_ok());
}

#[test]
fn a_launch_cap_or_exclusion_list_out_of_range_is_refused_before_any_request() {
    // The per-workload cap and the machine-exclusion list are checked before
    // a launch reaches the provider. Every prior test passes a valid cap and
    // an empty list, so the fail-closed edges never ran: a zero or oversized
    // cap that would disable or defeat the ceiling, and an exclusion list past
    // the bound or naming machine 0 (never a real id), are all refused here.
    validate_cap_and_rejections(1_000_000, &[]).unwrap();
    validate_cap_and_rejections(MAX_HOURLY_MICROS, &[42]).unwrap();

    for bad_cap in [0, MAX_HOURLY_MICROS + 1] {
        assert!(matches!(
            validate_cap_and_rejections(bad_cap, &[]),
            Err(VastError::InvalidRequest(_))
        ));
    }
    assert!(matches!(
        validate_cap_and_rejections(1_000_000, &[0]),
        Err(VastError::InvalidRequest(_))
    ));
    let too_many: Vec<u64> = (1..=1_025).collect();
    assert!(matches!(
        validate_cap_and_rejections(1_000_000, &too_many),
        Err(VastError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn ranked_offers_return_the_cheapest_capacity_within_the_limit() {
    // Every other ranking test leaves at most one offer standing, so the order
    // the buyer actually chooses from is unpinned. The provider is asked for
    // cheapest-first but returns them shuffled here; ranking must sort by price
    // and only then apply the limit, so the survivors are the cheapest offers
    // rather than whichever the provider happened to list first. Truncating
    // before sorting would drop cheaper capacity and hand the buyer a dearer
    // rental. A price tie breaks by offer id to keep the choice deterministic.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/bundles/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "offers": [
                offer(1, 10, 0.62),
                offer(5, 50, 0.60),
                offer(4, 40, 0.64),
                offer(2, 20, 0.58),
                offer(3, 30, 0.60)
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let ranked = client(&server, 700_000)
        .ranked_offers(3, &[], 700_000)
        .await
        .unwrap();
    assert_eq!(
        ranked
            .offers
            .iter()
            .map(|offer| offer.id)
            .collect::<Vec<_>>(),
        vec![2, 3, 5]
    );
    assert_eq!(
        ranked
            .offers
            .iter()
            .map(|offer| offer.hourly_micros)
            .collect::<Vec<_>>(),
        vec![580_000, 600_000, 600_000]
    );
    assert_eq!(
        ranked.survey,
        OfferSurvey {
            returned: 5,
            admitted: 3,
            ..OfferSurvey::default()
        }
    );
}

#[test]
fn hourly_prices_round_up_and_refuse_unrepresentable_values() {
    // Every provider price crosses into micros through this one conversion. A
    // fractional dollar is rounded up, never down, so the buyer's escrow ceiling
    // can only ever meet or exceed what Vast bills; flooring would leak the
    // sub-micro remainder on every rental. Only the negative-price edge is
    // exercised elsewhere, through a dropped offer, so the rounding direction and
    // the non-finite and overflow refusals are pinned directly here.
    assert_eq!(hourly_micros(0.5), Ok(500_000));
    assert_eq!(hourly_micros(0.500_000_1), Ok(500_001));
    assert_eq!(hourly_micros(0.999_999_9), Ok(1_000_000));

    for refused in [0.0, -0.01, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(hourly_micros(refused).is_err(), "{refused} was accepted");
    }
    // A price whose micros would not fit in u64 is refused, not saturated to the
    // ceiling and then admitted as if it were affordable.
    assert!(hourly_micros(2.0e13).is_err());
    assert!(hourly_micros(f64::MAX).is_err());
}

#[tokio::test]
async fn a_launch_onto_an_excluded_machine_spends_nothing() {
    // The exclusion list is the retry lever after a host misbehaves: the caller
    // relaunches naming the bad machine so it is not rented again. Ranking honors
    // it, but the binding gate is the confirm before create. Here the quoted
    // offer is still live at its exact terms, yet its machine is excluded, so the
    // launch fails with NoCapacity and never issues the create PUT.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/bundles/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "offers": [offer(7, 70, 0.59)]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v0/asks/7/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"new_contract": 99})))
        .expect(0)
        .mount(&server)
        .await;

    let request = LaunchRequest {
        rejected_machine_ids: vec![70],
        ..launch_request(640_000, quoted_offer(7, 70, 590_000))
    };
    let error = client(&server, 640_000).launch(request).await.unwrap_err();
    assert!(matches!(error, VastError::NoCapacity));
    server.verify().await;
}

#[tokio::test]
async fn recovery_collapses_repeats_and_fails_closed_on_a_zero_id() {
    // Recovery drives orphan cleanup: its ids become destroy targets after a
    // create whose reply was lost. A label listed twice must collapse to one id
    // so the same rental is not chased twice, and a zero id anywhere on the page
    // is not a destroyable instance, so the whole lookup fails closed rather than
    // handing back a nonsense target or silently trusting a malformed page.
    let repeated = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/instances/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "instances": [
                {"id": 8, "label": "covenant-workload-job_123"},
                {"id": 8, "label": "covenant-workload-job_123"}
            ]
        })))
        .mount(&repeated)
        .await;
    assert_eq!(
        client(&repeated, 640_000).recover("job_123").await.unwrap(),
        vec![8]
    );

    let zero = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/instances/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "instances": [{"id": 0, "label": "covenant-workload-job_123"}]
        })))
        .mount(&zero)
        .await;
    assert!(matches!(
        client(&zero, 640_000).recover("job_123").await.unwrap_err(),
        VastError::InvalidResponse {
            reason: "instance ID is zero",
            ..
        }
    ));
}
