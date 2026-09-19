//! Manifest types and the pure functions that turn them into a release.
//!
//! A manifest written next to a primitive's code carries no version and no
//! checksums. [`release_manifest`] and [`build_index`] stamp the release tag
//! into the copies attached to the GitHub release, so a release needs no
//! hand-edited version anywhere.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Target triples `.github/workflows/release.yml` builds an archive for.
///
/// A manifest may not name a target the release does not build, so this list
/// and the workflow's matrix move together.
pub const RELEASE_TARGETS: [&str; 4] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
];

/// Where the engine downloads archives from by default.
pub const DEFAULT_RELEASE_URL: &str = "https://github.com/Govcraft/emergent-primitives/releases";

/// The kinds of primitive the engine runs.
pub const KINDS: [&str; 3] = ["source", "handler", "sink"];

/// A manifest as it is written next to a primitive's code.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SourceManifest {
    pub primitive: SourcePrimitive,
    #[serde(default)]
    pub messages: Messages,
    #[serde(default)]
    pub args: Vec<Argument>,
    pub binaries: SourceBinaries,
}

/// The `[primitive]` table of a source manifest.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SourcePrimitive {
    pub name: String,
    pub kind: String,
    pub description: String,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub runtime: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// The message types a primitive publishes and subscribes to.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Messages {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publishes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subscribes: Vec<String>,
}

/// One command-line argument the primitive accepts.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Argument {
    pub name: String,
    pub long: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(default)]
    pub required: bool,
    pub description: String,
}

/// The `[binaries]` table of a source manifest: target triples only.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SourceBinaries {
    pub targets: Vec<String>,
}

/// A manifest as it is attached to a release, with the version stamped in.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReleaseManifest {
    pub primitive: ReleasePrimitive,
    pub messages: Messages,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<Argument>,
    pub binaries: ReleaseBinaries,
}

/// The `[primitive]` table of a released manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReleasePrimitive {
    pub name: String,
    pub version: String,
    pub kind: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
}

/// Download locations for a released primitive.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReleaseBinaries {
    pub release_url: String,
    pub targets: BTreeMap<String, String>,
}

/// Every released manifest in one file, so the engine fetches one asset.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManifestBundle {
    pub version: String,
    pub manifests: BTreeMap<String, ReleaseManifest>,
}

/// The release's index: what the engine lists and searches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Index {
    pub registry: IndexInfo,
    pub primitives: Vec<IndexEntry>,
}

/// Who published the index and from which release.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IndexInfo {
    pub name: String,
    pub version: String,
    pub description: String,
}

/// One row of the index.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IndexEntry {
    pub name: String,
    pub version: String,
    pub kind: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publishes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subscribes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// A release tag without its leading `v`.
///
/// The workflow hands the tag through as `refs/tags/vX.Y.Z`, and callers may
/// pass either form.
#[must_use]
pub fn version_from_tag(tag: &str) -> &str {
    let tag = tag.rsplit('/').next().unwrap_or(tag);
    tag.strip_prefix('v').unwrap_or(tag)
}

/// The archive the release workflow packages for a target.
#[must_use]
pub fn archive_filename(name: &str, version: &str, target: &str) -> String {
    format!("{name}-{version}-{target}.tar.gz")
}

/// Stamp a version and a release URL into a source manifest.
#[must_use]
pub fn release_manifest(
    source: &SourceManifest,
    version: &str,
    release_url: &str,
) -> ReleaseManifest {
    let name = source.primitive.name.clone();
    let targets = source
        .binaries
        .targets
        .iter()
        .map(|target| (target.clone(), archive_filename(&name, version, target)))
        .collect();

    ReleaseManifest {
        primitive: ReleasePrimitive {
            name,
            version: version.to_string(),
            kind: source.primitive.kind.clone(),
            description: source.primitive.description.clone(),
            homepage: source.primitive.homepage.clone(),
            license: source.primitive.license.clone(),
            runtime: source.primitive.runtime.clone(),
        },
        messages: source.messages.clone(),
        args: source.args.clone(),
        binaries: ReleaseBinaries {
            release_url: release_url.to_string(),
            targets,
        },
    }
}

/// Build the release's index from every source manifest.
#[must_use]
pub fn build_index(sources: &[SourceManifest], version: &str) -> Index {
    let mut primitives: Vec<IndexEntry> = sources
        .iter()
        .map(|source| IndexEntry {
            name: source.primitive.name.clone(),
            version: version.to_string(),
            kind: source.primitive.kind.clone(),
            description: source.primitive.description.clone(),
            publishes: source.messages.publishes.clone(),
            subscribes: source.messages.subscribes.clone(),
            tags: source.primitive.tags.clone(),
        })
        .collect();
    primitives.sort_by(|a, b| a.name.cmp(&b.name));

    Index {
        registry: IndexInfo {
            name: "emergent-primitives".to_string(),
            version: version.to_string(),
            description: "Official Emergent primitives, published with the release".to_string(),
        },
        primitives,
    }
}

