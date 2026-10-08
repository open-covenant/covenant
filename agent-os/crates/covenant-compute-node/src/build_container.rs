//! Builds inside a container: the agent sees the job's checkout and nothing
//! else of this machine, and its only network route is covguard's metering
//! proxy, which holds the model key.
//!
//! covguard still starts the proxy and supervises the run, but the "agent" it
//! launches is this node binary's hidden `__build-container` subcommand. That
//! wrapper reads the proxy's port from `ANTHROPIC_BASE_URL`, starts a small
//! forwarder container that can reach the proxy, attaches it to an internal
//! network with no other way out, and runs the agent in the builder image on
//! that network. A builder image is the task's check image with Claude Code
//! added, so the agent runs its tests where the checker will.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

/// The subcommand covguard runs as its agent.
pub const BUILD_CONTAINER: &str = "__build-container";

/// The port the forwarder listens on inside the internal network.
const FORWARDER_PORT: u16 = 8080;

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
pub fn container_names(name: &str) -> [String; 2] {
    [name.to_string(), forwarder_name(name)]
}

fn forwarder_name(name: &str) -> String {
    format!("{name}-fwd")
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
fn agent_run_args(
    wrapper: &WrapperArgs,
    owner: (u32, u32),
    custom_headers: Option<&str>,
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
    args.push(builder_image(&wrapper.image));
    args.extend(wrapper.agent.iter().cloned());
    args
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
    let upstream = format!("TCP:{}:{port}", wrapper.builds.proxy_host);
    let listen = format!("TCP-LISTEN:{FORWARDER_PORT},fork,reuseaddr");
    let started = docker(&[
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
            .is_ok_and(|s| s.success());
    if !started {
        eprintln!("{BUILD_CONTAINER}: the proxy forwarder did not start");
        let _ = docker(&["rm", "-f", &forwarder]).status().await;
        return 1;
    }

    let headers = std::env::var("ANTHROPIC_CUSTOM_HEADERS").ok();
    let run_args = agent_run_args(&wrapper, owner, headers.as_deref());
    let run_refs: Vec<&str> = run_args.iter().map(String::as_str).collect();
    let code = match docker(&run_refs).status().await {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("{BUILD_CONTAINER}: docker run: {e}");
            1
        }
    };
    let _ = docker(&["rm", "-f", &forwarder])
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

        let run = agent_run_args(&parsed, (501, 20), Some("anthropic-workspace-id: w"));
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
    }

    #[test]
    fn a_wrapper_without_its_settings_refuses_to_run() {
        assert!(parse_wrapper(&["--name".into(), "x".into()]).is_err());
        assert!(parse_wrapper(&["--name".into(), "x".into(), "--".into()]).is_err());
    }
}
