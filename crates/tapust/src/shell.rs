//! ghostty's automatic shell integration (termio/shell_integration.zig) with
//! its default configuration: `shell-integration = detect` and the default
//! `shell-integration-features`, plus the variables from Exec.zig that the
//! integration scripts read.
//!
//! ghostty turns a rewritten command into a string for `/bin/sh -c`; tapust
//! runs programs directly, so commands here stay argument vectors.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// The integration scripts, ghostty's `<resources>/shell-integration`.
const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/shell-integration");

/// The environment of a process, ghostty's `EnvMap`.
pub type Env = HashMap<OsString, OsString>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shell {
    Bash,
    Elvish,
    Fish,
    Nushell,
    Zsh,
}

/// ghostty's `shell-integration-features`.
struct Features {
    cursor: bool,
    sudo: bool,
    title: bool,
    ssh_env: bool,
    ssh_terminfo: bool,
    path: bool,
}

impl Default for Features {
    fn default() -> Self {
        Features { cursor: true, sudo: false, title: true, ssh_env: false, ssh_terminfo: false, path: true }
    }
}

/// Exec.zig's setup of a pane's process: the variables the scripts read, then
/// the integration when `argv` runs a supported shell. Returns the command to run.
pub fn prepare(argv: Vec<OsString>, env: &mut Env) -> Vec<OsString> {
    // Lets the scripts find the binary even if the shell's startup files reset PATH.
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        env.insert("TAPUST_BIN_DIR".into(), dir.into());
    }
    // ghostty's `cursor-style-blink` is unset by default, which means blinking.
    setup_features(env, &Features::default(), true);
    setup(Path::new(DIR), &argv, env).unwrap_or(argv)
}

/// Sets up `env` for the integration of `argv`'s shell and returns the command
/// that starts it, or None if the shell is not supported or cannot be integrated.
fn setup(dir: &Path, argv: &[OsString], env: &mut Env) -> Option<Vec<OsString>> {
    match detect_shell(argv)? {
        Shell::Bash => setup_bash(argv, dir, env),
        Shell::Nushell => setup_nushell(argv, dir, env),
        Shell::Zsh => setup_zsh(argv, dir, env),
        Shell::Elvish | Shell::Fish => setup_xdg_data_dirs(dir, env).then(|| argv.to_vec()),
    }
}

fn detect_shell(argv: &[OsString]) -> Option<Shell> {
    Some(match Path::new(argv.first()?).file_name()?.to_str()? {
        "bash" => Shell::Bash,
        "elvish" => Shell::Elvish,
        "fish" => Shell::Fish,
        "nu" => Shell::Nushell,
        "zsh" => Shell::Zsh,
        _ => return None,
    })
}

