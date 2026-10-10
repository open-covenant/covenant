//! Builds inside a container: the agent sees the job's checkout and nothing
//! else of this machine, and its only network route is covguard's metering
//! proxy, which holds the model key.
//!
//! covguard still starts the proxy and supervises the run, but the "agent" it
//! launches is this node binary's hidden `__build-container` subcommand. That
//! wrapper starts a small forwarder container that can reach the proxy,
//! attaches it to an internal network with no other way out, and runs the
//! agent in the builder image on that network. On Linux the forwarder mounts
//! the unix socket covguard's sandbox bridges to the proxy through
//! (`COVGUARD_BRIDGE_SOCKET`); on macOS the container VM reaches the proxy's
//! loopback port, read from `ANTHROPIC_BASE_URL`. A builder image is the task's check image with Claude Code
//! added, so the agent runs its tests where the checker will.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

/// The subcommand covguard runs as its agent.
pub const BUILD_CONTAINER: &str = "__build-container";

/// The port the forwarder listens on inside the internal network.
const FORWARDER_PORT: u16 = 8080;

/// The proxy that setups and builds reach the package registries through,
/// and nothing else.
pub const EGRESS_IMAGE: &str = "covenant-compute-egress:1";
const EGRESS_PORT: u16 = 8888;
/// The registries an install may reach: Python, Node, Rust and Go.
const REGISTRIES: &[&str] = &[
    "pypi.org",
    "files.pythonhosted.org",
    "registry.npmjs.org",
    "registry.yarnpkg.com",
    "crates.io",
    "index.crates.io",
    "static.crates.io",
    "proxy.golang.org",
    "sum.golang.org",
];

/// The egress proxy's image: tinyproxy, refusing every host but the
/// registries.
pub fn egress_dockerfile() -> String {
    let filter: Vec<String> = REGISTRIES
        .iter()
        .map(|host| format!("'^{}$'", host.replace('.', "\\.")))
        .collect();
    format!(
        "FROM alpine:3.20\n\
         RUN apk add --no-cache tinyproxy \\\n \
         && printf '%s\\n' 'Port {EGRESS_PORT}' 'Listen 0.0.0.0' 'Timeout 300' 'MaxClients 64' \
         'Allow 10.0.0.0/8' 'Allow 172.16.0.0/12' 'Allow 192.168.0.0/16' 'FilterDefaultDeny Yes' \
         'Filter \"/etc/tinyproxy/filter\"' 'FilterType ere' 'ConnectPort 443' \
         'DisableViaHeader Yes' 'LogLevel Warning' > /etc/tinyproxy/tinyproxy.conf \\\n \
         && printf '%s\\n' {} > /etc/tinyproxy/filter\n\
         USER nobody\n\
         ENTRYPOINT [\"tinyproxy\", \"-d\", \"-c\", \"/etc/tinyproxy/tinyproxy.conf\"]\n",
        filter.join(" ")
    )
}

/// Where a job's dependencies live: a volume at `/deps`, so what the setup
/// installs is still there in the containers that run the commands, and the
/// tools pointed at it. `offline` keeps those tools off the network.
pub fn deps_env(offline: bool) -> Vec<String> {
    let mut env = vec![
        "PIP_USER=1",
        "PYTHONUSERBASE=/deps/python",
        "PIP_CACHE_DIR=/deps/cache/pip",
        "PIP_DISABLE_PIP_VERSION_CHECK=1",
        "npm_config_cache=/deps/cache/npm",
        "npm_config_update_notifier=false",
        "npm_config_fund=false",
        "npm_config_audit=false",
        "CARGO_HOME=/deps/cargo",
        "CARGO_TARGET_DIR=/deps/target",
        "GOPATH=/deps/go",
        "GOMODCACHE=/deps/go/pkg/mod",
        "GOCACHE=/deps/go/cache",
        "GOFLAGS=-modcacherw",
        "TMPDIR=/deps/tmp",
    ];
    if offline {
        env.extend([
            "CARGO_NET_OFFLINE=true",
            "GOPROXY=off",
            "PIP_NO_INDEX=1",
            "npm_config_offline=true",
        ]);
    }
    env.into_iter().map(String::from).collect()
}

