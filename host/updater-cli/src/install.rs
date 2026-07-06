// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Adrian Erlacher

//! `updater-cli install` — installs the *bootloader itself* through a debug
//! adapter (UPDI/SWD/JTAG) by driving the right external programmer
//! (avrdude, probe-rs, openocd) from per-target templates in `install.toml`.
//!
//! Config search order: `--config <path>` if given, else `./install.toml`.
//!
//! Commands are spawned argv-exact — never through a shell — so nothing in
//! a path, port name or template argument can be reinterpreted as shell
//! syntax. The only text transformation is `{image}` / `{port}` placeholder
//! substitution, and any other `{...}` is rejected instead of guessed at.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

/// Parsed `install.toml`: a map so error messages can list what exists,
/// ordered so those listings are stable.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallConfig {
    targets: BTreeMap<String, Target>,
}

/// One programmer recipe. `args` may embed `{image}` / `{port}`; `pre` runs
/// first (e.g. a separate fuse step) under the same substitution rules with
/// element 0 as the literal tool name.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    tool: String,
    args: Vec<String>,
    #[serde(default)]
    pre: Vec<Vec<String>>,
    port_arg: Option<String>,
}

/// One fully-expanded command, ready to spawn argv-exact.
#[derive(Debug, PartialEq, Eq)]
pub struct Invocation {
    pub tool: String,
    pub args: Vec<String>,
}

/// Parse and validate config text; `source` names the file in errors.
/// Syntax and schema errors keep `toml`'s line/column + snippet context;
/// value-level rules (empty tool, empty pre step) are checked here.
pub fn parse_config(text: &str, source: &str) -> Result<InstallConfig> {
    let cfg: InstallConfig =
        toml::from_str(text).with_context(|| format!("parsing {source}"))?;
    for (name, t) in &cfg.targets {
        if t.tool.trim().is_empty() {
            bail!(
                "{source}: targets.{name}: tool is empty — name the programmer \
                 executable to run (e.g. \"avrdude\" or \"probe-rs\")"
            );
        }
        for (i, step) in t.pre.iter().enumerate() {
            if step.first().map_or(true, |tool| tool.trim().is_empty()) {
                bail!(
                    "{source}: targets.{name}: pre[{i}] is empty — each pre step \
                     is an argv list starting with the tool to run, \
                     e.g. [\"avrdude\", \"-U\", \"bootsize:w:8:m\"]"
                );
            }
        }
    }
    Ok(cfg)
}

impl InstallConfig {
    /// Look up a target by name; the error lists every available target.
    pub fn target(&self, name: &str, source: &str) -> Result<&Target> {
        self.targets.get(name).ok_or_else(|| {
            let available: Vec<&str> = self.targets.keys().map(String::as_str).collect();
            anyhow!(
                "no target {name:?} in {source} — available: {} \
                 (add a [targets.{name}] table to define it)",
                available.join(", ")
            )
        })
    }
}

/// Expand a target into the command sequence to run (pre steps, then the
/// main tool), applying placeholder substitution and `--port` injection.
pub fn plan(target: &Target, image: &str, port: Option<&str>) -> Result<Vec<Invocation>> {
    let mut port_consumed = false;
    let mut out = Vec::with_capacity(target.pre.len() + 1);
    for step in &target.pre {
        // Element 0 is the tool name, literal by definition; only its
        // arguments participate in substitution.
        out.push(Invocation {
            tool: step[0].clone(),
            args: expand_args(&step[1..], image, port, &mut port_consumed)?,
        });
    }
    let mut args = expand_args(&target.args, image, port, &mut port_consumed)?;
    if let Some(p) = port {
        if !port_consumed {
            let Some(flag) = &target.port_arg else {
                bail!(
                    "this target does not accept a port: its template has no \
                     {{port}} placeholder and no port_arg — add one of them to \
                     the target in install.toml, or drop --port (got {p:?})"
                );
            };
            args.push(flag.clone());
            args.push(p.to_owned());
        }
    }
    out.push(Invocation { tool: target.tool.clone(), args });
    Ok(out)
}