/// Build the bundle of released manifests from every source manifest.
#[must_use]
pub fn build_bundle(
    sources: &[SourceManifest],
    version: &str,
    release_url: &str,
) -> ManifestBundle {
    ManifestBundle {
        version: version.to_string(),
        manifests: sources
            .iter()
            .map(|source| {
                (
                    source.primitive.name.clone(),
                    release_manifest(source, version, release_url),
                )
            })
            .collect(),
    }
}

/// Whether a directory under `primitives/` holds a primitive rather than a
/// shared library like `exec-common`.
#[must_use]
pub fn is_primitive_dir(has_main_rs: bool, has_main_ts: bool) -> bool {
    has_main_rs || has_main_ts
}

/// Everything wrong with one source manifest, in the order it was found.
///
/// An empty vector means the manifest is releasable.
#[must_use]
pub fn validate(dir_name: &str, source: &SourceManifest) -> Vec<String> {
    let mut problems = Vec::new();
    let primitive = &source.primitive;

    if primitive.name != dir_name {
        problems.push(format!(
            "[primitive].name is \"{}\" but the directory is \"{dir_name}\"",
            primitive.name
        ));
    }
    if !KINDS.contains(&primitive.kind.as_str()) {
        problems.push(format!(
            "[primitive].kind is \"{}\", not one of {}",
            primitive.kind,
            KINDS.join(", ")
        ));
    }
    if primitive.description.trim().is_empty() {
        problems.push("[primitive].description is empty".to_string());
    }

    if primitive.kind == "source" && !source.messages.subscribes.is_empty() {
        problems.push("a source cannot subscribe, but [messages].subscribes is set".to_string());
    }
    if primitive.kind == "sink" && !source.messages.publishes.is_empty() {
        problems.push("a sink cannot publish, but [messages].publishes is set".to_string());
    }

    if source.binaries.targets.is_empty() {
        problems.push("[binaries].targets is empty".to_string());
    }
    for target in &source.binaries.targets {
        if !RELEASE_TARGETS.contains(&target.as_str()) {
            problems.push(format!(
                "[binaries].targets names \"{target}\", which the release does not build"
            ));
        }
    }
    problems.extend(
        duplicates(source.binaries.targets.iter().map(String::as_str))
            .map(|target| format!("[binaries].targets names \"{target}\" more than once")),
    );

    problems.extend(
        duplicates(source.args.iter().map(|arg| arg.name.as_str()))
            .map(|name| format!("two arguments are named \"{name}\"")),
    );
    problems.extend(
        duplicates(source.args.iter().map(|arg| arg.long.as_str()))
            .map(|long| format!("two arguments use --{long}")),
    );
    for arg in &source.args {
        if !is_kebab_case(&arg.long) {
            problems.push(format!("--{} is not a lowercase kebab-case flag", arg.long));
        }
        if arg.description.trim().is_empty() {
            problems.push(format!("--{} has no description", arg.long));
        }
    }

    problems
}

/// The values that appear more than once, each reported once.
fn duplicates<'a>(values: impl Iterator<Item = &'a str>) -> impl Iterator<Item = String> {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for value in values {
        *seen.entry(value).or_insert(0) += 1;
    }
    seen.into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(value, _)| value.to_string())
}

