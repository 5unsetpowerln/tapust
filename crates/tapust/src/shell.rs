//! Shell integration. bash and zsh learn to report their prompts, commands,
//! exit statuses, working directory and title to the terminal (OSC 133, 7 and
//! 2), which fish and nushell already do; all four get a bar cursor at the
//! prompt.
//!
//! The scripts are embedded and written to `$XDG_RUNTIME_DIR` when a shell
//! starts, and each shell is pointed at its script without changing which of
//! the user's startup files run:
//! - bash: an interactive shell reads the script as its `--rcfile`, which reads
//!   the user's rc file. A login shell reads no rc file, so it starts in POSIX
//!   mode, where the only startup file is `$ENV`, and the script leaves POSIX
//!   mode and reads the login files.
//! - zsh: `ZDOTDIR` points at the script as `.zshenv`, which restores `ZDOTDIR`
//!   and reads the user's `.zshenv`; zsh reads the rest from there.
//! - fish and nushell: an `XDG_DATA_DIRS` entry with a vendor configuration file,
//!   which restores `XDG_DATA_DIRS`.
//! - elvish runs unmodified: its only hook is `-rc`, and an rc file that loads
//!   the user's rc can only give the REPL copies of its variables
//!   (`edit:add-vars`) and resolves its relative imports elsewhere.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

pub type Env = HashMap<OsString, OsString>;

/// The scripts by their path in the integration directory.
const SCRIPTS: [(&str, &str); 4] = [
    (BASH_SCRIPT, include_str!("../shell/bash/tapust.bash")),
    ("zsh/.zshenv", include_str!("../shell/zsh/.zshenv")),
    ("fish/vendor_conf.d/tapust.fish", include_str!("../shell/fish/vendor_conf.d/tapust.fish")),
    ("nushell/vendor/autoload/tapust.nu", include_str!("../shell/nushell/vendor/autoload/tapust.nu")),
];
const BASH_SCRIPT: &str = "bash/tapust.bash";

/// How a supported shell is started with the integration.
#[derive(Debug, PartialEq)]
enum Plan {
    /// `args` are the arguments without a `--rcfile`, which goes to `rcfile`.
    Bash { login: bool, noprofile: bool, rcfile: Option<OsString>, args: Vec<OsString> },
    Zsh,
    Fish,
    Nushell,
}

/// Returns the command line for `argv` and sets `env` up for tapust's shell
/// integration, whose prompt cursor blinks with `cursor_blink` (ghostty's
/// cursor:blink or cursor:steady); `login` tells that argv[0] will start
/// with '-'.
pub fn prepare(argv: Vec<OsString>, login: bool, cursor_blink: bool, env: &mut Env) -> Vec<OsString> {
    env.insert("TAPUST_CURSOR_BLINK".into(), if cursor_blink { "1" } else { "0" }.into());
    let Some(plan) = plan(&argv, login, env) else { return argv };
    let dir = match env.get(OsStr::new("XDG_RUNTIME_DIR")) {
        Some(runtime) if !runtime.is_empty() => Path::new(runtime).join("tapust/shell-integration"),
        _ => {
            eprintln!("tapust: XDG_RUNTIME_DIR is not set, so the shell runs without shell integration");
            return argv;
        }
    };
    if let Err(e) = write_scripts(&dir) {
        eprintln!("tapust: cannot write the shell integration to {}: {e}", dir.display());
        return argv;
    }
    apply(plan, argv, &dir, env)
}

/// The command line for ghostty's shell command `line`, run by `/bin/sh -c`,
/// with the integration of the shell its words start, which ghostty finds
/// the same way. bash's integration changes its arguments, so it runs as
/// those words (as ghostty's does).
pub fn prepare_command(line: &str, cursor_blink: bool, env: &mut Env) -> Vec<OsString> {
    let sh: Vec<OsString> = vec!["/bin/sh".into(), "-c".into(), line.into()];
    match words(line) {
        Some(words) => {
            let argv = prepare(words.clone(), false, cursor_blink, env);
            if argv == words { sh } else { argv }
        }
        None => prepare(sh, false, cursor_blink, env),
    }
}

