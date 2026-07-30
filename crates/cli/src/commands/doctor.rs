use std::sync::Arc;

use orbit_actions::native::information;
use orbit_core::{ActionInput, AlwaysDeny, OrbitError};
use orbit_mcp_server::OrbitMcpServer;
use orbit_providers::OllamaProvider;
use orbit_workspace::DiscoveredRoot;
use serde_json::json;

use crate::args::GlobalArgs;
use crate::output::print_json;
use crate::resolve::{resolve_project, resolve_workspace};
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
    name: String,
    status: Status,
    detail: String,
}

fn check(name: impl Into<String>, status: Status, detail: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        status,
        detail: detail.into(),
    }
}

pub async fn run(global: &GlobalArgs) -> Result<(), OrbitError> {
    let use_workspace = global.workspace.is_some()
        || (global.project.is_none() && global.config.is_none() && is_workspace_root()?);

    let checks = if use_workspace {
        run_workspace_checks(global).await
    } else {
        run_single_project_checks(global).await
    };

    let failed = report(global, &checks);
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

fn is_workspace_root() -> Result<bool, OrbitError> {
    let cwd = std::env::current_dir().map_err(|e| OrbitError::io(".", e))?;
    Ok(matches!(
        orbit_workspace::discover(&cwd),
        Ok(DiscoveredRoot::Workspace(_))
    ))
}

async fn run_single_project_checks(global: &GlobalArgs) -> Vec<Check> {
    let mut checks = Vec::new();

    let loaded = match resolve_project(global) {
        Ok(loaded) => loaded,
        Err(err) => {
            checks.push(check(
                "project configuration",
                Status::Fail,
                err.to_string(),
            ));
            return checks;
        }
    };

    checks.push(check(
        "project configuration",
        Status::Ok,
        format!("valid at {}", loaded.paths.config_path.display()),
    ));
    checks.push(check(
        "project root",
        Status::Ok,
        loaded.paths.root.display().to_string(),
    ));

    let permissions_configured = !loaded.config.permissions.is_empty();
    checks.push(check(
        "permission configuration",
        Status::Ok,
        if permissions_configured {
            format!("{} explicit entries", loaded.config.permissions.len())
        } else {
            "using action defaults (no explicit entries)".to_string()
        },
    ));

    let registry = match orbit_actions::native_registry() {
        Ok(registry) => registry,
        Err(err) => {
            checks.push(check("action registry", Status::Fail, err.to_string()));
            return checks;
        }
    };

    if loaded.config.mcp.expose.is_empty() {
        checks.push(check(
            "mcp export configuration",
            Status::Warn,
            "mcp.expose is empty; `orbit mcp serve` will expose nothing",
        ));
    } else {
        let exposure = orbit_mcp_server::compute_exposure(
            &registry,
            &loaded.config.mcp.expose,
            &loaded.config,
        );
        if exposure.warnings.is_empty() {
            checks.push(check(
                "mcp export configuration",
                Status::Ok,
                format!("{} action(s) exposed cleanly", exposure.listable.len()),
            ));
        } else {
            for warning in &exposure.warnings {
                checks.push(check("mcp exposure", Status::Warn, warning.message.clone()));
            }
        }
    }

    let model = loaded.config.model.model.clone();
    let endpoint = loaded.config.model.endpoint.clone();
    let expose = loaded.config.mcp.expose.clone();
    let ctx = build_context(loaded);

    let (_, result) = registry
        .execute(&ctx, information::NAME, ActionInput::empty(), &AlwaysDeny)
        .await;
    match result {
        Ok(output) => checks.push(check(
            "file discovery",
            Status::Ok,
            format!(
                "{} file(s) discovered",
                output.data["discovered_file_count"]
                    .as_u64()
                    .unwrap_or_default()
            ),
        )),
        Err(err) => checks.push(check("file discovery", Status::Fail, err.to_string())),
    }

    let mcp_server = OrbitMcpServer::new(registry, ctx, expose);
    match orbit_mcp_server::self_check(mcp_server).await {
        Ok(report) => checks.push(check(
            "mcp server initialization",
            Status::Ok,
            format!(
                "initialized over an in-process transport; {} tool(s) listed",
                report.tool_count
            ),
        )),
        Err(err) => checks.push(check(
            "mcp server initialization",
            Status::Fail,
            err.to_string(),
        )),
    }
    checks.push(check(
        "mcp stdout reservation",
        Status::Ok,
        "orbit mcp serve writes protocol frames to stdout only; diagnostics use stderr \
         (see --verbose) -- enforced by the protocol integration tests",
    ));

    checks.push(ollama_check(&endpoint, &model).await);
    if let Some(model_check) = ollama_model_check(&endpoint, &model).await {
        checks.push(model_check);
    }

    checks
}

async fn ollama_check(endpoint: &str, _model: &str) -> Check {
    let provider = OllamaProvider::new(endpoint, "unused");
    match provider.check_connectivity().await {
        Ok(()) => check(
            "ollama connectivity",
            Status::Ok,
            format!("reachable at {endpoint}"),
        ),
        Err(err) => check(
            "ollama connectivity",
            Status::Warn,
            format!("could not reach {endpoint}: {err}"),
        ),
    }
}

/// `None` when Ollama itself is unreachable (already reported by
/// `ollama_check`); otherwise the model-availability result for one
/// specific model.
async fn ollama_model_check(endpoint: &str, model: &str) -> Option<Check> {
    let provider = OllamaProvider::new(endpoint, model);
    if provider.check_connectivity().await.is_err() {
        return None;
    }
    Some(match provider.model_is_available().await {
        Ok(true) => check(
            "ollama model",
            Status::Ok,
            format!("`{model}` is available"),
        ),
        Ok(false) => check(
            "ollama model",
            Status::Warn,
            format!(
                "`{model}` is not pulled yet. Run `{}`.",
                provider.pull_command()
            ),
        ),
        Err(err) => check("ollama model", Status::Warn, err.to_string()),
    })
}

async fn run_workspace_checks(global: &GlobalArgs) -> Vec<Check> {
    let mut checks = Vec::new();

    let project_registry = match resolve_workspace(global) {
        Ok(registry) => registry,
        Err(err) => {
            checks.push(check(
                "workspace configuration",
                Status::Fail,
                err.to_string(),
            ));
            return checks;
        }
    };

    checks.push(check(
        "workspace configuration",
        Status::Ok,
        format!(
            "valid at {}",
            project_registry.workspace_config_path.display()
        ),
    ));
    checks.push(check(
        "workspace root",
        Status::Ok,
        project_registry.workspace_root.display().to_string(),
    ));

    match project_registry.config.defaults.project.as_deref() {
        None => checks.push(check(
            "default project",
            Status::Warn,
            "no defaults.project configured; some read-only commands at the workspace root \
             will require an explicit --project",
        )),
        Some(name) => match project_registry.get_project(name) {
            Some(entry) if entry.available => {
                checks.push(check("default project", Status::Ok, name.to_string()))
            }
            Some(entry) => checks.push(check(
                "default project",
                Status::Warn,
                format!(
                    "`{name}` is registered but unavailable: {}",
                    entry.error.clone().unwrap_or_default()
                ),
            )),
            None => checks.push(check(
                "default project",
                Status::Fail,
                format!("`{name}` is not a registered project"),
            )),
        },
    }

    checks.push(check(
        "relationships",
        Status::Ok,
        format!("{} configured", project_registry.relationships().len()),
    ));

    for project in project_registry.list_projects() {
        checks.extend(project_checks(project).await);
    }

    let confirmation: Arc<dyn orbit_core::ConfirmationProvider> = Arc::new(AlwaysDeny);
    match orbit_workspace::build_registry(project_registry.clone(), confirmation) {
        Ok(registry) => {
            let tool_count = registry.descriptors().len();
            checks.push(check(
                "workspace action registry",
                Status::Ok,
                format!("{tool_count} workspace.* action(s) registered"),
            ));

            let expose: Vec<String> = registry.descriptors().into_iter().map(|d| d.name).collect();
            checks.push(check(
                "workspace mcp exposure",
                Status::Ok,
                format!("{} action(s) exposed: {}", expose.len(), expose.join(", ")),
            ));

            let ctx = project_registry.workspace_action_context();
            let server = OrbitMcpServer::new(registry, ctx, expose);
            match orbit_mcp_server::self_check(server).await {
                Ok(report) => checks.push(check(
                    "workspace mcp self-check",
                    Status::Ok,
                    format!("initialized; {} tool(s) listed", report.tool_count),
                )),
                Err(err) => checks.push(check(
                    "workspace mcp self-check",
                    Status::Fail,
                    err.to_string(),
                )),
            }
        }
        Err(err) => checks.push(check(
            "workspace action registry",
            Status::Fail,
            err.to_string(),
        )),
    }

    let endpoint = global
        .ollama_endpoint
        .clone()
        .unwrap_or_else(|| orbit_project::config::DEFAULT_OLLAMA_ENDPOINT.to_string());
    let model = global
        .model
        .clone()
        .unwrap_or_else(|| orbit_project::config::DEFAULT_OLLAMA_MODEL.to_string());
    checks.push(ollama_check(&endpoint, &model).await);
    if let Some(model_check) = ollama_model_check(&endpoint, &model).await {
        checks.push(model_check);
    }

    checks
}

async fn project_checks(project: &orbit_workspace::ProjectEntry) -> Vec<Check> {
    let name = format!("project {}", project.name);
    if !project.available {
        return vec![check(
            name,
            Status::Fail,
            project
                .error
                .clone()
                .unwrap_or_else(|| "unavailable".to_string()),
        )];
    }

    let mut checks = Vec::new();
    let Some(config) = &project.config else {
        return vec![check(
            name,
            Status::Fail,
            "loaded as available but has no configuration",
        )];
    };

    let ctx = orbit_actions::ActionContext {
        root: project.root.clone(),
        config_path: project.config_path.clone(),
        config: config.clone(),
    };
    let Ok(registry) = orbit_actions::native_registry() else {
        return vec![check(
            name,
            Status::Fail,
            "failed to build the native action registry",
        )];
    };
    let (_, result) = registry
        .execute(&ctx, information::NAME, ActionInput::empty(), &AlwaysDeny)
        .await;
    match result {
        Ok(output) => checks.push(check(
            name,
            Status::Ok,
            format!(
                "{} file(s) discovered, {} explicit permission(s)",
                output.data["discovered_file_count"]
                    .as_u64()
                    .unwrap_or_default(),
                config.permissions.len()
            ),
        )),
        Err(err) => checks.push(check(name, Status::Fail, err.to_string())),
    }

    if !config.model.model.trim().is_empty() {
        let provider =
            OllamaProvider::new(config.model.endpoint.clone(), config.model.model.clone());
        if provider.check_connectivity().await.is_ok() {
            match provider.model_is_available().await {
                Ok(false) => checks.push(check(
                    format!("project {}", project.name),
                    Status::Warn,
                    format!(
                        "model `{}` is unavailable. Run `{}`.",
                        config.model.model,
                        provider.pull_command()
                    ),
                )),
                Ok(true) => {}
                Err(_) => {}
            }
        }
    }

    checks
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
