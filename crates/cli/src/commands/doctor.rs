use orbit_actions::native::information;
use orbit_core::{ActionInput, AlwaysDeny, OrbitError};
use orbit_providers::OllamaProvider;
use serde_json::json;

use crate::args::GlobalArgs;
use crate::output::print_json;
use crate::resolve::resolve_project;
use crate::runtime::build_context;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Ok => "OK",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
        }
    }
}

struct Check {
    name: &'static str,
    status: Status,
    detail: String,
}

pub async fn run(global: &GlobalArgs) -> Result<(), OrbitError> {
    let mut checks = Vec::new();

    let loaded = match resolve_project(global) {
        Ok(loaded) => loaded,
        Err(err) => {
            checks.push(Check {
                name: "project configuration",
                status: Status::Fail,
                detail: err.to_string(),
            });
            report(global, &checks);
            std::process::exit(1);
        }
    };

    checks.push(Check {
        name: "project configuration",
        status: Status::Ok,
        detail: format!("valid at {}", loaded.paths.config_path.display()),
    });
    checks.push(Check {
        name: "project root",
        status: Status::Ok,
        detail: loaded.paths.root.display().to_string(),
    });

    let permissions_configured = !loaded.config.permissions.is_empty();
    checks.push(Check {
        name: "permission configuration",
        status: Status::Ok,
        detail: if permissions_configured {
            format!("{} explicit entries", loaded.config.permissions.len())
        } else {
            "using action defaults (no explicit entries)".to_string()
        },
    });

    if loaded.config.mcp.expose.is_empty() {
        checks.push(Check {
            name: "mcp export configuration",
            status: Status::Warn,
            detail: "mcp.expose is empty; `orbit mcp serve` will expose nothing".to_string(),
        });
    } else {
        checks.push(Check {
            name: "mcp export configuration",
            status: Status::Ok,
            detail: format!("{} action(s) exposed", loaded.config.mcp.expose.len()),
        });
    }

    let model = loaded.config.model.model.clone();
    let endpoint = loaded.config.model.endpoint.clone();
    let ctx = build_context(loaded);
    let registry = orbit_actions::native_registry()?;

    let (_, result) = registry
        .execute(&ctx, information::NAME, ActionInput::empty(), &AlwaysDeny)
        .await;
    match result {
        Ok(output) => {
            checks.push(Check {
                name: "file discovery",
                status: Status::Ok,
                detail: format!(
                    "{} file(s) discovered",
                    output.data["discovered_file_count"]
                        .as_u64()
                        .unwrap_or_default()
                ),
            });
        }
        Err(err) => checks.push(Check {
            name: "file discovery",
            status: Status::Fail,
            detail: err.to_string(),
        }),
    }

    let provider = OllamaProvider::new(endpoint.clone(), model.clone());
    match provider.check_connectivity().await {
        Ok(()) => {
            checks.push(Check {
                name: "ollama connectivity",
                status: Status::Ok,
                detail: format!("reachable at {endpoint}"),
            });
            match provider.model_is_available().await {
                Ok(true) => checks.push(Check {
                    name: "ollama model",
                    status: Status::Ok,
                    detail: format!("`{model}` is available"),
                }),
                Ok(false) => checks.push(Check {
                    name: "ollama model",
                    status: Status::Warn,
                    detail: format!(
                        "`{model}` is not pulled yet. Run `{}`.",
                        provider.pull_command()
                    ),
                }),
                Err(err) => checks.push(Check {
                    name: "ollama model",
                    status: Status::Warn,
                    detail: err.to_string(),
                }),
            }
        }
        Err(err) => checks.push(Check {
            name: "ollama connectivity",
            status: Status::Warn,
            detail: format!("could not reach {endpoint}: {err}"),
        }),
    }

    let failed = report(global, &checks);
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

fn report(global: &GlobalArgs, checks: &[Check]) -> bool {
    if global.json {
        let entries: Vec<_> = checks
            .iter()
            .map(|c| json!({ "check": c.name, "status": c.status.label(), "detail": c.detail }))
            .collect();
        print_json(&json!({ "checks": entries }));
    } else {
        for check in checks {
            println!(
                "[{}] {}: {}",
                check.status.label(),
                check.name,
                check.detail
            );
        }
    }
    checks.iter().any(|c| c.status == Status::Fail)
}
