//! Shared shell helpers: shell quoting and the one resume-command builder
//! every surface (Live `y`, session detail popup copy + display) routes
//! through. Centralising them keeps the `format!("cd {} && ...")` lint (#28)
//! enforceable and prevents copies from drifting if the escaping policy
//! changes.
//!
//! Two dialects: POSIX sh (`cd '<dir>' && claude -r '<id>'`) for Unix and
//! Git Bash / MSYS on Windows, and PowerShell for native Windows terminals,
//! where cmd.exe has no single-quote quoting and Windows PowerShell 5.1
//! rejects `&&`.

/// Which shell the copied resume command is written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeShell {
    Posix,
    PowerShell,
}

impl ResumeShell {
    /// POSIX everywhere except native Windows. `MSYSTEM` is exported by Git
    /// Bash / MSYS2 shells, whose users paste into a POSIX shell.
    pub fn detect() -> Self {
        if cfg!(windows) && std::env::var_os("MSYSTEM").is_none() {
            Self::PowerShell
        } else {
            Self::Posix
        }
    }
}

/// Quote a cwd for `shell`, expanding leading `~/` (or bare `~`) via the
/// home directory first. Without expansion `cd '~/dev/foo'` is literal.
/// Home absent → plain quote (pasteable, broken — still better than panic).
fn shell_quote_cwd(shell: ResumeShell, path: &str, home: Option<&str>) -> String {
    let sep = match shell {
        ResumeShell::Posix => '/',
        ResumeShell::PowerShell => '\\',
    };
    let expanded: String = if let Some(rest) = path.strip_prefix("~/") {
        match home {
            Some(h) => format!("{h}{sep}{rest}"),
            None => path.to_string(),
        }
    } else if path == "~" {
        home.map_or_else(|| path.to_string(), str::to_string)
    } else {
        path.to_string()
    };
    quote(shell, &expanded)
}

fn quote(shell: ResumeShell, s: &str) -> String {
    match shell {
        ResumeShell::Posix => posix_shell_quote(s),
        ResumeShell::PowerShell => powershell_quote(s),
    }
}

/// Resume command for the current platform's shell (see
/// [`ResumeShell::detect`]). `dir` must be a VERIFIED resume target
/// (`AppState::resume_dir`): `claude -r` only finds a session when the cwd's
/// official slug matches the transcript's storage dir, so callers pass that
/// verified path — never a transcript cwd or a slug reversal directly.
pub fn resume_command(dir: &str, session_id: &str) -> String {
    let home = crate::infrastructure::home_dir();
    resume_command_for(
        ResumeShell::detect(),
        dir,
        session_id,
        home.as_deref().and_then(std::path::Path::to_str),
    )
}

/// Testable core of [`resume_command`]: both interpolations quoted for
/// `shell`. PowerShell uses `-ErrorAction Stop` so a missing dir aborts the
/// line instead of resuming from the wrong cwd (the `&&` equivalent).
fn resume_command_for(
    shell: ResumeShell,
    dir: &str,
    session_id: &str,
    home: Option<&str>,
) -> String {
    let cwd = shell_quote_cwd(shell, dir, home);
    let id = quote(shell, session_id);
    match shell {
        ResumeShell::Posix => format!("cd {cwd} && claude -r {id}"),
        ResumeShell::PowerShell => {
            format!("Set-Location -LiteralPath {cwd} -ErrorAction Stop; claude -r {id}")
        }
    }
}

/// POSIX single-quote shell escape. Wraps the input in `'...'` and turns
/// any embedded `'` into `'\''`, which is the standard portable way to
/// embed arbitrary bytes in a sh / bash / zsh command line.
pub fn posix_shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// PowerShell verbatim-string escape: wrap in `'...'` and double every
/// single-quote character. PowerShell also treats the typographic quotes
/// U+2018..U+201B as single quotes, so those are doubled too — otherwise a
/// `’` in a path would close the string early.
pub fn powershell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        out.push(c);
        if is_powershell_single_quote(c) {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

fn is_powershell_single_quote(c: char) -> bool {
    matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}')
}

#[cfg(test)]
mod tests {
    use super::{
        ResumeShell, posix_shell_quote, powershell_quote, resume_command_for, shell_quote_cwd,
    };

    fn resume_command(dir: &str, session_id: &str) -> String {
        resume_command_for(ResumeShell::Posix, dir, session_id, None)
    }

    fn shell_quote_cwd_with_home(path: &str, home: Option<&str>) -> String {
        shell_quote_cwd(ResumeShell::Posix, path, home)
    }

    #[test]
    fn powershell_resume_command_quotes_both_interpolations() {
        let cmd = resume_command_for(
            ResumeShell::PowerShell,
            r"C:\Users\me\dev\multi word",
            "0aef-1234",
            None,
        );
        assert_eq!(
            cmd,
            r"Set-Location -LiteralPath 'C:\Users\me\dev\multi word' -ErrorAction Stop; claude -r '0aef-1234'"
        );
    }

    #[test]
    fn powershell_resume_command_expands_tilde_with_backslash() {
        let cmd = resume_command_for(
            ResumeShell::PowerShell,
            "~/dev",
            "id",
            Some(r"C:\Users\tester"),
        );
        assert!(cmd.starts_with(r"Set-Location -LiteralPath 'C:\Users\tester\dev' "));
    }

