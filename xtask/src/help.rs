//! Reading a clap `--help` page and comparing it with a manifest.
//!
//! clap lays an option out as a flag line indented two to eight spaces and a
//! description indented ten, so a flag mentioned inside a description is never
//! mistaken for an option. A description that wraps onto a line beginning with
//! `--` would be, which is the one thing this parser cannot tell apart; no
//! primitive's help does that today.
//!
//! Only the Rust primitives are checked this way. The Deno primitives parse
//! `Deno.args` by hand and print no help page, so their manifests are read by
//! eye, not by machine.

use std::collections::BTreeMap;

use crate::manifest::SourceManifest;

/// clap's own flags, which no manifest declares.
const BUILTIN_FLAGS: [&str; 2] = ["help", "version"];

/// The long flags a help page offers, each with the environment variable clap
/// reads for it.
#[must_use]
pub fn flags_in_help(help: &str) -> BTreeMap<String, Option<String>> {
    let mut flags: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut current: Option<String> = None;

    for line in help.lines() {
        if let Some(long) = long_flag_on(line) {
            current = Some(long.clone());
            if !BUILTIN_FLAGS.contains(&long.as_str()) {
                flags.insert(long, None);
            }
        } else if let Some(env) = env_on(line)
            && let Some(long) = current.as_ref()
            && let Some(slot) = flags.get_mut(long)
        {
            *slot = Some(env);
        }
    }

    flags
}

/// The long flag an option line introduces, if it is one.
fn long_flag_on(line: &str) -> Option<String> {
    let indent = line.len() - line.trim_start().len();
    if !(2..=8).contains(&indent) {
        return None;
    }
    let rest = line.trim_start();
    // Skip a short flag and its comma, as in "-c, --command <COMMAND>".
    let rest = match rest.split_once(", ") {
        Some((short, rest)) if short.len() == 2 && short.starts_with('-') => rest,
        _ => rest,
    };
    let name: String = rest
        .strip_prefix("--")?
        .chars()
        .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .collect();
    if name.is_empty() { None } else { Some(name) }
}

/// The environment variable a `[env: NAME=]` line names.
fn env_on(line: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix("[env: ")?;
    let name = rest.split(['=', ']']).next()?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Everything a manifest says about flags that its binary's help page
/// contradicts, in the order it was found.
#[must_use]
pub fn compare_with_help(source: &SourceManifest, help: &str) -> Vec<String> {
    let actual = flags_in_help(help);
    let declared: BTreeMap<&str, Option<&str>> = source
        .args
        .iter()
        .map(|arg| (arg.long.as_str(), arg.env.as_deref()))
        .collect();

    let mut problems = Vec::new();
    for (long, env) in &actual {
        match declared.get(long.as_str()) {
            None => problems.push(format!("the binary takes --{long}, the manifest does not")),
            Some(declared_env) => {
                if declared_env.map(str::to_string) != *env {
                    problems.push(format!(
                        "--{long} reads {}, the manifest says {}",
                        env.as_deref().unwrap_or("no environment variable"),
                        declared_env.unwrap_or("none")
                    ));
                }
            }
        }
    }
    for long in declared.keys() {
        if !actual.contains_key(*long) {
            problems.push(format!(
                "the manifest declares --{long}, the binary does not"
            ));
        }
    }

    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed copy of exec-source's real help page.
    const EXEC_SOURCE_HELP: &str = "\
Executes shell commands and emits output events

Usage: exec-source [OPTIONS] --command <COMMAND>

Options:
  -c, --command <COMMAND>
          Command to execute

          [env: EXEC_SOURCE_COMMAND=]

  -i, --interval <INTERVAL>
          Optional interval in milliseconds

          [env: EXEC_SOURCE_INTERVAL=]
          [default: 0]

      --correlate
          Mint one correlation ID at startup.

          Ignored when `--correlation-id` supplies one to adopt.

  -h, --help
          Print help
";

    fn manifest(args: &str) -> SourceManifest {
        let toml = format!(
            r#"
[primitive]
name = "exec-source"
kind = "source"
description = "Execute shell commands"

[messages]
publishes = ["exec.output"]
{args}
[binaries]
targets = ["x86_64-unknown-linux-gnu"]
"#
        );
        match toml::from_str(&toml) {
            Ok(parsed) => parsed,
            Err(e) => panic!("fixture should parse: {e}"),
        }
    }

    const MATCHING_ARGS: &str = r#"
[[args]]
name = "command"
long = "command"
short = "c"
env = "EXEC_SOURCE_COMMAND"
required = true
description = "Command to execute"

[[args]]
name = "interval"
long = "interval"
short = "i"
env = "EXEC_SOURCE_INTERVAL"
required = false
description = "Interval in milliseconds"

[[args]]
name = "correlate"
long = "correlate"
required = false
description = "Mint one correlation ID"
"#;

    #[test]
    fn a_help_page_yields_every_long_flag_but_clap_s_own() {
        let flags = flags_in_help(EXEC_SOURCE_HELP);
        let names: Vec<&str> = flags.keys().map(String::as_str).collect();
        assert_eq!(names, vec!["command", "correlate", "interval"]);
    }

    #[test]
    fn an_env_line_belongs_to_the_flag_above_it() {
        let flags = flags_in_help(EXEC_SOURCE_HELP);
        assert_eq!(
            flags.get("command"),
            Some(&Some("EXEC_SOURCE_COMMAND".to_string()))
        );
        assert_eq!(flags.get("correlate"), Some(&None));
    }

    #[test]
    fn a_flag_named_inside_a_description_is_not_an_option() {
        assert!(!flags_in_help(EXEC_SOURCE_HELP).contains_key("correlation-id"));
    }

    #[test]
    fn a_manifest_that_matches_its_help_page_has_no_problems() {
        assert_eq!(
            compare_with_help(&manifest(MATCHING_ARGS), EXEC_SOURCE_HELP),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_undeclared_flag_is_reported() {
        let without_correlate = MATCHING_ARGS
            .split("[[args]]\nname = \"correlate\"")
            .next()
            .unwrap_or_default();
        let problems = compare_with_help(&manifest(without_correlate), EXEC_SOURCE_HELP);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("the binary takes --correlate"),
            "{problems:?}"
        );
    }

    #[test]
    fn a_flag_the_binary_dropped_is_reported() {
        let extra = format!(
            "{MATCHING_ARGS}\n[[args]]\nname = \"gone\"\nlong = \"gone\"\nrequired = false\ndescription = \"Removed last release\"\n"
        );
        let problems = compare_with_help(&manifest(&extra), EXEC_SOURCE_HELP);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("the manifest declares --gone"),
            "{problems:?}"
        );
    }

    #[test]
    fn an_environment_variable_that_disagrees_is_reported() {
        let wrong = MATCHING_ARGS.replace("EXEC_SOURCE_COMMAND", "EXEC_SOURCE_CMD");
        let problems = compare_with_help(&manifest(&wrong), EXEC_SOURCE_HELP);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("--command reads"), "{problems:?}");
        assert!(problems[0].contains("EXEC_SOURCE_CMD"), "{problems:?}");
    }

    #[test]
    fn an_undeclared_environment_variable_is_reported() {
        let missing = MATCHING_ARGS.replace("env = \"EXEC_SOURCE_INTERVAL\"\n", "");
        let problems = compare_with_help(&manifest(&missing), EXEC_SOURCE_HELP);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("--interval reads"), "{problems:?}");
        assert!(problems[0].contains("none"), "{problems:?}");
    }
}
