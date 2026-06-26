//! TUI command palette — `:` commands for quick actions.

#[derive(Debug, PartialEq, Clone)]
pub enum Command {
    Quit,
    Clear,
    Pause,
    Resume,
    Filter(String),
    Unfilter,
    Theme(String),
    CopyCurl,
    View(String),
    Replay,
    Help,
    Unknown(String),
}

/// Parse a colon-command string (without the leading `:`).
pub fn parse_command(input: &str) -> Command {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Command::Help;
    }

    let (cmd, args) = split_first(trimmed);
    let cmd = cmd.to_ascii_lowercase();
    let cmd: &str = &cmd;

    match cmd {
        "q" | "quit" => Command::Quit,
        "c" | "clear" => Command::Clear,
        "p" | "pause" => Command::Pause,
        "r" | "resume" => Command::Resume,
        "f" | "filter" => {
            if args.is_empty() {
                Command::Unfilter
            } else {
                Command::Filter(args.to_string())
            }
        }
        "uf" | "unfilter" => Command::Unfilter,
        "t" | "theme" => {
            if args.is_empty() {
                Command::Unknown("theme: missing theme name".into())
            } else {
                Command::Theme(args.to_string())
            }
        }
        "cp" | "copy" => Command::CopyCurl,
        "rr" | "replay" => Command::Replay,
        "v" | "view" => {
            if args.is_empty() {
                Command::View("auto".into())
            } else {
                Command::View(args.to_string())
            }
        }
        "h" | "help" | "?" => Command::Help,
        other => Command::Unknown(format!("unknown command: '{other}'")),
    }
}

/// Split input into command word and remaining args.
fn split_first(input: &str) -> (&str, &str) {
    if let Some(idx) = input.find(char::is_whitespace) {
        (&input[..idx], input[idx..].trim_start())
    } else {
        (input, "")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_quit_variants() {
        assert_eq!(parse_command("q"), Command::Quit);
        assert_eq!(parse_command("quit"), Command::Quit);
    }

    #[test]
    fn parse_clear() {
        assert_eq!(parse_command("clear"), Command::Clear);
        assert_eq!(parse_command("c"), Command::Clear);
    }

    #[test]
    fn parse_pause_resume() {
        assert_eq!(parse_command("pause"), Command::Pause);
        assert_eq!(parse_command("p"), Command::Pause);
        assert_eq!(parse_command("resume"), Command::Resume);
        assert_eq!(parse_command("r"), Command::Resume);
    }

    #[test]
    fn parse_filter() {
        assert_eq!(parse_command("f"), Command::Unfilter);
        assert_eq!(parse_command("filter"), Command::Unfilter);
        assert_eq!(
            parse_command("filter host:api.example"),
            Command::Filter("host:api.example".into())
        );
        assert_eq!(
            parse_command("f method:POST"),
            Command::Filter("method:POST".into())
        );
        assert_eq!(parse_command("unfilter"), Command::Unfilter);
        assert_eq!(parse_command("uf"), Command::Unfilter);
    }

    #[test]
    fn parse_theme() {
        assert_eq!(parse_command("theme slate"), Command::Theme("slate".into()));
        assert_eq!(parse_command("t relay"), Command::Theme("relay".into()));
    }

    #[test]
    fn parse_copy() {
        assert_eq!(parse_command("copy"), Command::CopyCurl);
        assert_eq!(parse_command("cp"), Command::CopyCurl);
    }

    #[test]
    fn parse_view() {
        assert_eq!(parse_command("view"), Command::View("auto".into()));
        assert_eq!(parse_command("v"), Command::View("auto".into()));
        assert_eq!(parse_command("v raw"), Command::View("raw".into()));
        assert_eq!(parse_command("view pretty"), Command::View("pretty".into()));
    }

    #[test]
    fn parse_help() {
        assert_eq!(parse_command("?"), Command::Help);
        assert_eq!(parse_command("help"), Command::Help);
        assert_eq!(parse_command("h"), Command::Help);
    }

    #[test]
    fn parse_unknown() {
        assert!(matches!(
            parse_command("nopenopenope"),
            Command::Unknown(m) if m.contains("nopenopenope")
        ));
    }

    #[test]
    fn parse_case_insensitive() {
        assert_eq!(parse_command("QUIT"), Command::Quit);
        assert_eq!(parse_command("CLEAR"), Command::Clear);
    }

    #[test]
    fn parse_extra_whitespace() {
        assert_eq!(parse_command("  q  "), Command::Quit);
        assert_eq!(
            parse_command(" filter  host:x  "),
            Command::Filter("host:x".into())
        );
    }

    #[test]
    fn parse_missing_args() {
        assert_eq!(
            parse_command("theme"),
            Command::Unknown("theme: missing theme name".into())
        );
    }
}