/// Whether a flag is spelled the way clap spells a long flag.
fn is_kebab_case(long: &str) -> bool {
    !long.is_empty()
        && !long.starts_with('-')
        && !long.ends_with('-')
        && long
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(toml: &str) -> SourceManifest {
        match toml::from_str(toml) {
            Ok(manifest) => manifest,
            Err(e) => panic!("fixture should parse: {e}"),
        }
    }

    fn exec_source() -> SourceManifest {
        source(
            r#"
[primitive]
name = "exec-source"
kind = "source"
description = "Execute shell commands and emit output as events"
homepage = "https://example.invalid"
license = "MIT"
tags = ["exec", "shell"]

[messages]
publishes = ["exec.output"]

[[args]]
name = "command"
long = "command"
short = "c"
env = "EXEC_SOURCE_COMMAND"
required = true
description = "Command to execute"

[binaries]
targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]
"#,
        )
    }

    #[test]
    fn a_tag_yields_the_plain_version() {
        for (tag, expected) in [
            ("v0.12.0", "0.12.0"),
            ("0.12.0", "0.12.0"),
            ("refs/tags/v0.12.0", "0.12.0"),
            ("refs/tags/0.12.0", "0.12.0"),
            ("v1.0.0-rc.1", "1.0.0-rc.1"),
        ] {
            assert_eq!(version_from_tag(tag), expected, "tag {tag}");
        }
    }

    #[test]
    fn an_archive_is_named_for_its_primitive_version_and_target() {
        assert_eq!(
            archive_filename("exec-source", "0.12.0", "x86_64-apple-darwin"),
            "exec-source-0.12.0-x86_64-apple-darwin.tar.gz"
        );
    }

    #[test]
    fn a_released_manifest_carries_the_version_and_every_archive_name() {
        let released = release_manifest(&exec_source(), "0.13.0", DEFAULT_RELEASE_URL);

        assert_eq!(released.primitive.version, "0.13.0");
        assert_eq!(released.primitive.runtime, None);
        assert_eq!(released.binaries.release_url, DEFAULT_RELEASE_URL);
        assert_eq!(
            released.binaries.targets.get("x86_64-unknown-linux-gnu"),
            Some(&"exec-source-0.13.0-x86_64-unknown-linux-gnu.tar.gz".to_string())
        );
        assert_eq!(released.binaries.targets.len(), 2);
        assert_eq!(released.args, exec_source().args);
    }

    #[test]
    fn the_index_sorts_by_name_and_stamps_one_version() {
        let mut other = exec_source();
        other.primitive.name = "aaa-sink".to_string();
        other.primitive.kind = "sink".to_string();
        let index = build_index(&[exec_source(), other], "0.13.0");

        assert_eq!(index.registry.version, "0.13.0");
        let names: Vec<&str> = index
            .primitives
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, vec!["aaa-sink", "exec-source"]);
        assert!(index.primitives.iter().all(|e| e.version == "0.13.0"));
        assert_eq!(index.primitives[1].tags, vec!["exec", "shell"]);
    }

    #[test]
    fn the_bundle_is_keyed_by_primitive_name() {
        let bundle = build_bundle(&[exec_source()], "0.13.0", DEFAULT_RELEASE_URL);
        assert_eq!(bundle.version, "0.13.0");
        assert_eq!(bundle.manifests.len(), 1);
        assert!(bundle.manifests.contains_key("exec-source"));
    }

    #[test]
    fn the_bundle_round_trips_through_toml() -> Result<(), Box<dyn std::error::Error>> {
        let bundle = build_bundle(&[exec_source()], "0.13.0", DEFAULT_RELEASE_URL);
        let text = toml::to_string_pretty(&bundle)?;
        let parsed: ManifestBundle = toml::from_str(&text)?;
        assert_eq!(parsed, bundle);
        Ok(())
    }

    #[test]
    fn the_index_round_trips_through_toml() -> Result<(), Box<dyn std::error::Error>> {
        let index = build_index(&[exec_source()], "0.13.0");
        let text = toml::to_string_pretty(&index)?;
        let parsed: Index = toml::from_str(&text)?;
        assert_eq!(parsed, index);
        Ok(())
    }

    #[test]
    fn a_shared_library_directory_is_not_a_primitive() {
        for (has_main_rs, has_main_ts, expected) in [
            (true, false, true),
            (false, true, true),
            (false, false, false),
        ] {
            assert_eq!(is_primitive_dir(has_main_rs, has_main_ts), expected);
        }
    }

    #[test]
    fn a_well_formed_manifest_has_no_problems() {
        assert_eq!(
            validate("exec-source", &exec_source()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_name_that_disagrees_with_its_directory_is_a_problem() {
        let problems = validate("exec-sink", &exec_source());
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("exec-sink"), "{problems:?}");
    }

    #[test]
    fn validation_reports_every_kind_of_problem() {
        let mut manifest = exec_source();
        manifest.primitive.kind = "router".to_string();
        manifest.primitive.description = "  ".to_string();
        manifest.binaries.targets = vec![
            "x86_64-unknown-linux-gnu".to_string(),
            "x86_64-unknown-linux-gnu".to_string(),
            "sparc-unknown-none".to_string(),
        ];
        manifest.args.push(Argument {
            name: "command".to_string(),
            long: "Command".to_string(),
            short: None,
            env: None,
            required: false,
            description: String::new(),
        });

        let problems = validate("exec-source", &manifest);
        let joined = problems.join("\n");
        assert!(joined.contains("kind"), "{joined}");
        assert!(joined.contains("description is empty"), "{joined}");
        assert!(joined.contains("sparc-unknown-none"), "{joined}");
        assert!(joined.contains("more than once"), "{joined}");
        assert!(joined.contains("two arguments are named"), "{joined}");
        assert!(joined.contains("kebab-case"), "{joined}");
        assert!(joined.contains("no description"), "{joined}");
    }

    #[test]
    fn a_source_may_not_subscribe_and_a_sink_may_not_publish() {
        let mut subscribing_source = exec_source();
        subscribing_source.messages.subscribes = vec!["timer.tick".to_string()];
        let problems = validate("exec-source", &subscribing_source);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("source cannot subscribe"),
            "{problems:?}"
        );

        let mut publishing_sink = exec_source();
        publishing_sink.primitive.kind = "sink".to_string();
        let problems = validate("exec-source", &publishing_sink);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("sink cannot publish"), "{problems:?}");
    }

    #[test]
    fn an_unknown_key_in_a_manifest_fails_to_parse() {
        let result: Result<SourceManifest, _> = toml::from_str(
            r#"
[primitive]
name = "exec-source"
kind = "source"
description = "d"
verison = "0.12.0"

[binaries]
targets = ["x86_64-unknown-linux-gnu"]
"#,
        );
        let Err(e) = result else {
            panic!("a misspelled key should not parse");
        };
        assert!(e.to_string().contains("verison"), "{e}");
    }
}