/// Sets `TAPUST_SHELL_FEATURES` to the enabled features sorted by name.
fn setup_features(env: &mut Env, features: &Features, cursor_blink: bool) {
    let enabled: Vec<&str> = [
        (features.cursor, if cursor_blink { "cursor:blink" } else { "cursor:steady" }),
        (features.path, "path"),
        (features.ssh_env, "ssh-env"),
        (features.ssh_terminfo, "ssh-terminfo"),
        (features.sudo, "sudo"),
        (features.title, "title"),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect();
    if !enabled.is_empty() {
        env.insert("TAPUST_SHELL_FEATURES".into(), enabled.join(",").into());
    }
}

/// Starts bash in POSIX mode, where it reads only the file named by `ENV`: our
/// script, which leaves POSIX mode and sources the startup files itself.
/// Options that would need complex or unsupported handling disable the integration.
fn setup_bash(argv: &[OsString], dir: &Path, env: &mut Env) -> Option<Vec<OsString>> {
    let mut args = argv.iter();
    let mut command = vec![args.next()?.clone(), "--posix".into()];
    // The flags the script must honor; "1" tells it that it was injected, not sourced.
    let mut inject = String::from("1");
    let mut rcfile = None;
    while let Some(arg) = args.next() {
        let bytes = arg.as_bytes();
        if arg == "--posix" {
            return None;
        } else if arg == "--norc" {
            inject.push_str(" --norc");
        } else if arg == "--noprofile" {
            inject.push_str(" --noprofile");
        } else if arg == "--rcfile" || arg == "--init-file" {
            rcfile = args.next();
        } else if bytes.len() > 1 && bytes[0] == b'-' && bytes[1] != b'-' {
            // -c is never interactive.
            if bytes.contains(&b'c') {
                return None;
            }
            command.push(arg.clone());
        } else if arg == "-" || arg == "--" {
            // The rest are arguments, not options.
            command.push(arg.clone());
            command.extend(args.cloned());
            break;
        } else {
            command.push(arg.clone());
        }
    }

    if let Some(user_env) = env.get(OsStr::new("ENV")).cloned() {
        env.insert("TAPUST_BASH_ENV".into(), user_env);
    }
    let script = dir.join("bash/tapust.bash");
    if let Err(err) = File::open(&script) {
        eprintln!("tapust: unable to open {}: {err}", script.display());
        env.remove(OsStr::new("TAPUST_BASH_ENV"));
        return None;
    }
    env.insert("ENV".into(), script.into());
    env.insert("TAPUST_BASH_INJECT".into(), inject.into());
    if let Some(rcfile) = rcfile {
        env.insert("TAPUST_BASH_RCFILE".into(), rcfile.clone());
    }

    // POSIX mode defaults HISTFILE to ~/.sh_history; the script unexports this one.
    if !env.contains_key(OsStr::new("HISTFILE"))
        && let Some(home) = std::env::home_dir()
    {
        let mut histfile = home.into_os_string();
        histfile.push("/.bash_history");
        env.insert("HISTFILE".into(), histfile);
        env.insert("TAPUST_BASH_UNEXPORT_HISTFILE".into(), "1".into());
    }
    Some(command)
}

/// Puts `dir` first in `XDG_DATA_DIRS`, where elvish, fish and nushell find
/// their scripts. The scripts remove it again using
/// `TAPUST_SHELL_INTEGRATION_XDG_DIR` so that it does not reach other programs.
fn setup_xdg_data_dirs(dir: &Path, env: &mut Env) -> bool {
    if let Err(err) = std::fs::read_dir(dir) {
        eprintln!("tapust: unable to open {}: {err}", dir.display());
        return false;
    }
    env.insert("TAPUST_SHELL_INTEGRATION_XDG_DIR".into(), dir.into());

    // Unset means the XDG Base Directory default, which must not be lost (ghostty #2711).
    let current = env.get(OsStr::new("XDG_DATA_DIRS")).map_or(OsStr::new("/usr/local/share:/usr/share"), OsString::as_os_str);
    let mut value = OsString::from(dir);
    if !current.is_empty() {
        value.push(":");
        value.push(current);
    }
    env.insert("XDG_DATA_DIRS".into(), value);
    true
}

/// Makes the `tapust` module available to nushell through `XDG_DATA_DIRS` and
/// has nu `use` it. Options that would need complex or unsupported handling
/// disable the latter.
fn setup_nushell(argv: &[OsString], dir: &Path, env: &mut Env) -> Option<Vec<OsString>> {
    if !setup_xdg_data_dirs(dir, env) {
        return None;
    }
    let mut args = argv.iter();
    // The features are checked at run time by the module itself.
    let mut command = vec![args.next()?.clone(), "--execute".into(), "use tapust *".into()];
    while let Some(arg) = args.next() {
        let bytes = arg.as_bytes();
        if arg == "--command" || arg == "--lsp" {
            return None;
        } else if bytes.len() > 1 && bytes[0] == b'-' && bytes[1] != b'-' {
            // -c is never interactive.
            if bytes.contains(&b'c') {
                return None;
            }
            command.push(arg.clone());
        } else if arg == "-" || arg == "--" {
            // The rest are arguments, not options.
            command.push(arg.clone());
            command.extend(args.cloned());
            break;
        } else {
            command.push(arg.clone());
        }
    }
    Some(command)
}

/// Points `ZDOTDIR` at our zsh directory, whose .zshenv restores the user's
/// `ZDOTDIR` and loads the integration.
fn setup_zsh(argv: &[OsString], dir: &Path, env: &mut Env) -> Option<Vec<OsString>> {
    if let Some(zdotdir) = env.get(OsStr::new("ZDOTDIR")).cloned() {
        env.insert("TAPUST_ZSH_ZDOTDIR".into(), zdotdir);
    }
    let zsh = dir.join("zsh");
    if let Err(err) = std::fs::read_dir(&zsh) {
        eprintln!("tapust: unable to open {}: {err}", zsh.display());
        return None;
    }
    env.insert("ZDOTDIR".into(), zsh.into());
    Some(argv.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MISSING: &str = "/nonexistent";

    fn argv(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    fn get<'a>(env: &'a Env, key: &str) -> Option<&'a str> {
        env.get(OsStr::new(key)).map(|v| v.to_str().unwrap())
    }

    fn dir() -> &'static Path {
        Path::new(DIR)
    }

    #[test]
    fn default_features() {
        let mut env = Env::new();
        assert_eq!(prepare(argv(&["sh"]), &mut env), argv(&["sh"]));
        assert_eq!(get(&env, "TAPUST_SHELL_FEATURES"), Some("cursor:blink,path,title"));
        assert!(get(&env, "TAPUST_BIN_DIR").is_some());
    }

    #[test]
    fn shell_integration_failure() {
        let mut env = Env::new();
        assert_eq!(setup(Path::new(MISSING), &argv(&["sh"]), &mut env), None);
        assert!(env.is_empty());
    }

    #[test]
    fn detects_shells() {
        assert_eq!(detect_shell(&argv(&["sh"])), None);
        assert_eq!(detect_shell(&argv(&["bash"])), Some(Shell::Bash));
        assert_eq!(detect_shell(&argv(&["elvish"])), Some(Shell::Elvish));
        assert_eq!(detect_shell(&argv(&["fish"])), Some(Shell::Fish));
        assert_eq!(detect_shell(&argv(&["nu"])), Some(Shell::Nushell));
        assert_eq!(detect_shell(&argv(&["zsh"])), Some(Shell::Zsh));
        assert_eq!(detect_shell(&argv(&["bash", "-c", "command"])), Some(Shell::Bash));
        assert_eq!(detect_shell(&argv(&["/a b/bash"])), Some(Shell::Bash));
    }

    #[test]
    fn setup_features_lists_enabled_features() {
        let features = |cursor, sudo, title, ssh_env, ssh_terminfo, path| Features { cursor, sudo, title, ssh_env, ssh_terminfo, path };
        let run = |features: Features, cursor_blink| {
            let mut env = Env::new();
            setup_features(&mut env, &features, cursor_blink);
            get(&env, "TAPUST_SHELL_FEATURES").map(str::to_owned)
        };
        assert_eq!(run(features(true, true, true, true, true, true), true).as_deref(), Some("cursor:blink,path,ssh-env,ssh-terminfo,sudo,title"));
        assert_eq!(run(features(false, false, false, false, false, false), true), None);
        assert_eq!(run(features(false, true, false, true, false, false), true).as_deref(), Some("ssh-env,sudo"));
        assert_eq!(run(features(true, false, false, false, false, false), true).as_deref(), Some("cursor:blink"));
        assert_eq!(run(features(true, false, false, false, false, false), false).as_deref(), Some("cursor:steady"));
    }

    #[test]
    fn bash() {
        let mut env = Env::new();
        assert_eq!(setup_bash(&argv(&["bash"]), dir(), &mut env), Some(argv(&["bash", "--posix"])));
        assert_eq!(get(&env, "TAPUST_BASH_INJECT"), Some("1"));
        assert_eq!(get(&env, "ENV"), Some(concat!(env!("CARGO_MANIFEST_DIR"), "/shell-integration/bash/tapust.bash")));
    }

    #[test]
    fn bash_unsupported_options() {
        for cmdline in [
            &["bash", "--posix"][..],
            &["bash", "--rcfile", "script.sh", "--posix"],
            &["bash", "--init-file", "script.sh", "--posix"],
            &["bash", "-c", "script.sh"],
            &["bash", "-ic", "script.sh"],
        ] {
            let mut env = Env::new();
            assert_eq!(setup_bash(&argv(cmdline), dir(), &mut env), None, "{cmdline:?}");
            assert!(env.is_empty());
        }
    }

    #[test]
    fn bash_inject_flags() {
        for (flag, inject) in [("--norc", "1 --norc"), ("--noprofile", "1 --noprofile")] {
            let mut env = Env::new();
            assert_eq!(setup_bash(&argv(&["bash", flag]), dir(), &mut env), Some(argv(&["bash", "--posix"])));
            assert_eq!(get(&env, "TAPUST_BASH_INJECT"), Some(inject));
        }
    }

    #[test]
    fn bash_rcfile() {
        let mut env = Env::new();
        for flag in ["--rcfile", "--init-file"] {
            assert_eq!(setup_bash(&argv(&["bash", flag, "profile.sh"]), dir(), &mut env), Some(argv(&["bash", "--posix"])));
            assert_eq!(get(&env, "TAPUST_BASH_RCFILE"), Some("profile.sh"));
        }
    }

    #[test]
    fn bash_histfile() {
        let mut env = Env::new();
        setup_bash(&argv(&["bash"]), dir(), &mut env);
        assert!(get(&env, "HISTFILE").unwrap().ends_with(".bash_history"));
        assert_eq!(get(&env, "TAPUST_BASH_UNEXPORT_HISTFILE"), Some("1"));

        let mut env = Env::from([("HISTFILE".into(), "my_history".into())]);
        setup_bash(&argv(&["bash"]), dir(), &mut env);
        assert_eq!(get(&env, "HISTFILE"), Some("my_history"));
        assert_eq!(get(&env, "TAPUST_BASH_UNEXPORT_HISTFILE"), None);
    }

    #[test]
    fn bash_env() {
        let mut env = Env::from([("ENV".into(), "env.sh".into())]);
        setup_bash(&argv(&["bash"]), dir(), &mut env);
        assert_eq!(get(&env, "TAPUST_BASH_ENV"), Some("env.sh"));
        assert_eq!(get(&env, "ENV"), Some(concat!(env!("CARGO_MANIFEST_DIR"), "/shell-integration/bash/tapust.bash")));
    }

    #[test]
    fn bash_additional_arguments() {
        let mut env = Env::new();
        for separator in ["-", "--"] {
            let command = setup_bash(&argv(&["bash", separator, "--arg", "file1", "file2"]), dir(), &mut env);
            assert_eq!(command, Some(argv(&["bash", "--posix", separator, "--arg", "file1", "file2"])));
        }
    }

    #[test]
    fn bash_missing_resources() {
        let mut env = Env::new();
        assert_eq!(setup_bash(&argv(&["bash"]), Path::new(MISSING), &mut env), None);
        assert!(env.is_empty());
    }

    #[test]
    fn xdg_empty_xdg_data_dirs() {
        let mut env = Env::new();
        assert!(setup_xdg_data_dirs(dir(), &mut env));
        assert_eq!(get(&env, "TAPUST_SHELL_INTEGRATION_XDG_DIR"), Some(DIR));
        assert_eq!(get(&env, "XDG_DATA_DIRS"), Some(format!("{DIR}:/usr/local/share:/usr/share").as_str()));
    }

    #[test]
    fn xdg_existing_xdg_data_dirs() {
        let mut env = Env::from([("XDG_DATA_DIRS".into(), "/opt/share".into())]);
        assert!(setup_xdg_data_dirs(dir(), &mut env));
        assert_eq!(get(&env, "TAPUST_SHELL_INTEGRATION_XDG_DIR"), Some(DIR));
        assert_eq!(get(&env, "XDG_DATA_DIRS"), Some(format!("{DIR}:/opt/share").as_str()));
    }

    #[test]
    fn xdg_missing_resources() {
        let mut env = Env::new();
        assert!(!setup_xdg_data_dirs(Path::new(MISSING), &mut env));
        assert!(env.is_empty());
    }

    #[test]
    fn nushell() {
        let mut env = Env::new();
        assert_eq!(setup_nushell(&argv(&["nu"]), dir(), &mut env), Some(argv(&["nu", "--execute", "use tapust *"])));
        assert_eq!(get(&env, "TAPUST_SHELL_INTEGRATION_XDG_DIR"), Some(DIR));
        assert!(get(&env, "XDG_DATA_DIRS").unwrap().starts_with(DIR));
    }

    #[test]
    fn nushell_unsupported_options() {
        for cmdline in [&["nu", "--command", "exit"][..], &["nu", "--lsp"], &["nu", "-c", "script.sh"], &["nu", "-ic", "script.sh"]] {
            let mut env = Env::new();
            assert_eq!(setup_nushell(&argv(cmdline), dir(), &mut env), None, "{cmdline:?}");
            assert!(get(&env, "XDG_DATA_DIRS").is_some());
            assert!(get(&env, "TAPUST_SHELL_INTEGRATION_XDG_DIR").is_some());
        }
    }

    #[test]
    fn nushell_missing_resources() {
        let mut env = Env::new();
        assert_eq!(setup_nushell(&argv(&["nu"]), Path::new(MISSING), &mut env), None);
        assert!(env.is_empty());
    }

    #[test]
    fn zsh() {
        let mut env = Env::new();
        assert_eq!(setup_zsh(&argv(&["zsh"]), dir(), &mut env), Some(argv(&["zsh"])));
        assert_eq!(get(&env, "ZDOTDIR"), Some(concat!(env!("CARGO_MANIFEST_DIR"), "/shell-integration/zsh")));
        assert_eq!(get(&env, "TAPUST_ZSH_ZDOTDIR"), None);
    }

    #[test]
    fn zsh_zdotdir() {
        let mut env = Env::from([("ZDOTDIR".into(), "$HOME/.config/zsh".into())]);
        assert_eq!(setup_zsh(&argv(&["zsh"]), dir(), &mut env), Some(argv(&["zsh"])));
        assert_eq!(get(&env, "ZDOTDIR"), Some(concat!(env!("CARGO_MANIFEST_DIR"), "/shell-integration/zsh")));
        assert_eq!(get(&env, "TAPUST_ZSH_ZDOTDIR"), Some("$HOME/.config/zsh"));
    }

    #[test]
    fn zsh_missing_resources() {
        let mut env = Env::new();
        assert_eq!(setup_zsh(&argv(&["zsh"]), Path::new(MISSING), &mut env), None);
        assert!(env.is_empty());
    }
}
