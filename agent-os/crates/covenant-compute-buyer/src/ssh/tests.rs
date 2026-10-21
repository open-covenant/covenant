use super::*;
use base64::Engine as _;
use covenant_compute_protocol::ssh::SshHostKey;
use rustls::{pki_types::PrivatePkcs8KeyDer, ServerConfig};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_rustls::TlsAcceptor;

fn key(byte: u8) -> SshHostKey {
    let mut bytes = b"\0\0\0\x0bssh-ed25519\0\0\0\x20".to_vec();
    bytes.extend_from_slice(&[byte; 32]);
    SshHostKey {
        algorithm: "ssh-ed25519".into(),
        key_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
    }
}

fn access(port: u16) -> SshAccess {
    SshAccess {
        host: "localhost".into(),
        port,
        username: "covenant".into(),
        lease_generation: "test-instance".into(),
        transport: Some(SshTransport::Tls),
        observed_host_keys: vec![key(7)],
    }
}

fn certificates(host: &str, expired: bool) -> (Arc<ServerConfig>, Arc<ClientConfig>) {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut parameters = rcgen::CertificateParams::new(vec![host.into()]).unwrap();
    if expired {
        parameters.not_before = rcgen::date_time_ymd(2019, 1, 1);
        parameters.not_after = rcgen::date_time_ymd(2020, 1, 1);
    }
    let cert = parameters.self_signed(&key).unwrap();
    let mut roots = RootCertStore::empty();
    roots.add(cert.der().clone()).unwrap();
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
            )
            .unwrap();
    (Arc::new(config), Arc::new(tls_config(roots)))
}

async fn tls_server(
    host: &str,
    expired: bool,
) -> (
    SshAccess,
    Arc<ClientConfig>,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let (server, client) = certificates(host, expired);
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let access = access(listener.local_addr().unwrap().port());
    let applications = Arc::new(AtomicUsize::new(0));
    let count = applications.clone();
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        if let Ok(mut stream) = TlsAcceptor::from(server).accept(tcp).await {
            let mut bytes = [0u8; 4];
            if stream.read_exact(&mut bytes).await.is_ok() {
                count.fetch_add(1, Ordering::SeqCst);
                stream.write_all(&bytes).await.unwrap();
                stream.shutdown().await.unwrap();
            }
        }
    });
    (access, client, applications, task)
}