fn expand_args(
    args: &[String],
    image: &str,
    port: Option<&str>,
    port_consumed: &mut bool,
) -> Result<Vec<String>> {
    args.iter().map(|arg| expand_one(arg, image, port, port_consumed)).collect()
}

/// Substitute `{image}` / `{port}` inside one argument. Anything else in
/// braces is rejected: a typo silently passed through would reach the
/// programmer and fail there with a far worse message — or not fail at all.
fn expand_one(
    arg: &str,
    image: &str,
    port: Option<&str>,
    port_consumed: &mut bool,
) -> Result<String> {
    let mut out = String::with_capacity(arg.len());
    let mut rest = arg;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            bail!(
                "unclosed '{{' in template argument {arg:?} — placeholders are \
                 {{image}} and {{port}}; fix the argument in install.toml"
            );
        };
        match &after[..close] {
            "image" => out.push_str(image),
            "port" => {
                let Some(p) = port else {
                    bail!(
                        "template argument {arg:?} needs a port — pass \
                         --port <dev> (e.g. --port /dev/ttyACM0)"
                    );
                };
                out.push_str(p);
                *port_consumed = true;
            }
            other => bail!(
                "unknown placeholder {{{other}}} in template argument {arg:?} — \
                 only {{image}} and {{port}} are substituted; fix install.toml"
            ),
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Resolve `tool` against the given PATH string (`resolve_tool` passes the
/// real one); split out so the search is testable without touching the
/// environment.
fn resolve_tool_in(tool: &str, path_var: &str) -> Result<PathBuf> {
    // A slash means "this exact file" (execvp semantics): no PATH search.
    if tool.contains('/') {
        let p = PathBuf::from(tool);
        if is_executable(&p) {
            return Ok(p);
        }
        bail!(
            "{tool} does not exist or is not executable — fix the tool path \
             in install.toml"
        );
    }
    for dir in path_var.split(':').filter(|d| !d.is_empty()) {
        let candidate = Path::new(dir).join(tool);
        if is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    bail!(
        "{tool:?} not found on PATH — install it first:\n  \
         avrdude:   apt install avrdude       (or dnf/pacman/brew equivalent)\n  \
         probe-rs:  cargo install probe-rs-tools   (https://probe.rs)\n  \
         openocd:   apt install openocd\n\
         or point the target's tool at an absolute path in install.toml"
    )
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

pub fn resolve_tool(tool: &str) -> Result<PathBuf> {
    resolve_tool_in(tool, &std::env::var("PATH").unwrap_or_default())
}

/// Entry point for the subcommand: locate + parse config, plan, resolve
/// every tool — all before any child runs, so a broken template or missing
/// tool can never leave the target half-programmed after a fuse step.
pub fn run_cli(
    target_name: &str,
    image: &Path,
    port: Option<&str>,
    config: Option<&Path>,
) -> Result<()> {
    let (text, source) = read_config(config)?;
    let cfg = parse_config(&text, &source)?;
    let target = cfg.target(target_name, &source)?;

    if !image.is_file() {
        bail!(
            "image {} does not exist — build the bootloader hex first \
             (e.g. make -C device/ports/avr_ea_twi) or fix the --image path",
            image.display()
        );
    }
    let image_str = image
        .to_str()
        .with_context(|| format!("image path {} is not valid UTF-8", image.display()))?;

    let invocations = plan(target, image_str, port)?;
    let resolved: Vec<PathBuf> = invocations
        .iter()
        .map(|inv| resolve_tool(&inv.tool))
        .collect::<Result<_>>()?;

    for (inv, exe) in invocations.iter().zip(&resolved) {
        let shown: Vec<String> = inv.args.iter().map(|a| display_arg(a)).collect();
        eprintln!("+ {} {}", inv.tool, shown.join(" "));
        // Inherited stdio streams the programmer's output live; status()
        // spawns argv-exact with no shell in between.
        let status = Command::new(exe)
            .args(&inv.args)
            .status()
            .with_context(|| format!("failed to start {}", exe.display()))?;
        if !status.success() {
            match status.code() {
                Some(code) => bail!(
                    "{} failed with exit status {code} — its output above says why; \
                     fix that and re-run",
                    exe.display()
                ),
                None => bail!("{} was killed by a signal", exe.display()),
            }
        }
    }
    println!("install OK: {target_name} via {}", target.tool);
    Ok(())
}

/// Quote an argument for the `+ tool args ...` echo line only (display,
/// never execution — the spawn stays argv-exact): arguments containing
/// whitespace, and empty ones, are shown quoted so the echoed line has an
/// unambiguous token count.
fn display_arg(arg: &str) -> String {
    if arg.is_empty() || arg.chars().any(char::is_whitespace) {
        format!("{arg:?}")
    } else {
        arg.to_owned()
    }
}

/// `--config <path>` if given, else `./install.toml` — the documented
/// search order. Returns the text plus the name used in error messages.
fn read_config(explicit: Option<&Path>) -> Result<(String, String)> {
    let path = match explicit {
        Some(p) => p.to_path_buf(),
        None => {
            let p = PathBuf::from("install.toml");
            if !p.is_file() {
                bail!(
                    "no install.toml in the current directory — run where one \
                     lives, pass --config <file>, or start from the reference \
                     install.toml at the updater-sdk repo root"
                );
            }
            p
        }
    };
    let text = fs::read_to_string(&path)
        .with_context(|| format!("reading {} — pass --config an existing install.toml", path.display()))?;
    Ok((text, path.display().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(toml_text: &str, name: &str) -> Target {
        let mut cfg = parse_config(toml_text, "test.toml").expect("config must parse");
        cfg.targets.remove(name).expect("target must exist")
    }

    const REFERENCE: &str = r#"
[targets.avr64ea28-updi]
tool = "avrdude"
args = ["-c", "atmelice_updi", "-p", "avr64ea28", "-U", "flash:w:{image}:i"]
pre = [["avrdude", "-U", "bootsize:w:8:m"]]
port_arg = "-P"
"#;

    // -- parse / validate ------------------------------------------------

    #[test]
    fn parses_the_full_schema() {
        let cfg = parse_config(REFERENCE, "install.toml").unwrap();
        let t = cfg.target("avr64ea28-updi", "install.toml").unwrap();
        assert_eq!(t.tool, "avrdude");
        assert_eq!(t.args[4], "-U");
        assert_eq!(t.pre, vec![vec!["avrdude", "-U", "bootsize:w:8:m"]]);
        assert_eq!(t.port_arg.as_deref(), Some("-P"));
    }

    #[test]
    fn syntax_error_carries_line_context() {
        let err = parse_config("[targets.x]\ntool = \n", "broken.toml").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("broken.toml"), "must name the file: {msg}");
        assert!(msg.contains("line 2"), "must point at the line: {msg}");
    }

    #[test]
    fn missing_tool_field_is_located() {
        let err =
            parse_config("[targets.x]\nargs = [\"-v\"]\n", "install.toml").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("tool"), "must name the missing field: {msg}");
        assert!(msg.contains("line"), "must carry line context: {msg}");
    }

    #[test]
    fn unknown_key_is_rejected_with_location() {
        let err = parse_config(
            "[targets.x]\ntool = \"t\"\narg = [\"typo\"]\n",
            "install.toml",
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("arg"), "must name the stray key: {msg}");
        assert!(msg.contains("line 3"), "must point at it: {msg}");
    }

    #[test]
    fn empty_tool_rejected_naming_the_target() {
        let err = parse_config("[targets.pico]\ntool = \"\"\nargs = []\n", "install.toml")
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("pico"), "must name the target: {msg}");
        assert!(msg.contains("tool"), "must say what is wrong: {msg}");
    }

    #[test]
    fn empty_pre_step_rejected() {
        let err = parse_config(
            "[targets.x]\ntool = \"t\"\nargs = []\npre = [[]]\n",
            "install.toml",
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("pre"), "must name the offending field: {msg}");
    }

    #[test]
    fn unknown_target_lists_available_ones() {
        let cfg = parse_config(REFERENCE, "my.toml").unwrap();
        let err = cfg.target("rp2040", "my.toml").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("rp2040"), "must name the request: {msg}");
        assert!(msg.contains("avr64ea28-updi"), "must list what exists: {msg}");
        assert!(msg.contains("my.toml"), "must name the file: {msg}");
    }

    #[test]
    fn shipped_reference_config_is_valid_and_keeps_the_bootsize_contract() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../install.toml");
        let text = std::fs::read_to_string(path).expect("reference install.toml must ship");
        let cfg = parse_config(&text, "install.toml").unwrap();
        let avr = cfg.target("avr64ea28-updi", "install.toml").unwrap();
        assert_eq!(avr.tool, "avrdude");
        assert!(
            avr.args.contains(&"bootsize:w:8:m".to_string()),
            "BOOTSIZE=8 is the port's install contract; without it the app \
             region is unwritable: {:?}",
            avr.args
        );
        let plan = plan(avr, "fw.hex", None).unwrap();
        assert!(plan.last().unwrap().args.contains(&"flash:w:fw.hex:i".to_string()));
        cfg.target("rp2040-swd-example", "install.toml").unwrap();
    }

    // -- template expansion ----------------------------------------------

    #[test]
    fn image_placeholder_expands_inside_a_token() {
        let t = target(REFERENCE, "avr64ea28-updi");
        let plan = plan(&t, "fw.hex", None).unwrap();
        let main = plan.last().unwrap();
        assert_eq!(main.args.last().unwrap(), "flash:w:fw.hex:i");
    }

    #[test]
    fn port_placeholder_expands_when_port_given() {
        let t = target(
            "[targets.x]\ntool = \"t\"\nargs = [\"--port={port}\", \"{image}\"]\n",
            "x",
        );
        let plan = plan(&t, "fw.hex", Some("/dev/ttyACM0")).unwrap();
        assert_eq!(plan[0].args, vec!["--port=/dev/ttyACM0", "fw.hex"]);
    }

    #[test]
    fn port_placeholder_without_port_flag_errors_actionably() {
        let t = target("[targets.x]\ntool = \"t\"\nargs = [\"{port}\"]\n", "x");
        let msg = format!("{:#}", plan(&t, "fw.hex", None).unwrap_err());
        assert!(msg.contains("--port"), "must tell the user what to pass: {msg}");
    }

    #[test]
    fn unknown_placeholder_rejected_by_name() {
        let t = target("[targets.x]\ntool = \"t\"\nargs = [\"{flash}\"]\n", "x");
        let msg = format!("{:#}", plan(&t, "fw.hex", None).unwrap_err());
        assert!(msg.contains("{flash}"), "must name the offender: {msg}");
        assert!(msg.contains("{image}"), "must list what is supported: {msg}");
        assert!(msg.contains("{port}"), "must list what is supported: {msg}");
    }

    #[test]
    fn unmatched_brace_rejected() {
        let t = target("[targets.x]\ntool = \"t\"\nargs = [\"oops{image\"]\n", "x");
        let msg = format!("{:#}", plan(&t, "fw.hex", None).unwrap_err());
        assert!(msg.contains("oops{image"), "must show the broken argument: {msg}");
    }

    #[test]
    fn port_flag_appends_port_arg_pair_when_template_has_no_port() {
        let t = target(REFERENCE, "avr64ea28-updi");
        let plan = plan(&t, "fw.hex", Some("/dev/ttyACM1")).unwrap();
        let main = plan.last().unwrap();
        let n = main.args.len();
        assert_eq!(&main.args[n - 2..], ["-P", "/dev/ttyACM1"]);
    }

    #[test]
    fn port_flag_without_any_port_slot_errors() {
        let t = target("[targets.x]\ntool = \"t\"\nargs = [\"{image}\"]\n", "x");
        let msg = format!("{:#}", plan(&t, "fw.hex", Some("/dev/ttyACM0")).unwrap_err());
        assert!(msg.contains("{port}"), "must explain how to accept a port: {msg}");
        assert!(msg.contains("port_arg"), "must offer the alternative: {msg}");
    }

    #[test]
    fn no_port_means_no_injection() {
        let t = target(REFERENCE, "avr64ea28-updi");
        let plan = plan(&t, "fw.hex", None).unwrap();
        let main = plan.last().unwrap();
        assert!(!main.args.contains(&"-P".to_string()), "{:?}", main.args);
    }

    #[test]
    fn pre_steps_run_first_expanded_with_literal_tool() {
        let t = target(
            "[targets.x]\ntool = \"t\"\nargs = [\"{image}\"]\n\
             pre = [[\"fuse-tool\", \"write\", \"{image}\"]]\n",
            "x",
        );
        let plan = plan(&t, "fw.hex", None).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0], Invocation {
            tool: "fuse-tool".into(),
            args: vec!["write".into(), "fw.hex".into()],
        });
        assert_eq!(plan[1].tool, "t");
    }

    #[test]
    fn port_used_only_in_pre_still_counts_as_consumed() {
        let t = target(
            "[targets.x]\ntool = \"t\"\nargs = [\"{image}\"]\n\
             pre = [[\"fuse-tool\", \"{port}\"]]\n",
            "x",
        );
        let plan = plan(&t, "fw.hex", Some("/dev/ttyACM0")).unwrap();
        assert_eq!(plan[0].args, vec!["/dev/ttyACM0"]);
        assert_eq!(plan[1].args, vec!["fw.hex"], "no injection into main args");
    }

    // -- echo-line display quoting (display only, spawn is argv-exact) ------

    #[test]
    fn display_arg_quotes_whitespace_and_empty_only() {
        assert_eq!(display_arg("plain"), "plain");
        assert_eq!(display_arg("flash:w:fw.hex:i"), "flash:w:fw.hex:i");
        assert_eq!(display_arg("with space"), "\"with space\"");
        assert_eq!(display_arg(""), "\"\"");
        assert_eq!(display_arg("tab\there"), "\"tab\\there\"");
    }

    // -- tool resolution ---------------------------------------------------

    #[test]
    fn resolves_executable_from_path_dirs() {
        let dir = std::env::temp_dir().join(format!("updater-inst-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("fake-prog");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();

        let path_var = format!("/nonexistent:{}", dir.display());
        assert_eq!(resolve_tool_in("fake-prog", &path_var).unwrap(), exe);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_tool_error_names_the_install_ladder() {
        let msg = format!("{:#}", resolve_tool_in("avrdude", "/nonexistent").unwrap_err());
        assert!(msg.contains("avrdude"), "must name the tool: {msg}");
        assert!(msg.contains("apt install avrdude"), "ladder must be actionable: {msg}");
        assert!(msg.contains("probe-rs-tools"), "ladder must cover probe-rs: {msg}");
        assert!(msg.contains("openocd"), "ladder must cover openocd: {msg}");
    }

    #[test]
    fn path_like_tool_is_checked_directly_not_via_path() {
        let msg =
            format!("{:#}", resolve_tool_in("/no/such/dir/avrdude", "/usr/bin").unwrap_err());
        assert!(msg.contains("/no/such/dir/avrdude"), "must show the path: {msg}");
    }
}