/// The directories the installed tools' commands land in, ahead of the
/// image's own: `/deps/bin` holds the build's `go` shim.
pub const DEPS_PATH: &str = "/deps/bin:/deps/python/bin:/deps/go/bin";

/// Sends the package managers' registry traffic through the proxy. Each
/// tool gets its own setting; `everywhere` adds the generic variable too,
/// which a check's setup can take but a build cannot: the agent would send
/// its model traffic to the proxy as well, and the proxy refuses it.
pub fn proxy_env(proxy: &str, everywhere: bool) -> Vec<String> {
    let url = egress_url(proxy);
    let mut keys = vec![
        "PIP_PROXY",
        "npm_config_proxy",
        "npm_config_https_proxy",
        "CARGO_HTTP_PROXY",
    ];
    if everywhere {
        keys.extend(["https_proxy", "HTTPS_PROXY"]);
    }
    keys.iter().map(|key| format!("{key}={url}")).collect()
}

fn egress_url(proxy: &str) -> String {
    format!("http://{proxy}:{EGRESS_PORT}")
}

/// Readies a fresh `/deps` volume for the user a job's containers run as.
/// For a build, `go_proxy` adds a `go` that reaches the registries through
/// that proxy: go reads only the generic variable a build cannot set.
pub fn deps_init_args(volume: &str, owner: (u32, u32), go_proxy: Option<&str>) -> Vec<String> {
    let mut script = String::from("mkdir -p /deps/tmp /deps/bin");
    if let Some(proxy) = go_proxy {
        script.push_str(&format!(
            " && printf '#!/bin/sh\\nHTTPS_PROXY={} exec /usr/local/go/bin/go \"$@\"\\n' > /deps/bin/go \
             && chmod 755 /deps/bin/go",
            egress_url(proxy)
        ));
    }
    script.push_str(&format!(" && chown -R {}:{} /deps", owner.0, owner.1));
    vec![
        "run".into(),
        "--rm".into(),
        "--user".into(),
        "0".into(),
        "--network".into(),
        "none".into(),
        "--entrypoint".into(),
        "sh".into(),
        "-v".into(),
        format!("{volume}:/deps"),
        EGRESS_IMAGE.into(),
        "-c".into(),
        script,
    ]
}

/// The volume a build's or a check's dependencies go to.
pub fn deps_volume(name: &str) -> String {
    format!("{name}-deps")
}

/// How builds run.
#[derive(Clone, Debug)]
pub enum Builder {
    /// On this machine, under covguard's OS sandbox.
    Host,
    Container(ContainerBuilds),
}

#[derive(Clone, Debug)]
pub struct ContainerBuilds {
    /// Where the docker CLI finds its daemon. Passed explicitly because the
    /// sandbox covguard puts around the wrapper hides `~/.docker`.
    pub docker_host: String,
    /// An `--internal` network: containers on it reach each other and
    /// nothing else.
    pub network: String,
    pub forwarder_image: String,
    /// How a container reaches this machine's loopback, where covguard's
    /// proxy listens. `host.lima.internal` under colima.
    pub proxy_host: String,
    pub memory: String,
    pub cpus: String,
    pub pids: u32,
}

/// The builder image for a check image: `python:3.12-slim` builds in
/// `covenant-compute-builder:python-3.12-slim`.
pub fn builder_image(check_image: &str) -> String {
    let tag: String = check_image
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' => c.to_ascii_lowercase(),
            _ => '-',
        })
        .collect();
    format!("covenant-compute-builder:{tag}")
}

/// The check image with git and Claude Code added. Debian-based check
/// images only, which every allowed image is today.
pub fn builder_dockerfile(check_image: &str) -> String {
    format!(
        "FROM {check_image}\n\
         RUN apt-get update \\\n \
         && apt-get install -y --no-install-recommends git ca-certificates curl \\\n \
         && rm -rf /var/lib/apt/lists/*\n\
         RUN curl -fsSL https://claude.ai/install.sh | bash \\\n \
         && cp -L /root/.local/bin/claude /usr/local/bin/claude \\\n \
         && rm -rf /root/.local /root/.claude /root/.claude.json \\\n \
         && claude --version\n"
    )
}