#[tokio::test]
async fn real_tls_accepts_exact_hostname_before_application_bytes() {
    let (access, config, applications, task) = tls_server("localhost", false).await;
    let mut stream = connect_tls(&access, config).await.unwrap();
    stream.write_all(b"ping").await.unwrap();
    let mut received = [0; 4];
    stream.read_exact(&mut received).await.unwrap();
    assert_eq!(&received, b"ping");
    task.await.unwrap();
    assert_eq!(applications.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn real_tls_rejects_wrong_hostname_expired_and_untrusted_certificates_before_payload() {
    for (hostname, expired, trusted) in [
        ("other.example", false, true),
        ("localhost", true, true),
        ("localhost", false, false),
    ] {
        let (access, mut config, applications, task) = tls_server(hostname, expired).await;
        if !trusted {
            config = SshClient::default().tls;
        }
        assert!(
            connect_tls(&access, config).await.is_err(),
            "{hostname}/{expired}/{trusted}"
        );
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(applications.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn provider_metadata_never_becomes_a_proxy_command_and_original_host_stays_pinned() {
    let access = access(443);
    let command = ssh_command(
        &access,
        "127.0.0.1",
        33333,
        Path::new("/private/buyer"),
        Path::new("/private/known hosts"),
    )
    .unwrap();
    let args: Vec<_> = command
        .as_std()
        .get_args()
        .map(|a| a.to_str().unwrap())
        .collect();
    assert!(args.contains(&"ProxyCommand=none"));
    assert!(args.contains(&"ProxyJump=none"));
    assert!(args.contains(&"HostKeyAlias=[localhost]:443"));
    assert!(args.contains(&"StrictHostKeyChecking=yes"));
    assert!(args.contains(&"IdentityAgent=none"));
    assert!(args.contains(&"UserKnownHostsFile=\"/private/known hosts\""));
    assert_eq!(&args[args.len() - 2..], ["--", "127.0.0.1"]);
}

#[test]
fn upload_path_expansion_traversal_and_shell_syntax_are_rejected() {
    for path in [
        "",
        "../secret",
        ".hidden",
        "a/b",
        "a b",
        "$HOME",
        "x;id",
        "$(id)",
        "'x'",
        "x\nid",
    ] {
        assert!(validate_basename(path).is_err(), "{path}");
    }
    validate_basename("covenant-123-v2.snapshot").unwrap();
}

#[tokio::test]
async fn output_limit_is_enforced() {
    assert!(bounded_output(&vec![0u8; OUTPUT_LIMIT as usize + 1][..])
        .await
        .is_err());
}

#[tokio::test]
async fn refused_current_lease_cannot_open_a_channel_or_read_plaintext() {
    use covenant_identity::LocalIdentity;
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let buyer = LocalIdentity::generate("ssh-gate@buyer");
    let mut scope = HardwareScope {
        profile: Default::default(),
        job_id: uuid::Uuid::new_v4(),
        operator_pubkey_b58: LocalIdentity::generate("ssh-gate@operator")
            .agent_id()
            .pubkey_base58(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        lease_generation: "closed-test".into(),
        host: "localhost".into(),
        port: listener.local_addr().unwrap().port(),
        username: "covenant".into(),
        transport: Some(SshTransport::Tls),
        guest_channel_key: key(7),
        workload_sha256: "a".repeat(64),
        lease_expires_at_ms: crate::epoch_ms() + 60_000,
    };
    let http = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let url = format!("http://{}", http.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    let router = axum::Router::new().fallback(move || {
        seen.fetch_add(1, Ordering::SeqCst);
        async { axum::http::StatusCode::CONFLICT }
    });
    let server = tokio::spawn(async move { axum::serve(http, router).await.unwrap() });
    let hardware = HardwareClient::new(&url, &buyer).unwrap();
    let error = SshClient::default()
        .upload_file(
            &hardware,
            &scope,
            TrustClass::Open,
            Path::new("/does-not-exist/private-key"),
            Path::new("/does-not-exist/plaintext"),
            "snapshot",
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("409"), "{error}");
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
    scope.lease_expires_at_ms = 1;
    assert!(SshClient::default()
        .probe(
            &hardware,
            &scope,
            TrustClass::Open,
            Path::new("/does-not-exist/private-key")
        )
        .await
        .is_err());
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    server.abort();
}

#[cfg(unix)]
mod openssh {
    use super::*;
    use std::process::{Child, Command as StdCommand};

    struct Server {
        directory: tempfile::TempDir,
        process: Child,
        port: u16,
        username: String,
        host_key: SshHostKey,
    }
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.process.kill();
            let _ = self.process.wait();
        }
    }
    fn generate_key(path: &Path) -> SshHostKey {
        assert!(StdCommand::new("/usr/bin/ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(path)
            .status()
            .unwrap()
            .success());
        let text = std::fs::read_to_string(path.with_extension("pub")).unwrap();
        let fields: Vec<_> = text.split_whitespace().collect();
        SshHostKey {
            algorithm: fields[0].into(),
            key_base64: fields[1].into(),
        }
    }

    impl Server {
        async fn start() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let host_key = generate_key(&directory.path().join("host"));
            generate_key(&directory.path().join("buyer"));
            generate_key(&directory.path().join("wrong-buyer"));
            let username = String::from_utf8(
                StdCommand::new("/usr/bin/id")
                    .arg("-un")
                    .output()
                    .unwrap()
                    .stdout,
            )
            .unwrap()
            .trim()
            .to_owned();
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            let configuration = format!(
                "Port {port}\nListenAddress 127.0.0.1\nHostKey {0}/host\nPidFile {0}/pid\nAuthorizedKeysFile {0}/buyer.pub\nStrictModes no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\nPermitRootLogin prohibit-password\nAllowUsers {username}\n",
                directory.path().display()
            );
            let config = directory.path().join("sshd_config");
            std::fs::write(&config, configuration).unwrap();
            let mut process = StdCommand::new("/usr/sbin/sshd")
                .args(["-D", "-e", "-f"])
                .arg(&config)
                .arg("-E")
                .arg(directory.path().join("sshd.log"))
                .spawn()
                .expect("install openssh-server to run this opt-in integration test");
            for _ in 0..50 {
                if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
                    return Self {
                        directory,
                        process,
                        port,
                        username,
                        host_key,
                    };
                }
                if process.try_wait().unwrap().is_some() {
                    panic!(
                        "sshd failed: {}",
                        std::fs::read_to_string(directory.path().join("sshd.log"))
                            .unwrap_or_default()
                    );
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let _ = process.kill();
            panic!("local sshd did not become ready");
        }
        fn access(&self) -> SshAccess {
            SshAccess {
                host: "127.0.0.1".into(),
                port: self.port,
                username: self.username.clone(),
                lease_generation: "local-sshd".into(),
                transport: None,
                observed_host_keys: vec![self.host_key.clone()],
            }
        }
    }

    // This uses a real OpenSSH server, TLS certificate validation, SSH host-key
    // verification and public-key authentication. It is not a provider test.
    // CI/live acceptance runners with openssh-server installed must run it with
    // `cargo test -p covenant-compute-buyer ssh::tests::openssh -- --ignored`.
    #[tokio::test]
    #[ignore = "requires /usr/sbin/sshd and /usr/bin/ssh-keygen; loopback only"]
    async fn real_tls_ssh_pin_buyer_auth_upload_and_deadline() {
        let sshd = Server::start().await;
        let buyer = sshd.directory.path().join("buyer");
        let direct = sshd.access();
        SshClient::default()
            .run(&direct, &buyer, "id -u; pwd", None, Duration::from_secs(5))
            .await
            .unwrap();

        let (server, client) = certificates("localhost", false);
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let mut access = access(listener.local_addr().unwrap().port());
        access.username = sshd.username.clone();
        access.observed_host_keys = vec![sshd.host_key.clone()];
        let port = sshd.port;
        let relay = tokio::spawn(async move {
            loop {
                let (tcp, _) = listener.accept().await.unwrap();
                let server = server.clone();
                tokio::spawn(async move {
                    if let Ok(mut tls) = TlsAcceptor::from(server).accept(tcp).await {
                        let mut remote = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
                        let _ = tokio::io::copy_bidirectional(&mut tls, &mut remote).await;
                    }
                });
            }
        });
        let client = SshClient { tls: client };
        let result = client
            .run(&access, &buyer, "id -u; pwd", None, Duration::from_secs(5))
            .await
            .unwrap();
        assert!(!result.stdout.is_empty());
        let mut wrong = access.clone();
        wrong.observed_host_keys = vec![key(11)];
        assert!(client
            .run(
                &wrong,
                &buyer,
                "echo should-not-run",
                None,
                Duration::from_secs(5)
            )
            .await
            .is_err());
        assert!(client
            .run(
                &access,
                &sshd.directory.path().join("wrong-buyer"),
                "echo should-not-run",
                None,
                Duration::from_secs(5)
            )
            .await
            .is_err());

        // Internal transport exercise uses an isolated local directory; public
        // upload_file accepts only /workspace/<validated basename>.
        let destination = sshd.directory.path().join("synthetic");
        let source = sshd.directory.path().join("source");
        std::fs::write(&source, b"synthetic workspace boundary\n").unwrap();
        let upload = upload_command(destination.to_str().unwrap());
        client
            .run(
                &access,
                &buyer,
                &upload,
                Some((open_source(&source).unwrap(), 29)),
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            b"synthetic workspace boundary\n"
        );
        assert!(client
            .run(
                &access,
                &buyer,
                &upload,
                Some((open_source(&source).unwrap(), 29)),
                Duration::from_secs(5)
            )
            .await
            .is_err());
        let symlink = sshd.directory.path().join("symlink");
        std::os::unix::fs::symlink(&destination, &symlink).unwrap();
        assert!(client
            .run(
                &access,
                &buyer,
                &upload_command(symlink.to_str().unwrap()),
                Some((open_source(&source).unwrap(), 29)),
                Duration::from_secs(5)
            )
            .await
            .is_err());
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            b"synthetic workspace boundary\n"
        );
        assert!(client
            .run(&access, &buyer, "sleep 5", None, Duration::from_millis(100))
            .await
            .is_err());
        client
            .run(&access, &buyer, "id -u; pwd", None, Duration::from_secs(5))
            .await
            .unwrap();
        relay.abort();
    }
}
