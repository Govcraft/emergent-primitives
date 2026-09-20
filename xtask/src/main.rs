//! Repository tasks: check the primitive manifests and generate a release's
//! index and manifest bundle from them.
//!
//! ```bash
//! cargo run -p xtask -- check --flags
//! cargo run -p xtask -- generate --tag v0.12.0 --out dist
//! ```
//!
//! Every decision lives in [`manifest`] and [`help`] as a pure function with
//! tests. This file is the shell around them: the filesystem, `cargo run` and
//! the exit code.

mod help;
mod manifest;

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use manifest::{DEFAULT_RELEASE_URL, SourceManifest};

#[derive(Parser, Debug)]
#[command(about = "Repository tasks for emergent-primitives")]
struct Cli {
    #[command(subcommand)]
    command: Task,
}

#[derive(Subcommand, Debug)]
enum Task {
    /// Check that every primitive has a manifest and that it is releasable
    Check {
        /// Also build each Rust primitive and compare its --help with the manifest
        #[arg(long)]
        flags: bool,
    },

    /// Write the release's index.toml and manifests.toml
    Generate {
        /// The release tag, with or without its leading v
        #[arg(long, value_name = "TAG")]
        tag: String,

        /// Directory to write into
        #[arg(long, value_name = "DIR")]
        out: PathBuf,

        /// Base URL the archives are downloaded from
        #[arg(long, value_name = "URL", default_value = DEFAULT_RELEASE_URL)]
        release_url: String,
    },
}

/// One primitive directory and the manifest it holds.
struct Primitive {
    dir_name: String,
    /// True for a Rust primitive, whose --help can be compared with the manifest.
    rust: bool,
    manifest: SourceManifest,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = repo_root()?;

    match cli.command {
        Task::Check { flags } => check(&root, flags),
        Task::Generate {
            tag,
            out,
            release_url,
        } => generate(&root, &tag, &out, &release_url),
    }
}

/// The repository root, which is the xtask crate's parent.
fn repo_root() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir
        .parent()
        .context("xtask should sit one level below the repository root")?;
    Ok(root.to_path_buf())
}

/// Load every primitive's manifest, reporting each problem rather than the
/// first one.
fn load_primitives(root: &Path) -> Result<Vec<Primitive>> {
    let primitives_dir = root.join("primitives");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&primitives_dir)
        .with_context(|| format!("reading {}", primitives_dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .collect();
    entries.sort();

    let mut primitives = Vec::new();
    let mut problems = Vec::new();

    for dir in entries {
        let Some(dir_name) = dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let has_main_rs = dir.join("src").join("main.rs").is_file();
        let has_main_ts = dir.join("main.ts").is_file();
        if !manifest::is_primitive_dir(has_main_rs, has_main_ts) {
            continue;
        }

        let path = dir.join("manifest.toml");
        if !path.is_file() {
            problems.push(format!("{dir_name}: has no manifest.toml"));
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let parsed: SourceManifest = match toml::from_str(&text) {
            Ok(parsed) => parsed,
            Err(e) => {
                problems.push(format!("{dir_name}/manifest.toml: {e}"));
                continue;
            }
        };
        for problem in manifest::validate(dir_name, &parsed) {
            problems.push(format!("{dir_name}/manifest.toml: {problem}"));
        }
        primitives.push(Primitive {
            dir_name: dir_name.to_string(),
            rust: has_main_rs,
            manifest: parsed,
        });
    }

    if !problems.is_empty() {
        report(&problems);
        bail!("{} manifest problem(s)", problems.len());
    }
    Ok(primitives)
}

fn check(root: &Path, flags: bool) -> Result<()> {
    let primitives = load_primitives(root)?;
    println!("{} manifests are well formed", primitives.len());

    if flags {
        let mut problems = Vec::new();
        for primitive in primitives.iter().filter(|p| p.rust) {
            let help = cargo_help(root, &primitive.dir_name)?;
            for problem in help::compare_with_help(&primitive.manifest, &help) {
                problems.push(format!("{}/manifest.toml: {problem}", primitive.dir_name));
            }
        }
        if !problems.is_empty() {
            report(&problems);
            bail!("{} flag problem(s)", problems.len());
        }
        let checked = primitives.iter().filter(|p| p.rust).count();
        println!("{checked} Rust primitives agree with their --help");
        println!(
            "{} Deno primitives parse their own arguments and print no help, so their \
             flags are not machine checked",
            primitives.len() - checked
        );
    }

    Ok(())
}

/// A primitive's `--help` page, built on demand.
fn cargo_help(root: &Path, name: &str) -> Result<String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(cargo)
        .current_dir(root)
        .args(["run", "--quiet", "-p", name, "--", "--help"])
        .output()
        .with_context(|| format!("running {name} --help"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{name} --help failed: {stderr}");
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn generate(root: &Path, tag: &str, out: &Path, release_url: &str) -> Result<()> {
    let primitives = load_primitives(root)?;
    let sources: Vec<SourceManifest> = primitives
        .into_iter()
        .map(|primitive| primitive.manifest)
        .collect();
    let version = manifest::version_from_tag(tag);

    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;

    let index = manifest::build_index(&sources, version);
    let index_path = out.join("index.toml");
    std::fs::write(&index_path, toml::to_string_pretty(&index)?)
        .with_context(|| format!("writing {}", index_path.display()))?;

    let bundle = manifest::build_bundle(&sources, version, release_url);
    let bundle_path = out.join("manifests.toml");
    std::fs::write(&bundle_path, toml::to_string_pretty(&bundle)?)
        .with_context(|| format!("writing {}", bundle_path.display()))?;

    println!(
        "wrote {} and {} for v{version} ({} primitives)",
        index_path.display(),
        bundle_path.display(),
        sources.len()
    );
    Ok(())
}

fn report(problems: &[String]) {
    for problem in problems {
        eprintln!("  {problem}");
    }
}
