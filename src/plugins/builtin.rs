//! 内置插件：随程序发布，启动时写到插件目录，默认启用，只能启用或停用，不能删除。

use std::path::Path;

/// 内置插件：id 与随程序发布的文件
pub(crate) struct Builtin {
    pub id: &'static str,
    pub files: &'static [(&'static str, &'static str)],
}

pub(crate) const BUILTIN: &[Builtin] = &[Builtin {
    id: "group-control",
    files: &[
        ("plugin.json", include_str!("../builtin/group-control/plugin.json")),
        ("main.lua", include_str!("../builtin/group-control/main.lua")),
    ],
}];

pub(crate) fn is_builtin(id: &str) -> bool {
    BUILTIN.iter().any(|b| b.id == id)
}

/// 把内置插件写到 `root/<id>/`：缺失或内容与程序自带的不同就覆盖
pub(crate) fn install(root: &Path) {
    for b in BUILTIN {
        let dir = root.join(b.id);
        if std::fs::create_dir_all(&dir).is_err() {
            continue;
        }
        for (name, content) in b.files {
            let path = dir.join(name);
            if std::fs::read(&path).ok().as_deref() != Some(content.as_bytes()) {
                if let Err(e) = std::fs::write(&path, content) {
                    tracing::warn!("builtin plugin {}: cannot write {}: {e}", b.id, path.display());
                }
            }
        }
    }
}