/// The words of a command line as /bin/sh splits them, for one with
/// quotes and backslashes but no expansions or operators.
fn words(line: &str) -> Option<Vec<OsString>> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => words.extend(word.take().map(OsString::from)),
            '\'' => {
                let word = word.get_or_insert_default();
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => word.push(c),
                    }
                }
            }
            '"' => {
                let word = word.get_or_insert_default();
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => {
                            let next = chars.next()?;
                            if !matches!(next, '"' | '\\' | '$' | '`') {
                                word.push('\\');
                            }
                            word.push(next);
                        }
                        '$' | '`' => return None,
                        c => word.push(c),
                    }
                }
            }
            '\\' => word.get_or_insert_default().push(chars.next()?),
            _ if "$`;|&<>()*?[]{}~#\n".contains(c) => return None,
            c => word.get_or_insert_default().push(c),
        }
    }
    words.extend(word.map(OsString::from));
    Some(words)
}

/// The integration for `argv`, if its shell and arguments are supported.
fn plan(argv: &[OsString], login: bool, env: &Env) -> Option<Plan> {
    let (program, args) = argv.split_first()?;
    match Path::new(program).file_name()?.as_bytes() {
        b"bash" => plan_bash(args, login, env),
        b"zsh" => interactive_only(args).then_some(Plan::Zsh),
        b"fish" => interactive_only(args).then_some(Plan::Fish),
        b"nu" => interactive_only(args).then_some(Plan::Nushell),
        _ => None,
    }
}

/// Whether `args` only ask for an interactive or a login shell.
fn interactive_only(args: &[OsString]) -> bool {
    args.iter().all(|arg| matches!(arg.as_bytes(), b"--login" | b"--interactive") || login_or_interactive_flags(arg))
}

/// `-i`, `-l` or both in one argument.
fn login_or_interactive_flags(arg: &OsStr) -> bool {
    matches!(arg.as_bytes(), [b'-', flags @ ..] if !flags.is_empty() && flags.iter().all(|f| matches!(f, b'i' | b'l')))
}

fn plan_bash(args: &[OsString], login: bool, env: &Env) -> Option<Plan> {
    // A shell that starts in POSIX mode reads only $ENV, which it needs for itself.
    let shellopts = env.get(OsStr::new("SHELLOPTS")).map_or(&b""[..], |o| o.as_bytes());
    if env.contains_key(OsStr::new("POSIXLY_CORRECT")) || shellopts.split(|&b| b == b':').any(|o| o == b"posix") {
        return None;
    }
    let (mut login, mut noprofile, mut rcfile, mut rest) = (login, false, None, Vec::new());
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_bytes() {
            b"--login" => login = true,
            b"--noprofile" => noprofile = true,
            b"--rcfile" | b"--init-file" => {
                rcfile = Some(args.next()?.clone());
                continue;
            }
            _ if login_or_interactive_flags(arg) => login |= arg.as_bytes().contains(&b'l'),
            _ => return None,
        }
        rest.push(arg.clone());
    }
    Some(Plan::Bash { login, noprofile, rcfile, args: rest })
}

/// The command line for `plan` with the scripts in `dir`, setting `env` up.
fn apply(plan: Plan, mut argv: Vec<OsString>, dir: &Path, env: &mut Env) -> Vec<OsString> {
    match plan {
        Plan::Bash { login: true, noprofile, args, .. } => {
            save(env, "ENV", "TAPUST_BASH_ENV");
            env.insert("ENV".into(), dir.join(BASH_SCRIPT).into());
            set(env, "TAPUST_BASH_NOPROFILE", noprofile.then(|| "1".into()));
            argv.truncate(1);
            argv.push("--posix".into());
            argv.extend(args);
        }
        Plan::Bash { login: false, rcfile, args, .. } => {
            set(env, "TAPUST_BASH_RCFILE", rcfile);
            argv.truncate(1);
            argv.extend(["--rcfile".into(), dir.join(BASH_SCRIPT).into()]);
            argv.extend(args);
        }
        Plan::Zsh => {
            save(env, "ZDOTDIR", "TAPUST_ZSH_ZDOTDIR");
            env.insert("ZDOTDIR".into(), dir.join("zsh").into());
        }
        Plan::Fish | Plan::Nushell => {
            let mut dirs = dir.as_os_str().to_owned();
            match save(env, "XDG_DATA_DIRS", "TAPUST_XDG_DATA_DIRS") {
                Some(user_dirs) => {
                    dirs.push(":");
                    dirs.push(user_dirs);
                }
                // fish always reads its own data directory, but nushell reads
                // the XDG default only when the variable is unset.
                None if plan == Plan::Nushell => dirs.push(":/usr/local/share:/usr/share"),
                None => {}
            }
            env.insert("XDG_DATA_DIRS".into(), dirs);
        }
    }
    argv
}

