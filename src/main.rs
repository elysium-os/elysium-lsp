use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, ValueEnum};
use tokio::io::{stdin, stdout};
use tower_lsp::{LspService, Server};
use tracing_subscriber::EnvFilter;

use crate::{
    lsp::ElysiumLsp,
    plugins::{HookPlugin, InitDependencyPlugin, LspPlugin},
};

mod compile_commands;
mod lsp;
mod plugins;

#[derive(Clone, Debug, ValueEnum)]
#[value(rename_all = "kebab_case")]
enum PluginChoice {
    InitDeps,
    Hooks,
}

#[derive(Parser, Debug)]
#[command(author, version, about = "Elysium LSP")]
struct Args {
    /// Cronus repository root (required)
    #[arg(long)]
    project_root: PathBuf,

    /// Set tracing log level (e.g. info, debug)
    #[arg(long)]
    log_level: Option<String>,

    /// Plugins to enable (repeatable)
    #[arg(
        long = "plugin",
        value_enum,
        default_values_t = [PluginChoice::InitDeps, PluginChoice::Hooks]
    )]
    plugins: Vec<PluginChoice>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let level = args.log_level.unwrap_or_else(|| "info".into());
    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .init();

    let (service, socket) = {
        let project_root = args.project_root.canonicalize()?;
        LspService::new(move |client| {
            let plugins = instantiate_plugins(&args.plugins, project_root.as_path())
                .expect("failed to initialize plugins");

            ElysiumLsp::new(client, project_root.clone(), plugins)
        })
    };
    Server::new(stdin(), stdout(), socket).serve(service).await;

    Ok(())
}

impl PluginChoice {
    fn instantiate(&self, project_root: &Path) -> Result<Box<dyn LspPlugin>> {
        match self {
            PluginChoice::InitDeps => Ok(Box::new(InitDependencyPlugin::new(project_root)?)),
            PluginChoice::Hooks => Ok(Box::new(HookPlugin::new(project_root)?)),
        }
    }
}

fn instantiate_plugins(
    selections: &[PluginChoice],
    project_root: &Path,
) -> Result<Vec<Box<dyn LspPlugin>>> {
    let mut plugins: Vec<Box<dyn LspPlugin>> = Vec::new();
    for selection in selections {
        plugins.push(selection.instantiate(project_root)?);
    }
    Ok(plugins)
}
