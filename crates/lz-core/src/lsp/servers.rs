//! Built-in language-server table (PATH-detected only in v1).

use std::path::{Path, PathBuf};

use serde_json::Value;

pub struct ServerDef {
    pub id: &'static str,
    pub name: &'static str,
    pub command: &'static [&'static str],
    pub extensions: &'static [&'static str],
    /// Files/dirs that mark a project root, nearest wins.
    pub root_markers: &'static [&'static str],
    pub initialization: Option<Value>,
}

impl ServerDef {
    pub fn language_id(&self, path: &Path) -> &'static str {
        match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
            "rs" => "rust",
            "ts" | "mts" | "cts" => "typescript",
            "tsx" => "typescriptreact",
            "js" | "mjs" | "cjs" => "javascript",
            "jsx" => "javascriptreact",
            "go" => "go",
            "py" | "pyi" => "python",
            "c" | "h" => "c",
            "cc" | "cpp" | "cxx" | "hpp" | "hh" => "cpp",
            "zig" => "zig",
            "lua" => "lua",
            "sh" | "bash" | "zsh" => "shellscript",
            "json" => "json",
            "yaml" | "yml" => "yaml",
            "toml" => "toml",
            "rb" => "ruby",
            "java" => "java",
            "kt" => "kotlin",
            "swift" => "swift",
            "cs" => "csharp",
            "ex" | "exs" => "elixir",
            "dart" => "dart",
            _ => "plaintext",
        }
    }

    pub fn find_root(&self, file: &Path, worktree: &Path) -> Option<PathBuf> {
        let mut dir = file.parent()?;
        loop {
            if self.root_markers.iter().any(|m| dir.join(m).exists()) {
                return Some(dir.to_path_buf());
            }
            if dir == worktree || dir.parent().is_none() {
                break;
            }
            dir = dir.parent()?;
        }
        Some(worktree.to_path_buf())
    }
}

pub static SERVERS: &[ServerDef] = &[
    ServerDef {
        id: "rust",
        name: "rust-analyzer",
        command: &["rust-analyzer"],
        extensions: &["rs"],
        root_markers: &["Cargo.toml"],
        initialization: None,
    },
    ServerDef {
        id: "typescript",
        name: "typescript-language-server",
        command: &["typescript-language-server", "--stdio"],
        extensions: &["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"],
        root_markers: &["tsconfig.json", "jsconfig.json", "package.json"],
        initialization: None,
    },
    ServerDef {
        id: "gopls",
        name: "gopls",
        command: &["gopls"],
        extensions: &["go"],
        root_markers: &["go.mod", "go.work"],
        initialization: None,
    },
    ServerDef {
        id: "pyright",
        name: "pyright",
        command: &["pyright-langserver", "--stdio"],
        extensions: &["py", "pyi"],
        root_markers: &[
            "pyproject.toml",
            "setup.py",
            "setup.cfg",
            "requirements.txt",
            "Pipfile",
            "pyrightconfig.json",
        ],
        initialization: None,
    },
    ServerDef {
        id: "clangd",
        name: "clangd",
        command: &["clangd"],
        extensions: &["c", "cpp", "cc", "cxx", "h", "hpp", "hh"],
        root_markers: &[
            "compile_commands.json",
            "compile_flags.txt",
            "CMakeLists.txt",
            ".clangd",
        ],
        initialization: None,
    },
    ServerDef {
        id: "zls",
        name: "zls",
        command: &["zls"],
        extensions: &["zig"],
        root_markers: &["build.zig"],
        initialization: None,
    },
    ServerDef {
        id: "lua-ls",
        name: "lua-language-server",
        command: &["lua-language-server"],
        extensions: &["lua"],
        root_markers: &[".luarc.json", ".luarc.jsonc"],
        initialization: None,
    },
    ServerDef {
        id: "bash",
        name: "bash-language-server",
        command: &["bash-language-server", "start"],
        extensions: &["sh", "bash", "zsh"],
        root_markers: &[],
        initialization: None,
    },
    ServerDef {
        id: "ruby-lsp",
        name: "ruby-lsp",
        command: &["ruby-lsp"],
        extensions: &["rb"],
        root_markers: &["Gemfile"],
        initialization: None,
    },
    ServerDef {
        id: "sourcekit-lsp",
        name: "sourcekit-lsp",
        command: &["sourcekit-lsp"],
        extensions: &["swift"],
        root_markers: &["Package.swift"],
        initialization: None,
    },
    ServerDef {
        id: "jdtls",
        name: "jdtls",
        command: &["jdtls"],
        extensions: &["java"],
        root_markers: &["pom.xml", "build.gradle", "build.gradle.kts"],
        initialization: None,
    },
    ServerDef {
        id: "elixir-ls",
        name: "elixir-ls",
        command: &["elixir-ls"],
        extensions: &["ex", "exs"],
        root_markers: &["mix.exs"],
        initialization: None,
    },
    ServerDef {
        id: "dart",
        name: "dart",
        command: &["dart", "language-server", "--protocol=lsp"],
        extensions: &["dart"],
        root_markers: &["pubspec.yaml"],
        initialization: None,
    },
    ServerDef {
        id: "yaml-ls",
        name: "yaml-language-server",
        command: &["yaml-language-server", "--stdio"],
        extensions: &["yaml", "yml"],
        root_markers: &[],
        initialization: None,
    },
];

pub fn for_path(path: &Path) -> Option<&'static ServerDef> {
    let ext = path.extension()?.to_str()?;
    SERVERS.iter().find(|s| s.extensions.contains(&ext))
}

pub fn on_path(bin: &str) -> bool {
    if Path::new(bin).is_absolute() {
        return Path::new(bin).exists();
    }
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}
