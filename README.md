# Elysium LSP

Elysium LSP is a custom language server for the [Cronus](https://github.com/elysium-os/cronus) kernel. It is of little use for any other project.

### Arguments

| Flag                    | Description                                                               |
| ----------------------- | ------------------------------------------------------------------------- |
| `--project-root <path>` | Root of the Cronus repository. Required.                                  |
| `--log-level <level>`   | Tracing level (e.g. `info`, `debug`).                                     |
| `--plugin <name>`       | Repeatable flag selecting which plugins to load. Defaults to all plugins. |

## Plugins

Plugins live in `src/plugins`. Each plugin implements the `LspPlugin` trait. To enable or disable plugins from the CLI, pass one or more `--plugin` flags.

To add a new plugin, create a module under `src/plugins`, implement the trait, and register it in `PluginChoice`/`instantiate_plugins` in `main.rs`.
