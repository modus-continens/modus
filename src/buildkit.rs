//! This module contains code which handles buildkit integration.
//!
//! To reduce friction, it is best that we do not require the user to install
//! buildkit themselves, which is actually another system service when running
//! on its own. Instead, we make use of the buildkit integration built into
//! `docker build` (search for `DOCKER_BUILDKIT` env) by creating our own little
//! "frontend" for buildkit.  In our case this would be a shim not intended to
//! be used directly by the user, but can be invoked simply by spawning `docker
//! build` in our code.
//!
//! When a user tries to build an image with our buildkit backend,
//! the following things will happen in order:
//!
//! 1. The outer modus program generates all required proofs and build plan for
//! the user's query, then encodes the build plan to json.
//!
//! 2. The outer program writes the json along with a special declaration line
//! `#syntax=...` to a temporary Dockerfile, and invokes `docker build`. The
//! `#syntax` declaration tells Docker to use our frontend instead of treating
//! it as a normal Dockerfile.
//!
//! 3. Our frontend (which runs inside a separate container created by docker
//! build) turns the json back into the build plan and generate all the required
//! buildkit calls to build all output images. If there are more than one output
//! images, it also generate a final empty image which depeneds on all the
//! output images user wants. This will effectively force buildkit to build
//! multiple output images in parallel, if that's what the user wants.
//!
//! 4. If there are multiple output images, invoke docker build again for each
//! output image, and instead of building the empty image, just build the
//! required output image, and also ignore all "no_cache". This should be
//! instant since everything is cached. We can also tell docker build to tag the
//! image with a name so that it can be referenced by the user.
//!
//! The easiest way to create this inner frontend is to build a separate binary.
//! Check out `buildkit_frontend.rs` for the main function of this inner
//! frontend.