fn set(env: &mut Env, key: &str, value: Option<OsString>) {
    match value {
        Some(value) => env.insert(key.into(), value),
        None => env.remove(OsStr::new(key)),
    };
}

/// Copies `key` to `saved`, removing `saved` if `key` is unset, for the script
/// to restore it.
fn save(env: &mut Env, key: &str, saved: &str) -> Option<OsString> {
    let value = env.get(OsStr::new(key)).cloned();
    set(env, saved, value.clone());
    value
}

/// Writes the scripts that differ from those in `dir`, creating it private.
fn write_scripts(dir: &Path) -> io::Result<()> {
    for (name, script) in SCRIPTS {
        let path = dir.join(name);
        if fs::read(&path).is_ok_and(|old| old == script.as_bytes()) {
            continue;
        }
        let parent = path.parent().expect("a script path has a directory");
        fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?;
        // Another tapust may be starting a shell that reads the script now.
        let tmp = parent.join(format!(".tmp{}", std::process::id()));
        fs::write(&tmp, script)?;
        fs::rename(&tmp, &path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: &str = "/run/user/1000/tapust/shell-integration";

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    fn get<'a>(env: &'a Env, key: &str) -> Option<&'a str> {
        env.get(OsStr::new(key)).map(|v| v.to_str().unwrap())
    }

    /// The command line and environment for `argv` started from `env`, or
    /// None if the shell runs unmodified.
    fn run(argv: &[&str], login: bool, mut env: Env) -> Option<(Vec<OsString>, Env)> {
        let plan = plan(&os(argv), login, &env)?;
        let argv = apply(plan, os(argv), Path::new(DIR), &mut env);
        Some((argv, env))
    }

    fn vars(vars: &[(&str, &str)]) -> Env {
        vars.iter().map(|(k, v)| (k.into(), v.into())).collect()
    }

    #[test]
    fn bash_interactive_reads_the_script_as_its_rc_file() {
        let rc = format!("{DIR}/bash/tapust.bash");
        for argv in [&["/bin/bash"][..], &["bash", "-i"], &["bash", "--noprofile", "-i"]] {
            let (args, env) = run(argv, false, Env::new()).unwrap();
            assert_eq!(args, os(&[&[argv[0], "--rcfile", &rc], &argv[1..]].concat()));
            assert_eq!(get(&env, "TAPUST_BASH_RCFILE"), None);
            assert_eq!(get(&env, "ENV"), None);
        }
    }

    #[test]
    fn bash_keeps_the_users_rc_file_for_the_script() {
        let rc = format!("{DIR}/bash/tapust.bash");
        let (args, env) = run(&["bash", "--rcfile", "my.rc", "-i"], false, Env::new()).unwrap();
        assert_eq!(args, os(&["bash", "--rcfile", &rc, "-i"]));
        assert_eq!(get(&env, "TAPUST_BASH_RCFILE"), Some("my.rc"));
        let (args, env) = run(&["bash", "--init-file", "my.rc"], false, Env::new()).unwrap();
        assert_eq!(args, os(&["bash", "--rcfile", &rc]));
        assert_eq!(get(&env, "TAPUST_BASH_RCFILE"), Some("my.rc"));
        assert_eq!(run(&["bash", "--rcfile"], false, Env::new()), None);
    }

    #[test]
    fn bash_login_starts_in_posix_mode_with_the_script_as_env() {
        let rc = format!("{DIR}/bash/tapust.bash");
        for (argv, login) in [(&["bash"][..], true), (&["bash", "-l"], false), (&["bash", "--login"], false), (&["bash", "-il"], false)] {
            let (args, env) = run(argv, login, Env::new()).unwrap();
            assert_eq!(args[..2], os(&["bash", "--posix"])[..]);
            assert_eq!(args[2..], os(&argv[1..])[..]);
            assert_eq!(get(&env, "ENV"), Some(&rc[..]));
            assert_eq!(get(&env, "TAPUST_BASH_ENV"), None);
            assert_eq!(get(&env, "TAPUST_BASH_NOPROFILE"), None);
        }
    }

    #[test]
    fn bash_login_keeps_env_and_noprofile_for_the_script() {
        let (args, env) = run(&["bash", "--noprofile"], true, vars(&[("ENV", "my.sh")])).unwrap();
        assert_eq!(args, os(&["bash", "--posix", "--noprofile"]));
        assert_eq!(get(&env, "TAPUST_BASH_ENV"), Some("my.sh"));
        assert_eq!(get(&env, "TAPUST_BASH_NOPROFILE"), Some("1"));
        // An empty ENV is kept as empty, not as unset.
        let (_, env) = run(&["bash"], true, vars(&[("ENV", "")])).unwrap();
        assert_eq!(get(&env, "TAPUST_BASH_ENV"), Some(""));
    }

    #[test]
    fn bash_runs_unmodified_with_other_arguments_or_in_posix_mode() {
        for argv in [
            &["bash", "-c", "true"][..],
            &["bash", "script.sh"],
            &["bash", "--norc"],
            &["bash", "--posix"],
            &["bash", "-o", "vi"],
            &["bash", "-ix"],
            &["bash", "-"],
            &["sh"],
            &["/usr/bin/bash5"],
        ] {
            assert_eq!(run(argv, false, Env::new()), None, "{argv:?}");
        }
        for v in [&[("POSIXLY_CORRECT", "")][..], &[("SHELLOPTS", "braceexpand:posix")]] {
            assert_eq!(run(&["bash"], true, vars(v)), None, "{v:?}");
        }
        assert!(run(&["bash"], true, vars(&[("SHELLOPTS", "braceexpand:posixx")])).is_some());
    }

    #[test]
    fn zsh_reads_the_script_through_zdotdir() {
        for (argv, login) in [(&["/usr/bin/zsh"][..], true), (&["zsh"], false), (&["zsh", "-l"], false), (&["zsh", "--login", "-i"], false)] {
            let (args, env) = run(argv, login, Env::new()).unwrap();
            assert_eq!(args, os(argv));
            assert_eq!(get(&env, "ZDOTDIR"), Some(&format!("{DIR}/zsh")[..]));
            assert_eq!(get(&env, "TAPUST_ZSH_ZDOTDIR"), None);
        }
        let (_, env) = run(&["zsh"], true, vars(&[("ZDOTDIR", "/home/u/.config/zsh")])).unwrap();
        assert_eq!(get(&env, "TAPUST_ZSH_ZDOTDIR"), Some("/home/u/.config/zsh"));
        for argv in [&["zsh", "-f"][..], &["zsh", "-c", "true"], &["zsh", "--no-rcs"], &["zsh", "script.zsh"]] {
            assert_eq!(run(argv, false, Env::new()), None, "{argv:?}");
        }
    }

    #[test]
    fn fish_and_nushell_find_the_script_through_xdg_data_dirs() {
        let (args, env) = run(&["fish", "-l"], false, vars(&[("XDG_DATA_DIRS", "/usr/share")])).unwrap();
        assert_eq!(args, os(&["fish", "-l"]));
        assert_eq!(get(&env, "XDG_DATA_DIRS"), Some(&format!("{DIR}:/usr/share")[..]));
        assert_eq!(get(&env, "TAPUST_XDG_DATA_DIRS"), Some("/usr/share"));
        let (_, env) = run(&["fish"], true, Env::new()).unwrap();
        assert_eq!(get(&env, "XDG_DATA_DIRS"), Some(DIR));
        assert_eq!(get(&env, "TAPUST_XDG_DATA_DIRS"), None);
        let (_, env) = run(&["nu"], true, Env::new()).unwrap();
        assert_eq!(get(&env, "XDG_DATA_DIRS"), Some(&format!("{DIR}:/usr/local/share:/usr/share")[..]));
        assert_eq!(get(&env, "TAPUST_XDG_DATA_DIRS"), None);
        let (_, env) = run(&["nu", "-i"], false, vars(&[("XDG_DATA_DIRS", "")])).unwrap();
        assert_eq!(get(&env, "XDG_DATA_DIRS"), Some(&format!("{DIR}:")[..]));
        assert_eq!(get(&env, "TAPUST_XDG_DATA_DIRS"), Some(""));
        for argv in [&["fish", "-c", "true"][..], &["fish", "--no-config"], &["nu", "-c", "1"], &["nu", "script.nu"]] {
            assert_eq!(run(argv, false, Env::new()), None, "{argv:?}");
        }
    }

    #[test]
    fn other_shells_run_unmodified() {
        for argv in [&["elvish"][..], &["dash"], &["tcsh"], &["vim"]] {
            assert_eq!(run(argv, true, Env::new()), None, "{argv:?}");
        }
    }

    #[test]
    fn without_a_runtime_directory_the_shell_runs_unmodified() {
        let mut env = vars(&[("ZDOTDIR", "/z")]);
        assert_eq!(prepare(os(&["zsh"]), true, true, &mut env), os(&["zsh"]));
        assert_eq!(env, vars(&[("ZDOTDIR", "/z"), ("TAPUST_CURSOR_BLINK", "1")]));
    }

    #[test]
    fn shell_commands_split_into_words_without_expansions() {
        assert_eq!(words("fish -l"), Some(os(&["fish", "-l"])));
        assert_eq!(words(r#" a  'b c' "d \"e\" \q" f\ g"#), Some(os(&["a", "b c", "d \"e\" \\q", "f g"])));
        for line in ["echo $HOME", "a | b", "a; b", "ls *", "~/bin/x", "a 'b", "\"$x\"", "a # b"] {
            assert_eq!(words(line), None, "{line}");
        }
    }

    /// ghostty runs a shell command through /bin/sh -c with the integration
    /// of the shell it starts; bash's needs its arguments, so it runs them.
    #[test]
    fn shell_commands_run_through_sh_unless_bash_needs_its_arguments() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-shell-command");
        let integration = dir.join("tapust/shell-integration");
        let run = |line: &str| {
            let mut env = vars(&[("XDG_RUNTIME_DIR", dir.to_str().unwrap())]);
            let argv = prepare_command(line, false, &mut env);
            (argv, env)
        };
        let (argv, env) = run("fish -l");
        assert_eq!(argv, os(&["/bin/sh", "-c", "fish -l"]));
        assert_eq!(get(&env, "XDG_DATA_DIRS"), integration.to_str());
        assert_eq!(get(&env, "TAPUST_CURSOR_BLINK"), Some("0"));
        let (argv, _) = run("bash -i");
        assert_eq!(argv, os(&["bash", "--rcfile", integration.join(BASH_SCRIPT).to_str().unwrap(), "-i"]));
        assert_eq!(run("vim 'a b'").0, os(&["/bin/sh", "-c", "vim 'a b'"]));
        assert_eq!(run("bash -c 'echo $0'").0, os(&["/bin/sh", "-c", "bash -c 'echo $0'"]));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scripts_are_written_private_and_rewritten_only_when_they_differ() {
        use std::os::unix::fs::MetadataExt;
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-shell-integration");
        let _ = fs::remove_dir_all(&dir);
        write_scripts(&dir).unwrap();
        for sub in ["", "bash", "nushell/vendor/autoload"] {
            assert_eq!(fs::metadata(dir.join(sub)).unwrap().mode() & 0o777, 0o700, "{sub}");
        }
        let (bash, zsh) = (dir.join(BASH_SCRIPT), dir.join("zsh/.zshenv"));
        let zsh_inode = fs::metadata(&zsh).unwrap().ino();
        fs::write(&bash, "stale").unwrap();
        write_scripts(&dir).unwrap();
        assert_eq!(fs::read_to_string(&bash).unwrap(), SCRIPTS[0].1);
        assert_eq!(fs::metadata(&zsh).unwrap().ino(), zsh_inode);
        fs::remove_dir_all(&dir).unwrap();
    }
}