    #[test]
    fn powershell_quote_doubles_all_single_quote_forms() {
        assert_eq!(powershell_quote("a'b"), "'a''b'");
        assert_eq!(powershell_quote("a\u{2019}b"), "'a\u{2019}\u{2019}b'");
        assert_eq!(powershell_quote("$(evil); x"), "'$(evil); x'");
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(1024))]

        /// PowerShell verbatim strings decode by collapsing each doubled
        /// quote; the decoded form must equal the input, and no quote char
        /// may appear un-doubled (which would end the string early).
        #[test]
        fn powershell_quote_roundtrips(s in ".*") {
            let quoted: Vec<char> = powershell_quote(&s).chars().collect();
            let inner = &quoted[1..quoted.len() - 1];
            let mut decoded = String::new();
            let mut i = 0;
            while i < inner.len() {
                let c = inner[i];
                if super::is_powershell_single_quote(c) {
                    proptest::prop_assert!(i + 1 < inner.len() && inner[i + 1] == c);
                    i += 2;
                } else {
                    i += 1;
                }
                decoded.push(c);
            }
            proptest::prop_assert_eq!(decoded, s);
        }
    }

    #[test]
    fn resume_command_quotes_both_interpolations() {
        let cmd = resume_command("/Users/me/dev/multi-word-dir", "0aef-1234");
        assert_eq!(
            cmd,
            "cd '/Users/me/dev/multi-word-dir' && claude -r '0aef-1234'"
        );
        // Hostile on-disk values stay one shell argument each.
        let cmd = resume_command("/tmp; rm -rf /", "x'; echo pwned");
        assert!(cmd.starts_with("cd '/tmp; rm -rf /' && claude -r 'x'"));
    }

    #[test]
    fn shell_quote_cwd_expands_tilde_slash() {
        assert_eq!(
            shell_quote_cwd_with_home("~/dev/myproject", Some("/Users/tester")),
            "'/Users/tester/dev/myproject'"
        );
    }

    #[test]
    fn shell_quote_cwd_expands_bare_tilde() {
        assert_eq!(
            shell_quote_cwd_with_home("~", Some("/Users/tester")),
            "'/Users/tester'"
        );
    }

    #[test]
    fn shell_quote_cwd_passes_absolute_paths_through() {
        assert_eq!(
            shell_quote_cwd_with_home("/var/log", Some("/Users/tester")),
            "'/var/log'"
        );
    }

    #[test]
    fn shell_quote_cwd_without_home_keeps_tilde() {
        // No $HOME — keep the literal `~/...` so the user at least sees a
        // pasteable command (broken, but visible) rather than a panic.
        assert_eq!(shell_quote_cwd_with_home("~/dev/foo", None), "'~/dev/foo'");
    }

    #[test]
    fn quotes_plain_path() {
        assert_eq!(posix_shell_quote("/Users/foo/bar"), "'/Users/foo/bar'");
    }

    #[test]
    fn quotes_path_with_space() {
        assert_eq!(posix_shell_quote("/My Docs/x"), "'/My Docs/x'");
    }

    #[test]
    fn neutralises_command_injection_attempts() {
        let payload = "/tmp; rm -rf /";
        let q = posix_shell_quote(payload);
        // The dangerous semicolon is now inside single quotes — `cd` will
        // simply receive the whole string as a (failing) directory name.
        assert_eq!(q, "'/tmp; rm -rf /'");
        assert!(q.starts_with('\'') && q.ends_with('\''));
    }

    #[test]
    fn escapes_embedded_single_quote() {
        // The classic escape: `'\''` ends the current quoted region,
        // inserts a literal `'`, and reopens.
        assert_eq!(posix_shell_quote("a'b"), "'a'\\''b'");
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(1024))]

        /// For ANY arbitrary input, the quoted form must be one shell
        /// argument — never a chain of commands. Test by simulating the
        /// shell-tokenisation rules: after our quoting, the only `'` chars
        /// should be the wrapping pair plus `'\''` patches; no unescaped
        /// `;` `|` `&` `$` `(` `>` `<` etc. should appear OUTSIDE quotes.
        #[test]
        fn quote_never_escapes_quoting_context(s in ".*") {
            let quoted = super::posix_shell_quote(&s);
            // Property: stripping the wrapping single quotes and the
            // documented `'\''` patches leaves a string that contains no
            // single quotes — i.e., every `'` in `quoted` is accounted for
            // by the wrapping scheme.
            proptest::prop_assert!(quoted.starts_with('\''));
            proptest::prop_assert!(quoted.ends_with('\''));
            // Walk the quoted string and check the only structural `'`s
            // are: opening quote (idx 0), closing quote (last idx), and
            // `'\''` triples in between.
            let bytes = quoted.as_bytes();
            let mut i = 1;
            let end = bytes.len() - 1;
            while i < end {
                if bytes[i] == b'\'' {
                    // Must be the start of `'\''` (3 bytes: ', \, ', ')
                    // — actually 4 bytes total. Verify the next 3.
                    proptest::prop_assert!(
                        i + 3 < bytes.len()
                            && bytes[i + 1] == b'\\'
                            && bytes[i + 2] == b'\''
                            && bytes[i + 3] == b'\'',
                        "found bare quote at index {i} in {quoted:?}"
                    );
                    i += 4;
                } else {
                    i += 1;
                }
            }
        }

        /// A POSIX-shell `printf %s` of the quoted form should reproduce
        /// the original input bytes exactly — i.e., the quote is faithful.
        /// We model `printf %s` here as: strip outer quotes, replace each
        /// `'\''` with a literal `'`, and verify the result equals input.
        #[test]
        fn quote_roundtrips_via_shell_interpretation(s in ".*") {
            let quoted = super::posix_shell_quote(&s);
            // Strip the outer single quotes
            let inner = &quoted[1..quoted.len() - 1];
            // Replace `'\''` (4 chars) with a single `'`
            let decoded = inner.replace("'\\''", "'");
            proptest::prop_assert_eq!(decoded, s);
        }
    }
}