use std::{
    fs::OpenOptions,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use signal_hook::{
    consts::{SIGCHLD, SIGINT, SIGTERM},
    iterator::{exfiltrator::SignalOnly, SignalsInfo},
};

use crate::imagegen::{BuildNode, BuildPlan, Output};

use colored::Colorize;
use rand::{
    distributions::{Distribution, Uniform},
    Rng,
};
use std::io::Write;

use thiserror::Error;

pub const FRONTEND_IMAGE: &str = concat!(
    "ghcr.io/modus-continens/modus-buildkit-frontend:",
    env!("GIT_SHA")
);

#[derive(Error, Debug)]
pub enum BuildError {
    #[error("Could not get current working directory")]
    CwdError(#[source] std::io::Error),
    #[error("Could not enter context directory: {0}")]
    EnterContextDir(#[source] std::io::Error),
    #[error("Could not open a temporary file under the current directory: {0}. This is required to invoke docker build correctly.")]
    UnableToCreateTempFile(#[source] std::io::Error),
    #[error("Unable to run docker build.")]
    UnableToRunDockerBuild(#[source] std::io::Error),
    #[error("docker build exited with code {0}.")]
    DockerBuildFailed(i32),
    #[error("docker tag {0} {1} exited with code {2}.")]
    DockerTagFailed(String, String, i32),
    #[error("{0} contains invalid utf-8.")]
    FileHasInvalidUtf8(String),
    #[error("Unable to create temporary directory: {0}")]
    UnableToCreateTempDir(#[source] std::io::Error),
    #[error("Unable to write {0}: {1}")]
    UnableToWriteTmpFile(String, #[source] std::io::Error),
    #[error("Unable to read {0}: {1}")]
    UnableToReadTmpFile(String, #[source] std::io::Error),
    #[error("Could not resolve {0}")]
    CouldNotResolveImage(String),
    #[error("{0}")]
    IOError(
        #[from]
        #[source]
        std::io::Error,
    ),
    #[error("Interrupted by user.")]
    Interrupted,
}

use BuildError::*;

#[derive(Debug, Clone, Default)]
pub struct DockerBuildOptions {
    pub verbose: bool,
    pub quiet: bool,
    pub no_cache: bool,
    pub additional_args: Vec<String>,
}

fn invoke_buildkit(
    dockerfile: &str,
    tag: Option<String>,
    target: Option<String>,
    has_dockerignore: bool,
    iidfile: Option<&str>,
    signals: &mut SignalsInfo<SignalOnly>,
    options: &DockerBuildOptions,
) -> Result<(), BuildError> {
    let mut args = Vec::new();
    args.push("build".to_string());
    args.push(".".to_string());
    args.push("-f".to_string());
    args.push(dockerfile.to_owned());
    if let Some(tag) = tag {
        args.push("-t".to_string());
        args.push(tag);
    }
    if options.no_cache {
        args.push("--no-cache".to_string());
        // Sometimes it isn't enough to just use --no-cache, so we also tell our frontend
        // to issue ignore_cache.
        args.push("--build-arg".to_string());
        args.push("no_cache=true".to_string());
    } else {
        args.push("--build-arg".to_string());
        args.push("no_cache=false".to_string());
    }
    if let Some(target) = target {
        args.push("--target".to_string());
        args.push(target);
    }
    if options.quiet {
        args.push("--quiet".to_string());
    }
    args.push("--build-arg".to_string());
    if has_dockerignore {
        args.push("has_dockerignore=true".to_string());
    } else {
        args.push("has_dockerignore=false".to_string());
    }
    if let Some(iidfile) = iidfile {
        args.push("--iidfile".to_string());
        args.push(iidfile.to_owned());
    }
    if options.verbose {
        args.push("--progress=plain".to_string());
    }
    args.extend_from_slice(&options.additional_args);
    let mut cmd = Command::new("docker")
        .args(args)
        .stdin(Stdio::null())
        .stdout(if options.quiet {
            Stdio::null()
        } else {
            Stdio::inherit()
        })
        .stderr(Stdio::inherit())
        .env("DOCKER_BUILDKIT", "1")
        .spawn()
        .map_err(|e| UnableToRunDockerBuild(e))?;
    let exit_status = loop {
        let mut has_sigchild = false;
        let mut has_term = false;
        for sig in signals.wait() {
            if sig == SIGCHLD {
                has_sigchild = true;
            } else if sig == SIGINT || sig == SIGTERM {
                has_term = true;
            }
        }
        if has_term {
            std::thread::sleep(std::time::Duration::from_millis(200)); // Wait for child to terminate first. This gives better terminal output.
            return Err(Interrupted);
        }
        if has_sigchild {
            let wait_res = cmd.try_wait().map_err(|e| IOError(e))?;
            if let Some(wait_res) = wait_res {
                break wait_res;
            }
        }
    };
    if !exit_status.success() {
        return Err(DockerBuildFailed(exit_status.code().unwrap_or(-1)));
    }
    Ok(())
}

/// A holder for a file name that deletes the file when dropped.
struct AutoDeleteTmpFilename(String);
/// A holder for a directory in std::env::temp_dir() that deletes the directory when dropped.
struct AutoRmTmpDir(PathBuf);
pub const TMP_PREFIX: &str = "modus_temp_";
pub const TMP_PREFIX_IGNORE_PATTERN: &str = "modus_temp_*";

pub fn gen_tmp_filename() -> String {
    const RANDOM_LEN: usize = 15;
    let mut rng = rand::thread_rng();
    let letters = Uniform::new_inclusive('a', 'z');
    let uletters = Uniform::new_inclusive('A', 'Z');
    let mut name = String::with_capacity(TMP_PREFIX.len() + RANDOM_LEN);
    name.push_str(TMP_PREFIX);
    for _ in 0..RANDOM_LEN {
        if rng.gen_bool(0.5) {
            name.push(letters.sample(&mut rng));
        } else {
            name.push(uletters.sample(&mut rng));
        }
    }
    name
}

impl AutoDeleteTmpFilename {
    /// Generate a name, but does not create the file.
    fn gen(suffix: &str) -> Self {
        let mut name = gen_tmp_filename();
        name.push_str(suffix);
        Self(name)
    }

    fn name(&self) -> &str {
        &self.0
    }
}

impl Drop for AutoDeleteTmpFilename {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(self.name()) {
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "Warning: unable to remove temporary file {}: {}",
                    self.name(),
                    e
                );
            }
        }
    }
}

impl AutoRmTmpDir {
    fn new_empty() -> std::io::Result<Self> {
        let mut name = std::env::temp_dir();
        name.push(&gen_tmp_filename());
        std::fs::create_dir(&name)?;
        Ok(Self(name))
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for AutoRmTmpDir {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "Warning: unable to remove temporary directory {}: {}",
                    self.0.display(),
                    e
                );
            }
        }
    }
}

fn write_tmp_dockerfile(content: &str) -> Result<AutoDeleteTmpFilename, std::io::Error> {
    let tmp_file = AutoDeleteTmpFilename::gen(".Dockerfile");
    let mut f = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(tmp_file.name())?;
    f.write_all(content.as_bytes())?;
    Ok(tmp_file)
}

struct RestoreCwd(PathBuf);
impl Drop for RestoreCwd {
    fn drop(&mut self) {
        if let Err(e) = std::env::set_current_dir(&self.0) {
            eprintln!(
                "Warning: unable to restore current directory to {}: {}",
                self.0.display(),
                e
            );
        }
    }
}

pub fn image_ref_is_hash(s: &str) -> bool {
    if s.contains('@') {
        return true;
    }
    if s.starts_with("sha256:") {
        return true;
    }
    let mut len = 0usize;
    const EXPECTED_LEN: usize = 256 / 8 * 2;
    for c in s.chars() {
        if !c.is_ascii_hexdigit() {
            return false;
        }
        len += 1;
        if len > EXPECTED_LEN {
            return false;
        }
    }
    len == EXPECTED_LEN
}

#[test]
fn test_image_ref_is_hash() {
    assert!(image_ref_is_hash("sha256:a"));
    assert!(image_ref_is_hash(
        "sha256:db94cc6af84cdf2f53d50391c4a9c6870fb524432d6bff02b3462d5cfc6c115a"
    ));
    assert!(image_ref_is_hash(
        "db94cc6af84cdf2f53d50391c4a9c6870fb524432d6bff02b3462d5cfc6c115a"
    ));
    assert!(image_ref_is_hash(
        "alpine@sha256:db94cc6af84cdf2f53d50391c4a9c6870fb524432d6bff02b3462d5cfc6c115a"
    ));

    assert!(!image_ref_is_hash("alpine"));
    assert!(!image_ref_is_hash("sha256"));
    assert!(!image_ref_is_hash("aaa/bbb"));
    assert!(!image_ref_is_hash(
        "db94cc6af84cdf2f53d50391c4a9c6870fb524432d6bff02b3462d5cfc6c115a0"
    ));
    assert!(!image_ref_is_hash(
        "db94cc6af84cdf2f53d50391c4a9c6870fb524432d6bff02b3462d5cfc6c115"
    ));
    assert!(!image_ref_is_hash(""));
    assert!(!image_ref_is_hash(
        "db94cc6af84cdf2f53d50391c4a9c6870fb524432d6bff02b3462d5cfc6c115a:latest"
    ));
    assert!(!image_ref_is_hash(
        "db94cc6af84cdf2f53d50391c4a9c6870fb524432d6bff02b3462d5gfc6c115a"
    ));
}

fn check_terminate(signals: &mut SignalsInfo<SignalOnly>) -> bool {
    signals.pending().any(|s| s == SIGINT || s == SIGTERM)
}

pub fn resolve_froms(
    build_plan: &mut BuildPlan,
    docker_build_options: &DockerBuildOptions,
    frontend_image: &str,
    signals: &mut SignalsInfo,
    tmp_tags: &mut Vec<String>,
) -> Result<(), BuildError> {
    let mut stderr = std::io::stderr();
    let mut printed = false;
    let total = build_plan
        .nodes
        .iter()
        .filter(|x| matches!(x, BuildNode::From { .. }))
        .count();
    let mut nb_done = 0usize;
    let _ctx = AutoRmTmpDir::new_empty().map_err(BuildError::UnableToCreateTempDir)?;
    let ctx = _ctx.path();
    std::env::set_current_dir(ctx).map_err(EnterContextDir)?; // CWD Restored outside
    let dockerfile = ctx.join("Dockerfile");
    let iidfile = ctx.join("iidfile");
    for node in build_plan.nodes.iter_mut() {
        match node {
            BuildNode::From {
                ref mut image_ref, ..
            } => {
                if image_ref_is_hash(&image_ref[..]) {
                    continue;
                }
                let _ = write!(
                    stderr,
                    "\x1b[2K\r{}\x1b[0m",
                    format!(
                        "[{}/{}] Resolving from({:?})...",
                        nb_done + 1,
                        total,
                        image_ref
                    )
                    .blue()
                );
                nb_done += 1;
                printed = true;
                let _ = stderr.flush();
                let mut tmp_plan = BuildPlan::new();
                let out = tmp_plan.new_node(
                    BuildNode::From {
                        image_ref: image_ref.clone(),
                        display_name: String::new(),
                    },
                    Vec::new(),
                );
                tmp_plan.outputs.push(Output {
                    node: out,
                    source_literal: None,
                });

                let mut content = String::new();
                content.push_str("#syntax=");
                content.push_str(frontend_image);
                content.push('\n');
                content.push_str(
                    &serde_json::to_string(&tmp_plan)
                        .expect("Unable to serialize temporary build plan"),
                );
                if check_terminate(signals) {
                    return Err(Interrupted);
                }
                std::fs::write(&dockerfile, content.as_bytes()).map_err(|e| {
                    BuildError::UnableToWriteTmpFile(dockerfile.display().to_string(), e)
                })?;
                let _ = std::fs::remove_file(&iidfile);
                let err = invoke_buildkit(
                    "Dockerfile",
                    None,
                    None,
                    false,
                    Some("iidfile"),
                    signals,
                    &DockerBuildOptions {
                        quiet: true,
                        verbose: false,
                        ..docker_build_options.clone()
                    },
                )
                .err();
                if err.is_none() {
                    let iid = std::fs::read_to_string(&iidfile)
                        .map_err(|e| UnableToReadTmpFile(iidfile.display().to_string(), e))?;
                    // Buildkit currently does not support building from a sha ID.
                    // https://github.com/moby/moby/issues/39769
                    // Therefore we create temporary tags for them.
                    if check_terminate(signals) {
                        return Err(Interrupted);
                    }
                    let tmp_tag = format!("modus_tmp_tag_{}", &iid);
                    // tmp_tag is going to be something like modus_tmp_tag_sha256:1234....
                    // This is very much intentional.
                    let st = Command::new("docker")
                        .args(&["tag", &iid, &tmp_tag])
                        .status()?;
                    if !st.success() {
                        return Err(BuildError::DockerTagFailed(
                            iid,
                            tmp_tag,
                            st.code().unwrap(),
                        ));
                    }
                    tmp_tags.push(tmp_tag.clone());
                    *image_ref = tmp_tag;
                } else {
                    let _ = write!(stderr, " failed.\n");
                    let _ = stderr.flush();
                    return Err(CouldNotResolveImage(image_ref.clone()));
                }
            }
            _ => {}
        }
    }
    if printed {
        let _ = write!(
            stderr,
            "\x1b[2K\r{}\x1b[0m\n",
            "[done] Resolving from(...)".blue()
        );
        let _ = stderr.flush();
    }
    Ok(())
}

struct CleanupTmpTags(Vec<String>);

impl Drop for CleanupTmpTags {
    fn drop(&mut self) {
        eprintln!("Cleaning up {} temporary tags...", self.0.len());
        for tag in self.0.iter() {
            let _ = Command::new("docker")
                .args(&["image", "rm", tag])
                .stdout(Stdio::null())
                .status();
        }
    }
}

/// Returns the image IDs on success, following the order in build_plan.outputs.
pub fn build<P: AsRef<Path>>(
    mut build_plan: BuildPlan,
    context: P,
    docker_build_options: &DockerBuildOptions,
    frontend_image: &str,
) -> Result<Vec<String>, BuildError> {
    let mut signals = SignalsInfo::with_exfiltrator(&[SIGINT, SIGTERM, SIGCHLD], SignalOnly)
        .expect("Failed to create signal handler.");
    if check_terminate(&mut signals) {
        return Err(Interrupted);
    }
    let context = context.as_ref().canonicalize().map_err(CwdError)?;
    let previous_cwd = PathBuf::from(".").canonicalize().map_err(CwdError)?;
    let _restore_cwd = RestoreCwd(previous_cwd);
    let mut tmp_tags = CleanupTmpTags(Vec::new());
    resolve_froms(
        &mut build_plan,
        docker_build_options,
        frontend_image,
        &mut signals,
        &mut tmp_tags.0,
    )?;
    std::env::set_current_dir(&context).map_err(EnterContextDir)?;
    let has_dockerignore = check_dockerignore()?;
    let mut content = String::new();
    content.push_str("#syntax=");
    content.push_str(frontend_image);
    content.push('\n');
    content.push_str(&serde_json::to_string(&build_plan).expect("Unable to serialize build plan"));
    if check_terminate(&mut signals) {
        return Err(Interrupted);
    }
    let dockerfile = write_tmp_dockerfile(&content).map_err(UnableToCreateTempFile)?;
    match build_plan.outputs.len() {
        0 => unreachable!(), // not possible because if there is no solution to the initial query, there will be an SLD failure.
        1 => {
            eprintln!("{}", "Running docker build...".blue());
            let iidfile = AutoDeleteTmpFilename::gen(".iid");
            invoke_buildkit(
                dockerfile.name(),
                None,
                None,
                has_dockerignore,
                Some(iidfile.name()),
                &mut signals,
                &docker_build_options,
            )?;
            if check_terminate(&mut signals) {
                return Err(Interrupted);
            }
            let iid = std::fs::read_to_string(iidfile.name())
                .map_err(|e| UnableToReadTmpFile(iidfile.name().to_owned(), e))?;
            Ok(vec![iid])
        }
        nb_outputs => {
            eprintln!("{}", "Running docker build...".blue());
            if check_terminate(&mut signals) {
                return Err(Interrupted);
            }
            invoke_buildkit(
                dockerfile.name(),
                None,
                None,
                has_dockerignore,
                None,
                &mut signals,
                docker_build_options,
            )?;
            let mut res = Vec::with_capacity(nb_outputs);
            let stderr = std::io::stderr();
            // TODO: check isatty
            let mut stderr = stderr.lock();
            write!(
                stderr,
                "\x1b[1A\x1b[2K\r=== Build success, exporting individual images ===\n"
            )?;
            for i in 0..nb_outputs {
                let target_str = format!("{}", i);
                write!(
                    stderr,
                    "{}",
                    format!(
                        "Exporting {}/{}: {}",
                        i + 1,
                        nb_outputs,
                        build_plan.outputs[i]
                            .source_literal
                            .as_ref()
                            .expect("Expected source_literal to present in build plan")
                            .to_string()
                    )
                    .blue()
                )?;
                stderr.flush()?;
                let iidfile = AutoDeleteTmpFilename::gen(".iid");
                invoke_buildkit(
                    dockerfile.name(),
                    None,
                    Some(target_str),
                    has_dockerignore,
                    Some(iidfile.name()),
                    &mut signals,
                    &DockerBuildOptions {
                        no_cache: false,
                        verbose: false,
                        quiet: true,
                        ..docker_build_options.clone()
                    },
                )?;
                let iid = std::fs::read_to_string(iidfile.name())
                    .map_err(|e| UnableToReadTmpFile(iidfile.name().to_owned(), e))?;
                write!(stderr, "{}", format!(" -> {}\n", &iid).blue())?;
                stderr.flush()?;
                res.push(iid);
            }
            Ok(res)
        }
    }
}

pub fn check_dockerignore() -> Result<bool, BuildError> {
    match std::fs::read(".dockerignore") {
        Ok(content) => {
            if std::str::from_utf8(&content).is_err() {
                Err(FileHasInvalidUtf8(".dockerignore".to_string()))
            } else {
                Ok(true)
            }
        }
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                Err(e.into())
            } else {
                Ok(false)
            }
        }
    }
}