/// The container names one build uses, so the node can remove them whatever
/// state the run ended in.
pub fn container_names(name: &str) -> [String; 3] {
    [name.to_string(), forwarder_name(name), egress_name(name)]
}

fn forwarder_name(name: &str) -> String {
    format!("{name}-fwd")
}

/// The proxy container's name, which is also its hostname on the internal
/// network: short, since a DNS label holds at most 63 bytes.
pub fn egress_name(name: &str) -> String {
    let digest = covenant_compute_protocol::sha256_hex(name.as_bytes());
    format!("egress-{}", &digest[..16])
}

/// covguard's agent argv for a containerized build: this binary's wrapper,
/// then the agent itself as it should run inside the image.
pub fn wrapper_argv(
    node_exe: &Path,
    builds: &ContainerBuilds,
    name: &str,
    tree: &Path,
    image: &str,
    agent: &[String],
) -> Vec<String> {
    let mut argv = vec![
        node_exe.to_string_lossy().into_owned(),
        BUILD_CONTAINER.into(),
        "--name".into(),
        name.into(),
        "--tree".into(),
        tree.to_string_lossy().into_owned(),
        "--image".into(),
        image.into(),
        "--docker-host".into(),
        builds.docker_host.clone(),
        "--network".into(),
        builds.network.clone(),
        "--forwarder".into(),
        builds.forwarder_image.clone(),
        "--proxy-host".into(),
        builds.proxy_host.clone(),
        "--memory".into(),
        builds.memory.clone(),
        "--cpus".into(),
        builds.cpus.clone(),
        "--pids".into(),
        builds.pids.to_string(),
        "--".into(),
    ];
    argv.extend(agent.iter().cloned());
    argv
}

