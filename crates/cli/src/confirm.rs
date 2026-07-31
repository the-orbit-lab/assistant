use std::io::{IsTerminal, Write};
use std::sync::Arc;

use orbit_core::{AlwaysAllow, AlwaysDeny, ConfirmationProvider, ConfirmationRequest};

struct InteractivePrompt;

#[async_trait::async_trait]
impl ConfirmationProvider for InteractivePrompt {
    async fn confirm(&self, request: &ConfirmationRequest) -> bool {
        // Blocking on stdin is the intended behavior here: this is a
        // one-user interactive prompt, and nothing else should proceed
        // until the user answers.
        eprintln!("\nAction `{}` requires confirmation.", request.action);
        if let Some(project) = &request.project {
            eprintln!("  Project:   {project}");
        }
        eprintln!("  Action:    {}", request.description);
        eprintln!("  Arguments: {}", request.arguments_summary);
        eprint!("Allow? [y/N] ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return false;
        }
        matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
    }
}

/// `--yes` always approves. Otherwise: prompt interactively when stdin is
/// a TTY, and deny by default when it isn't — an `ask` action never
/// silently runs in a non-interactive context (a script, a CI job) without
/// explicit approval.
pub fn build_confirmation_provider(yes: bool) -> Arc<dyn ConfirmationProvider> {
    if yes {
        return Arc::new(AlwaysAllow);
    }
    if std::io::stdin().is_terminal() {
        Arc::new(InteractivePrompt)
    } else {
        Arc::new(AlwaysDeny)
    }
}
