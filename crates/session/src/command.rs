//! Session commands (`/use`, `/cancel`, ...).
//!
//! Parsed here, in the session layer, rather than in the CLI renderer, for
//! two reasons: every front end gets the same command vocabulary, and — the
//! important one — **a command is never sent to the language model**. A
//! line beginning with `/` is interpreted by application code and either
//! resolves to a [`SessionCommand`] or is reported as an unknown command;
//! it never becomes a user message the model could act on or be confused
//! by.

/// A recognized in-session command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCommand {
    /// List every project registered in the active workspace.
    Projects,
    /// Replace the active project set. Never empty.
    Use(Vec<String>),
    /// Show session identity, mode, active projects, and counters.
    Status,
    /// Re-print the sources collected so far in this session.
    Sources,
    /// Cancel the turn currently running.
    Cancel,
    /// Forget the conversation, keeping the session and its project
    /// selection.
    Clear,
    /// End the session.
    Exit,
    /// List the available commands.
    Help,
}

/// The result of interpreting one line of session input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedInput {
    /// Ordinary text, to be sent to the model as a user message.
    Message(String),
    Command(SessionCommand),
    /// Looked like a command but is not one. Reported back to the user;
    /// never forwarded to the model, because silently treating `/usse obc`
    /// as prose would quietly ignore what the user actually asked for.
    UnknownCommand(String),
    /// Nothing but whitespace.
    Empty,
}

/// One line of help per command, in the order they are listed to users.
pub const COMMAND_HELP: &[(&str, &str)] = &[
    (
        "/projects",
        "list the projects registered in this workspace",
    ),
    ("/use <a[,b]>", "set the active project(s)"),
    (
        "/status",
        "show session id, mode, active projects, counters",
    ),
    ("/sources", "re-print the sources collected in this session"),
    ("/cancel", "cancel the turn currently running"),
    ("/clear", "forget the conversation, keep the session"),
    ("/help", "show this list"),
    ("/exit", "end the session"),
];

/// Interpret one line of input.
///
/// `exit`/`quit` without a slash are accepted too, because they are the
/// long-standing way out of `orbit chat` and removing them would break
/// muscle memory for no benefit.
pub fn parse(line: &str) -> ParsedInput {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return ParsedInput::Empty;
    }

    if trimmed.eq_ignore_ascii_case("exit") || trimmed.eq_ignore_ascii_case("quit") {
        return ParsedInput::Command(SessionCommand::Exit);
    }

    let Some(rest) = trimmed.strip_prefix('/') else {
        return ParsedInput::Message(trimmed.to_string());
    };

    let mut parts = rest.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or_default().to_lowercase();
    let argument = parts.next().unwrap_or("").trim();

    match name.as_str() {
        "projects" => ParsedInput::Command(SessionCommand::Projects),
        "status" => ParsedInput::Command(SessionCommand::Status),
        "sources" => ParsedInput::Command(SessionCommand::Sources),
        "cancel" => ParsedInput::Command(SessionCommand::Cancel),
        "clear" => ParsedInput::Command(SessionCommand::Clear),
        "exit" | "quit" => ParsedInput::Command(SessionCommand::Exit),
        "help" | "?" => ParsedInput::Command(SessionCommand::Help),
        "use" => {
            let projects: Vec<String> = argument
                .split(',')
                .map(|p| p.trim())
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .collect();
            if projects.is_empty() {
                // `/use` with nothing to switch to is a mistake, not a
                // request to clear the selection.
                ParsedInput::UnknownCommand(
                    "/use needs at least one project, e.g. `/use obc` or `/use docs,obc`"
                        .to_string(),
                )
            } else {
                ParsedInput::Command(SessionCommand::Use(projects))
            }
        }
        other => ParsedInput::UnknownCommand(format!("unknown command `/{other}`. Try /help.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_a_message() {
        assert_eq!(
            parse("Why was STM32 selected?"),
            ParsedInput::Message("Why was STM32 selected?".to_string())
        );
    }

    #[test]
    fn blank_input_is_empty() {
        assert_eq!(parse("   "), ParsedInput::Empty);
        assert_eq!(parse(""), ParsedInput::Empty);
    }

    #[test]
    fn simple_commands_parse() {
        assert_eq!(
            parse("/projects"),
            ParsedInput::Command(SessionCommand::Projects)
        );
        assert_eq!(
            parse("/status"),
            ParsedInput::Command(SessionCommand::Status)
        );
        assert_eq!(
            parse("/sources"),
            ParsedInput::Command(SessionCommand::Sources)
        );
        assert_eq!(
            parse("/cancel"),
            ParsedInput::Command(SessionCommand::Cancel)
        );
        assert_eq!(parse("/clear"), ParsedInput::Command(SessionCommand::Clear));
        assert_eq!(parse("/exit"), ParsedInput::Command(SessionCommand::Exit));
        assert_eq!(parse("/help"), ParsedInput::Command(SessionCommand::Help));
    }

    #[test]
    fn bare_exit_and_quit_still_work() {
        assert_eq!(parse("exit"), ParsedInput::Command(SessionCommand::Exit));
        assert_eq!(parse("QUIT"), ParsedInput::Command(SessionCommand::Exit));
    }

    #[test]
    fn use_accepts_one_or_several_projects() {
        assert_eq!(
            parse("/use obc"),
            ParsedInput::Command(SessionCommand::Use(vec!["obc".to_string()]))
        );
        assert_eq!(
            parse("/use docs,obc"),
            ParsedInput::Command(SessionCommand::Use(vec![
                "docs".to_string(),
                "obc".to_string()
            ]))
        );
        assert_eq!(
            parse("/use  docs , obc "),
            ParsedInput::Command(SessionCommand::Use(vec![
                "docs".to_string(),
                "obc".to_string()
            ]))
        );
    }

    #[test]
    fn use_without_an_argument_is_reported_not_guessed() {
        assert!(matches!(parse("/use"), ParsedInput::UnknownCommand(_)));
        assert!(matches!(
            parse("/use   ,  ,"),
            ParsedInput::UnknownCommand(_)
        ));
    }

    /// The important property: a mistyped command must never be quietly
    /// forwarded to the model as if it were a question.
    #[test]
    fn an_unknown_command_is_never_treated_as_a_message() {
        match parse("/usse obc") {
            ParsedInput::UnknownCommand(message) => assert!(message.contains("/usse")),
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
    }

    #[test]
    fn commands_are_case_insensitive() {
        assert_eq!(
            parse("/STATUS"),
            ParsedInput::Command(SessionCommand::Status)
        );
    }

    /// A path-like message must not be mistaken for a command.
    #[test]
    fn a_message_that_merely_contains_a_slash_is_still_a_message() {
        assert_eq!(
            parse("what is in docs/obc/architecture.md?"),
            ParsedInput::Message("what is in docs/obc/architecture.md?".to_string())
        );
    }
}
