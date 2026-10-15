//! 文件类型只按路径识别，不读取文件；图标复用已有的 GPUI Kit 资源。

use std::path::Path;

use gpui_kit::assets::IconName;

pub(crate) fn icon_for(path: &str) -> IconName {
    let path = Path::new(path);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    match name {
        "Cargo.toml" | "package.json" | "pyproject.toml" | "go.mod" => return IconName::Package,
        "Cargo.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "poetry.lock" => {
            return IconName::FileLock;
        }
        "Dockerfile" | "Containerfile" => return IconName::Container,
        "Makefile" | "Justfile" => return IconName::Terminal,
        ".gitignore" | ".gitattributes" | ".gitmodules" => return IconName::GitBranch,
        "README" | "LICENSE" | "CHANGELOG" => return IconName::FileText,
        _ => {}
    }
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "rs" | "py" | "js" | "jsx" | "ts" | "tsx" | "go" | "c" | "h" | "cpp" | "hpp" | "cc"
        | "java" | "kt" | "swift" | "rb" | "php" | "html" | "css" | "scss" | "vue" | "svelte" => {
            IconName::FileCode
        }
        "json" | "jsonc" | "json5" => IconName::FileBraces,
        "toml" | "yaml" | "yml" | "ini" | "conf" | "config" | "env" => IconName::FileCog,
        "md" | "mdx" | "txt" | "rst" | "log" => IconName::FileText,
        "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" => IconName::Terminal,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" => IconName::FileImage,
        "zip" | "gz" | "tar" | "xz" | "7z" => IconName::FileArchive,
        "lock" => IconName::FileLock,
        _ => IconName::File,
    }
}
