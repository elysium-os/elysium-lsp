use serde::Deserialize;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
pub struct CompileCommandEntry {
    file: PathBuf,
    arguments: Option<Vec<String>>,
    command: Option<String>,
}

pub struct CompileCommands {
    root: PathBuf,
    entries: HashMap<PathBuf, Vec<String>>,
    default_args: Vec<String>,
}

impl CompileCommandEntry {
    fn into_arguments(self) -> (PathBuf, Vec<String>) {
        let args = if let Some(arguments) = self.arguments {
            arguments.into_iter().skip(1).collect()
        } else if let Some(cmd) = self.command {
            shell_words::split(&cmd)
                .unwrap_or_default()
                .into_iter()
                .skip(1)
                .collect()
        } else {
            Vec::new()
        };

        (self.file, args)
    }
}

impl CompileCommands {
    pub fn load(root: PathBuf, default_args: Vec<String>) -> Self {
        let mut entries = HashMap::new();

        let path = root.join("compile_commands.json");
        if let Ok(contents) = fs::read_to_string(&path) {
            if let Ok(raw_entries) = serde_json::from_str::<Vec<CompileCommandEntry>>(&contents) {
                for entry in raw_entries {
                    let (file, args) = entry.into_arguments();
                    entries.insert(file.canonicalize().unwrap_or(file), args);
                }
            }
        }

        Self {
            root,
            entries,
            default_args,
        }
    }

    pub fn args_for(&self, file: &Path) -> Vec<String> {
        let canonical = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());

        if let Some(args) = self.entries.get(&canonical) {
            return args.clone();
        }

        if let Ok(rel) = canonical.strip_prefix(&self.root) {
            let candidate = self.root.join(rel);
            if let Some(args) = self.entries.get(&candidate) {
                return args.clone();
            }
        }

        self.default_args.clone()
    }
}