/// What the wrapper was asked to run.
#[derive(Debug, PartialEq, Eq)]
struct WrapperArgs {
    name: String,
    tree: PathBuf,
    image: String,
    builds: ContainerArgs,
    agent: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct ContainerArgs {
    docker_host: String,
    network: String,
    forwarder: String,
    proxy_host: String,
    memory: String,
    cpus: String,
    pids: u32,
}

fn parse_wrapper(args: &[String]) -> Result<WrapperArgs, String> {
    let split = args
        .iter()
        .position(|a| a == "--")
        .ok_or("no agent command after --")?;
    let (flags, agent) = (&args[..split], &args[split + 1..]);
    if agent.is_empty() {
        return Err("no agent command after --".into());
    }
    let value = |flag: &str| -> Result<String, String> {
        flags
            .iter()
            .position(|a| a == flag)
            .and_then(|i| flags.get(i + 1))
            .cloned()
            .ok_or_else(|| format!("{flag} is required"))
    };
    Ok(WrapperArgs {
        name: value("--name")?,
        tree: PathBuf::from(value("--tree")?),
        image: value("--image")?,
        builds: ContainerArgs {
            docker_host: value("--docker-host")?,
            network: value("--network")?,
            forwarder: value("--forwarder")?,
            proxy_host: value("--proxy-host")?,
            memory: value("--memory")?,
            cpus: value("--cpus")?,
            pids: value("--pids")?
                .parse()
                .map_err(|_| "--pids must be a whole number".to_string())?,
        },
        agent: agent.to_vec(),
    })
}

/// The `docker run` arguments for the agent's container. Root-owned files
/// are never created in the checkout: the agent runs as the checkout's
/// owner, on a read-only image with its home and temp in memory.
/// `deps` names the dependency volume and the PATH that finds what is
/// installed there; `egress`, the proxy to the registries.
fn agent_run_args(
    wrapper: &WrapperArgs,
    owner: (u32, u32),
    custom_headers: Option<&str>,
    deps: Option<(&str, &str)>,
    egress: Option<&str>,
) -> Vec<String> {
    let (uid, gid) = owner;
    let b = &wrapper.builds;
    let mut args: Vec<String> = [
        "run",
        "--rm",
        "--name",
        &wrapper.name,
        "--network",
        &b.network,
        "--user",
        &format!("{uid}:{gid}"),
        "--read-only",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--memory",
        &b.memory,
        "--cpus",
        &b.cpus,
        "--pids-limit",
        &b.pids.max(1).to_string(),
        "--tmpfs",
        "/tmp:rw,nosuid,size=512m",
        "--tmpfs",
        &format!("/home/agent:rw,nosuid,size=256m,uid={uid},gid={gid},mode=0700"),
        "-v",
        &format!("{}:/work:rw", wrapper.tree.display()),
        "-w",
        "/work",
        "-e",
        "HOME=/home/agent",
        "-e",
        &format!(
            "ANTHROPIC_BASE_URL=http://{}:{FORWARDER_PORT}",
            forwarder_name(&wrapper.name)
        ),
        // A placeholder: the proxy strips it and stamps the real key.
        "-e",
        "ANTHROPIC_API_KEY=covguard-proxy-injected",
        "-e",
        "DISABLE_AUTOUPDATER=1",
        "-e",
        "DISABLE_TELEMETRY=1",
        "-e",
        "DISABLE_ERROR_REPORTING=1",
        "-e",
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1",
        // Test runs would otherwise leave bytecode in the checkout, and in
        // the patch.
        "-e",
        "PYTHONDONTWRITEBYTECODE=1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Some(headers) = custom_headers.filter(|h| !h.is_empty()) {
        args.push("-e".into());
        args.push(format!("ANTHROPIC_CUSTOM_HEADERS={headers}"));
    }
    if let Some((volume, path)) = deps {
        args.push("-v".into());
        args.push(format!("{volume}:/deps:rw"));
        for var in deps_env(false).into_iter().chain([format!("PATH={path}")]) {
            args.push("-e".into());
            args.push(var);
        }
    }
    for var in egress
        .map(|egress| proxy_env(egress, false))
        .unwrap_or_default()
    {
        args.push("-e".into());
        args.push(var);
    }
    args.push(builder_image(&wrapper.image));
    args.extend(wrapper.agent.iter().cloned());
    args
}

/// The image's PATH with the dependency volume's tool directories ahead of
/// it.
async fn image_path(docker: &impl Fn(&[&str]) -> Command, image: &str) -> Option<String> {
    let out = docker(&[
        "image",
        "inspect",
        "--format",
        "{{range .Config.Env}}{{println .}}{{end}}",
        image,
    ])
    .stderr(Stdio::null())
    .output()
    .await
    .ok()
    .filter(|o| o.status.success())?;
    let env = String::from_utf8_lossy(&out.stdout);
    let base = env
        .lines()
        .find_map(|line| line.strip_prefix("PATH="))
        .unwrap_or("/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin");
    Some(format!("{DEPS_PATH}:{base}"))
}

/// The wrapper's body. Returns the exit code to leave with: the agent's own,
/// or 1 when the containers could not start.
pub async fn run(args: &[String]) -> i32 {
    let wrapper = match parse_wrapper(args) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("{BUILD_CONTAINER}: {e}");
            return 2;
        }
    };
    let port = match std::env::var("ANTHROPIC_BASE_URL")
        .ok()
        .and_then(|u| u.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()))
    {
        Some(port) => port,
        None => {
            eprintln!("{BUILD_CONTAINER}: covguard gave no proxy port in ANTHROPIC_BASE_URL");
            return 2;
        }
    };
    let owner = match std::fs::metadata(&wrapper.tree) {
        Ok(meta) => {
            use std::os::unix::fs::MetadataExt;
            (meta.uid(), meta.gid())
        }
        Err(e) => {
            eprintln!(
                "{BUILD_CONTAINER}: checkout {}: {e}",
                wrapper.tree.display()
            );
            return 2;
        }
    };
    // The sandbox hides ~/.docker, so the CLI gets its own empty config.
    let config = std::env::temp_dir().join(format!("{}-docker", wrapper.name));
    let _ = std::fs::create_dir_all(&config);
    let docker = |args: &[&str]| {
        let mut cmd = Command::new("docker");
        cmd.args(args)
            .env("DOCKER_HOST", &wrapper.builds.docker_host)
            .env("DOCKER_CONFIG", &config);
        cmd
    };

    let forwarder = forwarder_name(&wrapper.name);
    let listen = format!("TCP-LISTEN:{FORWARDER_PORT},fork,reuseaddr");
    let bridge = std::env::var("COVGUARD_BRIDGE_SOCKET")
        .ok()
        .filter(|sock| !sock.is_empty());
    let started = match &bridge {
        // Only the internal network: the socket is the forwarder's one exit.
        Some(sock) => {
            let mount = format!("{sock}:/covguard.sock");
            docker(&[
                "run",
                "-d",
                "--rm",
                "--name",
                &forwarder,
                "--network",
                &wrapper.builds.network,
                "-v",
                &mount,
                &wrapper.builds.forwarder,
                &listen,
                "UNIX-CONNECT:/covguard.sock",
            ])
            .stdout(Stdio::null())
            .status()
            .await
            .is_ok_and(|s| s.success())
        }
        None => {
            let upstream = format!("TCP:{}:{port}", wrapper.builds.proxy_host);
            docker(&[
                "run",
                "-d",
                "--rm",
                "--name",
                &forwarder,
                &wrapper.builds.forwarder,
                &listen,
                &upstream,
            ])
            .stdout(Stdio::null())
            .status()
            .await
            .is_ok_and(|s| s.success())
                && docker(&["network", "connect", &wrapper.builds.network, &forwarder])
                    .status()
                    .await
                    .is_ok_and(|s| s.success())
        }
    };
    if !started {
        eprintln!("{BUILD_CONTAINER}: the proxy forwarder did not start");
        let _ = docker(&["rm", "-f", &forwarder]).status().await;
        return 1;
    }

    // The registries and a dependency volume let the agent install what the
    // work needs and run the tests as the checker will. Either missing, it
    // still builds, offline.
    let egress = egress_name(&wrapper.name);
    let egress_up = docker(&["run", "-d", "--rm", "--name", &egress, EGRESS_IMAGE])
        .stdout(Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
        && docker(&["network", "connect", &wrapper.builds.network, &egress])
            .status()
            .await
            .is_ok_and(|s| s.success());
    if !egress_up {
        eprintln!("{BUILD_CONTAINER}: the registry proxy did not start; building offline");
    }
    let volume = deps_volume(&wrapper.name);
    let init = deps_init_args(&volume, owner, egress_up.then_some(egress.as_str()));
    let init_refs: Vec<&str> = init.iter().map(String::as_str).collect();
    let deps_up = docker(&["volume", "create", &volume])
        .stdout(Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
        && docker(&init_refs)
            .stdout(Stdio::null())
            .status()
            .await
            .is_ok_and(|s| s.success());
    let path = match deps_up {
        true => image_path(&docker, &builder_image(&wrapper.image)).await,
        false => None,
    };

    let headers = std::env::var("ANTHROPIC_CUSTOM_HEADERS").ok();
    let run_args = agent_run_args(
        &wrapper,
        owner,
        headers.as_deref(),
        path.as_deref().map(|path| (volume.as_str(), path)),
        egress_up.then_some(egress.as_str()),
    );
    let run_refs: Vec<&str> = run_args.iter().map(String::as_str).collect();
    let code = match docker(&run_refs).status().await {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("{BUILD_CONTAINER}: docker run: {e}");
            1
        }
    };
    let _ = docker(&["rm", "-f", &forwarder, &egress])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    let _ = docker(&["volume", "rm", "-f", &volume])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    let _ = std::fs::remove_dir_all(&config);
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builds() -> ContainerBuilds {
        ContainerBuilds {
            docker_host: "unix:///home/u/.colima/default/docker.sock".into(),
            network: "covenant-compute-build".into(),
            forwarder_image: "alpine/socat".into(),
            proxy_host: "host.lima.internal".into(),
            memory: "2g".into(),
            cpus: "2".into(),
            pids: 512,
        }
    }

    #[test]
    fn builder_images_are_named_after_their_check_image() {
        assert_eq!(
            builder_image("python:3.12-slim"),
            "covenant-compute-builder:python-3.12-slim"
        );
        assert_eq!(
            builder_image("ghcr.io/x/y:1@sha256:ab"),
            "covenant-compute-builder:ghcr.io-x-y-1-sha256-ab"
        );
        assert!(builder_dockerfile("python:3.12-slim").starts_with("FROM python:3.12-slim\n"));
    }

    #[test]
    fn the_wrapper_round_trips_and_runs_the_agent_locked_down() {
        let agent = vec!["claude".to_string(), "-p".into(), "fix -- it".into()];
        let argv = wrapper_argv(
            Path::new("/bin/node"),
            &builds(),
            "compute-build-ab-1",
            Path::new("/home/u/work/tree"),
            "python:3.12-slim",
            &agent,
        );
        assert_eq!(argv[1], BUILD_CONTAINER);
        let parsed = parse_wrapper(&argv[2..]).unwrap();
        assert_eq!(
            parsed.agent, agent,
            "everything after the first -- is the agent's"
        );
        assert_eq!(parsed.builds.pids, 512);

        let run = agent_run_args(
            &parsed,
            (501, 20),
            Some("anthropic-workspace-id: w"),
            Some(("compute-build-ab-1-deps", "/deps/python/bin:/usr/bin")),
            Some("compute-build-ab-1-egress"),
        );
        let joined = run.join(" ");
        for needle in [
            "--network covenant-compute-build",
            "--user 501:20",
            "--read-only",
            "--cap-drop ALL",
            "--security-opt no-new-privileges",
            "-v /home/u/work/tree:/work:rw",
            "ANTHROPIC_BASE_URL=http://compute-build-ab-1-fwd:8080",
            "ANTHROPIC_API_KEY=covguard-proxy-injected",
            "ANTHROPIC_CUSTOM_HEADERS=anthropic-workspace-id: w",
            "-v compute-build-ab-1-deps:/deps:rw",
            "PIP_USER=1",
            "PATH=/deps/python/bin:/usr/bin",
            "PIP_PROXY=http://compute-build-ab-1-egress:8888",
            "CARGO_HTTP_PROXY=http://compute-build-ab-1-egress:8888",
        ] {
            assert!(joined.contains(needle), "missing {needle}");
        }
        let image_at = run
            .iter()
            .position(|a| a == "covenant-compute-builder:python-3.12-slim")
            .unwrap();
        assert_eq!(&run[image_at + 1..], agent.as_slice());
        assert!(
            !joined.contains(" -v /home/u:"),
            "only the checkout is mounted"
        );
        assert!(
            !run.iter()
                .any(|a| a.to_ascii_lowercase().starts_with("http_proxy=")
                    || a.to_ascii_lowercase().starts_with("https_proxy=")),
            "the agent's own traffic is never sent to the proxy"
        );
    }

    #[test]
    fn the_egress_proxy_admits_only_the_registries() {
        let dockerfile = egress_dockerfile();
        assert!(dockerfile.contains("'^pypi\\.org$'"), "{dockerfile}");
        assert!(dockerfile.contains("FilterDefaultDeny Yes"));
        assert!(!dockerfile.contains("github"));
        assert!(deps_env(true).contains(&"GOPROXY=off".to_string()));
        assert!(!deps_env(false).contains(&"GOPROXY=off".to_string()));
    }

    #[test]
    fn a_wrapper_without_its_settings_refuses_to_run() {
        assert!(parse_wrapper(&["--name".into(), "x".into()]).is_err());
        assert!(parse_wrapper(&["--name".into(), "x".into(), "--".into()]).is_err());
    }
}
