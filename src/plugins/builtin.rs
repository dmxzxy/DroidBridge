//! 内置插件：随程序发布，启动时写到插件目录，只能启用或停用，不能删除。

use std::path::Path;

/// 内置插件：id、第一次出现时是否启用、随程序发布的文件
pub(crate) struct Builtin {
    pub id: &'static str,
    /// 第一次出现时是否默认启用（之后以用户的选择为准）
    pub enabled: bool,
    pub files: &'static [(&'static str, &'static str)],
}

macro_rules! files {
    ($dir:literal: $($name:literal),* $(,)?) => {
        &[$(($name, include_str!(concat!("../builtin/", $dir, "/", $name)))),*]
    };
}

pub(crate) const BUILTIN: &[Builtin] = &[
    Builtin { id: "group-control", enabled: true, files: files!("group-control": "plugin.json", "main.lua") },
    // AI 助手需要用户自己装 Pi，默认停用
    Builtin {
        id: "pi-agent",
        enabled: false,
        files: files!("pi-agent":
            "plugin.json", "main.lua", "agent.lua", "config.lua", "i18n.lua", "settings.lua", "tools.lua",
            "pi/db.ts", "pi/system.md", "pi/system.en.md", "pi/knowledge.md", "pi/knowledge.en.md",
            "web/index.html", "web/app.js", "web/app.css", "web/md.js",
        ),
    },
];

pub(crate) fn is_builtin(id: &str) -> bool {
    BUILTIN.iter().any(|b| b.id == id)
}

/// 第一次出现时是否默认启用
pub(crate) fn enabled_by_default(id: &str) -> bool {
    BUILTIN.iter().any(|b| b.id == id && b.enabled)
}

/// 把内置插件写到 `root/<id>/`：缺失或内容与程序自带的不同就覆盖。
/// 以前从插件源装过同名插件的，去掉插件源标记，之后由程序自带的版本接管（设置和数据保留）。
pub(crate) fn install(root: &Path) {
    for b in BUILTIN {
        let dir = root.join(b.id);
        for (name, content) in b.files {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                if std::fs::create_dir_all(parent).is_err() {
                    continue;
                }
            }
            if std::fs::read(&path).ok().as_deref() != Some(content.as_bytes()) {
                if let Err(e) = std::fs::write(&path, content) {
                    tracing::warn!("builtin plugin {}: cannot write {}: {e}", b.id, path.display());
                }
            }
        }
        let _ = std::fs::remove_file(dir.join(".source.json"));
    }
}
