//! End-to-end `updater-cli install` against a fake programmer: a shell
//! script that records its argv verbatim and exits per FAKE_TOOL_EXIT.
//! Proves argv-exact spawning (no shell interpretation), placeholder
//! expansion as delivered to the child, port injection, pre-step ordering
//! and exit-code mapping — with no real avrdude/probe-rs installed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Fresh scratch dir per test so recorders can't cross-contaminate.
fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("updater-install-{}-{test}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// The stand-in programmer: appends one "==CALL==" block per invocation,
/// one argv element per line, then exits with $FAKE_TOOL_EXIT.
fn write_fake_tool(dir: &Path) -> PathBuf {
    let tool = dir.join("fake-prog");
    fs::write(&tool, "#!/bin/sh\n{ echo \"==CALL==\"; printf '%s\\n' \"$@\"; } >> \"$FAKE_TOOL_OUT\"\nexit \"${FAKE_TOOL_EXIT:-0}\"\n")
        .unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    tool
}

fn run_install(dir: &Path, extra: &[&str], exit: &str) -> (Output, String) {
    let recorder = dir.join("argv.log");
    let out = Command::new(env!("CARGO_BIN_EXE_updater-cli"))
        .arg("install")
        .args(extra)
        .current_dir(dir)
        .env("FAKE_TOOL_OUT", &recorder)
        .env("FAKE_TOOL_EXIT", exit)
        .output()
        .expect("binary must spawn");
    let log = fs::read_to_string(&recorder).unwrap_or_default();
    (out, log)
}

fn write_config(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    path
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn expands_image_and_spawns_argv_exact() {
    let dir = scratch("argv");
    let tool = write_fake_tool(&dir);
    let image = dir.join("fw.hex");
    fs::write(&image, ":00000001FF\n").unwrap();
    // Shell metacharacters and spaces: if anything shell-interprets the
    // argv, these come out mangled and the exact-match below fails.
    write_config(
        &dir,
        "install.toml",
        &format!(
            "[targets.fake]\ntool = \"{}\"\n\
             args = [\"-U\", \"flash:w:{{image}}:i\", \"two words\", \"$(reboot); echo *\"]\n",
            tool.display()
        ),
    );

    let (out, log) =
        run_install(&dir, &["--target", "fake", "--image", image.to_str().unwrap()], "0");

    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let expected = format!(
        "==CALL==\n-U\nflash:w:{}:i\ntwo words\n$(reboot); echo *\n",
        image.display()
    );
    assert_eq!(log, expected, "child must receive the argv verbatim");
}

#[test]
fn nonzero_tool_exit_maps_to_error_naming_status() {
    let dir = scratch("exit");
    let tool = write_fake_tool(&dir);
    let image = dir.join("fw.hex");
    fs::write(&image, "x").unwrap();
    write_config(
        &dir,
        "install.toml",
        &format!("[targets.fake]\ntool = \"{}\"\nargs = [\"{{image}}\"]\n", tool.display()),
    );

    let (out, _) =
        run_install(&dir, &["--target", "fake", "--image", image.to_str().unwrap()], "3");

    assert!(!out.status.success(), "tool failure must fail the install");
    let err = stderr_of(&out);
    assert!(err.contains("exit status 3"), "must carry the tool's status: {err}");
    assert!(err.contains("fake-prog"), "must name the failing tool: {err}");
}

#[test]
fn pre_steps_run_before_the_main_tool() {
    let dir = scratch("pre");
    let tool = write_fake_tool(&dir);
    let image = dir.join("fw.hex");
    fs::write(&image, "x").unwrap();
    write_config(
        &dir,
        "install.toml",
        &format!(
            "[targets.fake]\ntool = \"{t}\"\nargs = [\"MAIN\", \"{{image}}\"]\n\
             pre = [[\"{t}\", \"FUSES\"]]\n",
            t = tool.display()
        ),
    );

    let (out, log) =
        run_install(&dir, &["--target", "fake", "--image", image.to_str().unwrap()], "0");

    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let expected = format!("==CALL==\nFUSES\n==CALL==\nMAIN\n{}\n", image.display());
    assert_eq!(log, expected, "fuse step must run first, once, in order");
}

#[test]
fn failing_pre_step_stops_before_the_main_tool() {
    let dir = scratch("prefail");
    let tool = write_fake_tool(&dir);
    let image = dir.join("fw.hex");
    fs::write(&image, "x").unwrap();
    write_config(
        &dir,
        "install.toml",
        &format!(
            "[targets.fake]\ntool = \"{t}\"\nargs = [\"MAIN\"]\npre = [[\"{t}\", \"FUSES\"]]\n",
            t = tool.display()
        ),
    );

    let (out, log) =
        run_install(&dir, &["--target", "fake", "--image", image.to_str().unwrap()], "1");

    assert!(!out.status.success());
    assert!(!log.contains("MAIN"), "main tool must not run after a failed pre step: {log}");
}

#[test]
fn port_flag_is_injected_via_port_arg() {
    let dir = scratch("port");
    let tool = write_fake_tool(&dir);
    let image = dir.join("fw.hex");
    fs::write(&image, "x").unwrap();
    write_config(
        &dir,
        "install.toml",
        &format!(
            "[targets.fake]\ntool = \"{}\"\nargs = [\"{{image}}\"]\nport_arg = \"-P\"\n",
            tool.display()
        ),
    );

    let (out, log) = run_install(
        &dir,
        &["--target", "fake", "--image", image.to_str().unwrap(), "--port", "/dev/ttyACM7"],
        "0",
    );

    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert!(log.ends_with("-P\n/dev/ttyACM7\n"), "port must arrive as its own argv pair: {log}");
}

#[test]
fn missing_config_says_how_to_provide_one() {
    let dir = scratch("noconfig");
    let (out, _) = run_install(&dir, &["--target", "x", "--image", "fw.hex"], "0");
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("install.toml"), "must name the searched file: {err}");
    assert!(err.contains("--config"), "must offer the flag: {err}");
}

#[test]
fn explicit_config_path_that_does_not_exist_is_named() {
    let dir = scratch("badconfig");
    let (out, _) = run_install(
        &dir,
        &["--target", "x", "--image", "fw.hex", "--config", "nope/none.toml"],
        "0",
    );
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("nope/none.toml"), "{}", stderr_of(&out));
}

#[test]
fn missing_image_is_reported_before_any_tool_runs() {
    let dir = scratch("noimage");
    let tool = write_fake_tool(&dir);
    write_config(
        &dir,
        "install.toml",
        &format!("[targets.fake]\ntool = \"{}\"\nargs = [\"{{image}}\"]\n", tool.display()),
    );

    let (out, log) = run_install(&dir, &["--target", "fake", "--image", "ghost.hex"], "0");

    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("ghost.hex"), "{}", stderr_of(&out));
    assert!(log.is_empty(), "no tool may run without an image: {log}");
}
