use std::ffi::OsString;
use std::path::PathBuf;

use covenant_timeline_adapter::release::{initialize_release_timeline, reconcile_release_timeline};

const USAGE: &str = "usage:
  covenant-timeline-release initial --created <path> --readiness <path> --state <path>
  covenant-timeline-release reconcile --created <path> --readiness <path> --state <path> --published <path>";

fn main() {
    if let Err(error) = run(std::env::args_os().skip(1).collect()) {
        eprintln!("covenant-timeline-release: {error}");
        std::process::exit(1);
    }
}

fn run(arguments: Vec<OsString>) -> Result<(), String> {
    let Some(command) = arguments.first().and_then(|value| value.to_str()) else {
        return Err(USAGE.into());
    };
    match command {
        "initial" => {
            let options = parse_options(&arguments[1..], &["--created", "--readiness", "--state"])?;
            initialize_release_timeline(
                option(&options, "--created")?,
                option(&options, "--readiness")?,
                option(&options, "--state")?,
            )
            .map_err(|error| error.to_string())?;
        }
        "reconcile" => {
            let options = parse_options(
                &arguments[1..],
                &["--created", "--readiness", "--state", "--published"],
            )?;
            reconcile_release_timeline(
                option(&options, "--created")?,
                option(&options, "--readiness")?,
                option(&options, "--state")?,
                option(&options, "--published")?,
            )
            .map_err(|error| error.to_string())?;
        }
        _ => return Err(USAGE.into()),
    }
    Ok(())
}

fn parse_options(
    arguments: &[OsString],
    allowed: &[&'static str],
) -> Result<Vec<(&'static str, PathBuf)>, String> {
    if arguments.len() != allowed.len() * 2 {
        return Err(USAGE.into());
    }
    let mut parsed = Vec::with_capacity(allowed.len());
    for pair in arguments.chunks_exact(2) {
        let Some(key) = pair[0].to_str() else {
            return Err(USAGE.into());
        };
        let Some(allowed_key) = allowed.iter().copied().find(|candidate| *candidate == key) else {
            return Err(USAGE.into());
        };
        if parsed.iter().any(|(existing, _)| *existing == allowed_key) {
            return Err(format!("{allowed_key} was repeated"));
        }
        if pair[1].is_empty() {
            return Err(format!("{allowed_key} requires a path"));
        }
        parsed.push((allowed_key, PathBuf::from(&pair[1])));
    }
    if parsed.len() != allowed.len() {
        return Err(USAGE.into());
    }
    Ok(parsed)
}

fn option<'a>(options: &'a [(&str, PathBuf)], name: &str) -> Result<&'a PathBuf, String> {
    options
        .iter()
        .find_map(|(key, value)| (*key == name).then_some(value))
        .ok_or_else(|| format!("{name} is required"))
}
