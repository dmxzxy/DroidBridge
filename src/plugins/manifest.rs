//! 插件清单 `plugin.json`：校验、权限、设备匹配（glob）与 MCP 工具名（SPEC 第 1 节）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 设置项允许的类型
const SETTING_TYPES: &[&str] = &["text", "number", "bool", "select", "file", "dir", "password"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub main: String,
    pub permissions: Vec<String>,
    /// 限定插件能看到的设备
    #[serde(rename = "match")]
    pub match_rules: MatchRules,
    pub settings: Vec<Setting>,
    /// 网页面板可以嵌入的外部网站（origin，如 `http://host:5173`），需要 `web.embed` 权限
    pub embed: Vec<String>,
    /// 需要的最低 DroidBridge 版本
    #[serde(rename = "minHost")]
    pub min_host: String,
    /// 英文界面下显示的名称与说明（可省略）
    pub en: Localized,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Localized {
    pub name: String,
    pub description: String,
}

impl Manifest {
    /// 按当前界面语言显示的名称
    pub fn display_name(&self) -> &str {
        if crate::i18n::is_en() && !self.en.name.is_empty() { &self.en.name } else { &self.name }
    }

    /// 按当前界面语言显示的说明
    pub fn display_description(&self) -> &str {
        if crate::i18n::is_en() && !self.en.description.is_empty() { &self.en.description } else { &self.description }
    }
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            version: String::new(),
            description: String::new(),
            author: String::new(),
            main: "main.lua".into(),
            permissions: Vec::new(),
            match_rules: MatchRules::default(),
            settings: Vec::new(),
            embed: Vec::new(),
            min_host: String::new(),
            en: Localized::default(),
        }
    }
}

/// 比较版本号：按 `.` 分段比数字，非数字部分忽略，缺少的分段当作 0
pub fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let parts = |v: &str| -> Vec<u64> {
        v.split('.')
            .map(|s| s.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0))
            .collect()
    };
    let (a, b) = (parts(a), parts(b));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x.cmp(&y);
        }
    }
    std::cmp::Ordering::Equal
}

/// 规整成 origin（`scheme://host[:port]`，小写）；不是 http(s) 地址返回 None
pub fn origin_of(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = rest.split(['/', '?', '#']).next()?.to_ascii_lowercase();
    if host.is_empty() || host.contains('@') {
        return None;
    }
    Some(format!("{scheme}://{host}"))
}

impl Manifest {
    /// 这个地址是否在清单声明的可嵌入网站内
    pub fn may_embed(&self, url: &str) -> bool {
        match origin_of(url) {
            Some(o) => self.embed.iter().filter_map(|e| origin_of(e)).any(|e| e == o),
            None => false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MatchRules {
    pub apps: Vec<String>,
    pub models: Vec<String>,
    pub serials: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Setting {
    pub name: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub default: Value,
    pub options: Vec<Value>,
}

/// id 规则：`[a-z0-9][a-z0-9_-]{0,39}`
pub fn valid_id(id: &str) -> bool {
    let mut it = id.chars();
    match it.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    id.chars().count() <= 40 && it.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// 相对路径是否安全：不允许绝对路径、盘符和 `..`
pub fn safe_rel(p: &str) -> bool {
    let p = p.replace('\\', "/");
    if p.trim().is_empty() || p.starts_with('/') {
        return false;
    }
    if p.len() >= 2 && p.as_bytes()[1] == b':' {
        return false;
    }
    !p.split('/').any(|c| c == "..")
}

/// 简单 glob（`*` 任意长度、`?` 单个字符，不区分大小写）
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            pi += 1;
            mark = ti;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// 列表里的任意一项匹配即可；列表为空表示“全部”
pub fn any_glob(pats: &[String], text: &str) -> bool {
    pats.is_empty() || pats.iter().any(|p| p == "*" || glob_match(p, text))
}

/// 设备是否符合插件的 match 规则（apps 由调用方先算好 has_app）
pub fn device_matches(m: &MatchRules, model: &str, serial: &str, has_app: bool) -> bool {
    any_glob(&m.serials, serial) && any_glob(&m.models, model) && (m.apps.is_empty() || has_app)
}

/// 是否已授予某项权限（`fs` 包含 `fs.read`）
pub fn has_perm(granted: &[String], need: &str) -> bool {
    let has = |p: &str| granted.iter().any(|g| g == p);
    has(need) || (need == "fs.read" && has("fs")) || (need == "ai" && has("mcp")) || (need == "mcp" && has("ai"))
}

/// 清单里有 granted 中没有的权限 → 需要用户重新确认（needs_grant）
pub fn needs_grant(perms: &[String], granted: &[String]) -> bool {
    perms.iter().any(|p| !has_perm(granted, p))
}

/// MCP 工具名：`<插件id>__<名称>`，非法字符换成 `_`，截断到 64
pub fn tool_name(plugin: &str, name: &str) -> String {
    let raw = format!("{plugin}__{name}");
    let s: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    s.chars().take(64).collect()
}

/// 校验清单，返回给用户看的错误信息
pub fn validate(m: &Manifest) -> Result<(), String> {
    if !valid_id(&m.id) {
        let id = &m.id;
        return Err(tr!("id 不符合规则（需要 [a-z0-9][a-z0-9_-]{{0,39}}）：{id}", "Invalid id (must match [a-z0-9][a-z0-9_-]{{0,39}}): {id}"));
    }
    if m.name.trim().is_empty() {
        return Err(tr!("缺少 name", "Missing name"));
    }
    if !safe_rel(&m.main) {
        return Err(tr!("main 必须是插件目录内的相对路径", "main must be a relative path inside the plugin folder"));
    }
    let min_host = m.min_host.trim();
    if !min_host.is_empty() && version_cmp(env!("CARGO_PKG_VERSION"), min_host).is_lt() {
        return Err(tr!("需要 DroidBridge {min_host}", "Requires DroidBridge {min_host}"));
    }
    let mut seen = std::collections::HashSet::new();
    for s in &m.settings {
        if s.name.trim().is_empty() {
            return Err(tr!("设置项缺少 name", "A setting is missing its name"));
        }
        if !seen.insert(s.name.as_str()) {
            let n = &s.name;
            return Err(tr!("设置项重复：{n}", "Duplicate setting: {n}"));
        }
        if !SETTING_TYPES.contains(&s.kind.as_str()) {
            let (n, k) = (&s.name, &s.kind);
            return Err(tr!("设置项 {n} 的 type 无效：{k}", "Invalid type for setting {n}: {k}"));
        }
    }
    Ok(())
}

/// 读取并校验目录里的 `plugin.json`
pub fn load(dir: &std::path::Path) -> Result<Manifest, String> {
    let p = dir.join("plugin.json");
    let raw = std::fs::read(&p).map_err(|e| tr!("无法读取 plugin.json：{e}", "Cannot read plugin.json: {e}"))?;
    let m: Manifest = serde_json::from_slice(&raw).map_err(|e| tr!("plugin.json 格式错误：{e}", "Invalid plugin.json: {e}"))?;
    validate(&m)?;
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn man(json: &str) -> Manifest {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn ids() {
        assert!(valid_id("mygame-dev"));
        assert!(valid_id("a"));
        assert!(valid_id("a_b-1"));
        assert!(!valid_id("MyGame"));
        assert!(!valid_id("-x"));
        assert!(!valid_id(""));
        assert!(!valid_id(&"a".repeat(41)));
    }

    #[test]
    fn manifest_defaults_and_errors() {
        let m = man(r#"{"id":"x","name":"X"}"#);
        assert_eq!(m.main, "main.lua");
        assert!(m.permissions.is_empty());
        assert!(validate(&m).is_ok());

        assert!(validate(&man(r#"{"id":"Bad","name":"X"}"#)).is_err());
        assert!(validate(&man(r#"{"id":"x"}"#)).is_err());
        assert!(validate(&man(r#"{"id":"x","name":"X","main":"../x.lua"}"#)).is_err());
        assert!(validate(&man(r#"{"id":"x","name":"X","main":"C:/x.lua"}"#)).is_err());
        assert!(validate(&man(r#"{"id":"x","name":"X","main":"lua/main.lua"}"#)).is_ok());
        // 设置项
        assert!(validate(&man(r#"{"id":"x","name":"X","settings":[{"name":"a","type":"dir"}]}"#)).is_ok());
        assert!(validate(&man(r#"{"id":"x","name":"X","settings":[{"name":"a","type":"nope"}]}"#)).is_err());
        assert!(validate(&man(r#"{"id":"x","name":"X","settings":[{"name":"a"},{"name":"a"}]}"#)).is_err());
    }

    #[test]
    fn globs() {
        assert!(glob_match("Pixel*", "Pixel 7"));
        assert!(glob_match("pixel*", "Pixel 7"));
        assert!(!glob_match("Pixel*", "SM G973F"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("com.my.?ame", "com.my.game"));
        assert!(!glob_match("com.my.?ame", "com.my.gam"));
        assert!(glob_match("a*b*c", "axxbyyc"));
        assert!(glob_match("a*b*c", "abc"));
        assert!(!glob_match("a*b*c", "abd"));
        assert!(glob_match("*.test:5555", "192.168.1.5.test:5555"));
        assert!(any_glob(&[], "x"));
        assert!(any_glob(&["a".into(), "x".into()], "x"));
        assert!(!any_glob(&["a".into()], "x"));
    }

    #[test]
    fn version_order() {
        assert!(version_cmp("0.9.6", "0.9.4").is_gt());
        assert!(version_cmp("0.9.4", "0.9.5").is_lt());
        assert!(version_cmp("0.9.5", "0.9.5").is_eq());
        assert!(version_cmp("0.9", "0.9.0").is_eq());
        assert!(version_cmp("0.10.0", "0.9.9").is_gt());
        assert!(version_cmp("1.4.0+3fa2c1d9", "1.4.0").is_eq());
        assert!(version_cmp("1.2.3-beta", "1.2.3").is_eq());
        assert!(version_cmp("1.0", "").is_gt());
        assert!(version_cmp("", "").is_eq());
    }

    #[test]
    fn min_host_checked() {
        // 不可能达到的版本：拒绝
        assert!(validate(&man(r#"{"id":"x","name":"X","minHost":"999.0"}"#)).is_err());
        assert!(validate(&man(r#"{"id":"x","name":"X","minHost":"0.0.1"}"#)).is_ok());
        assert!(validate(&man(r#"{"id":"x","name":"X"}"#)).is_ok());
        assert_eq!(man(r#"{"id":"x","name":"X","minHost":"0.9.4"}"#).min_host, "0.9.4");
    }

    #[test]
    fn match_rules() {
        let m = MatchRules { apps: vec![], models: vec!["Pixel*".into()], serials: vec!["*".into()] };
        assert!(device_matches(&m, "Pixel 7", "abc", false));
        assert!(!device_matches(&m, "SM G973F", "abc", false));
        let m = MatchRules { apps: vec!["com.my.game".into()], models: vec![], serials: vec!["emulator*".into()] };
        assert!(!device_matches(&m, "x", "R58M", true));
        assert!(device_matches(&m, "x", "emulator-5554", true));
        assert!(!device_matches(&m, "x", "emulator-5554", false));
        assert!(device_matches(&MatchRules::default(), "x", "y", false));
    }

    #[test]
    fn perms() {
        let granted = vec!["device.shell".to_string(), "fs".to_string()];
        assert!(has_perm(&granted, "device.shell"));
        assert!(has_perm(&granted, "fs"));
        assert!(has_perm(&granted, "fs.read"));
        assert!(!has_perm(&granted, "device.files"));
        assert!(!has_perm(&[], "device.shell"));

        // 清单里没有的权限不算需要授权
        let perms = vec!["device.shell".to_string()];
        assert!(!needs_grant(&perms, &granted));
        // 新增了权限 → needs_grant
        let perms = vec!["device.shell".to_string(), "exec".to_string()];
        assert!(needs_grant(&perms, &granted));
        // fs 已覆盖 fs.read
        let perms = vec!["fs.read".to_string()];
        assert!(!needs_grant(&perms, &granted));
        // mcp 是 ai 的旧名，互相等价；ai.call 要单独授予
        let old = vec!["mcp".to_string()];
        assert!(has_perm(&old, "ai"));
        assert!(!needs_grant(&["ai".to_string()], &old));
        assert!(!has_perm(&old, "ai.call"));
        // 空清单
        assert!(!needs_grant(&[], &[]));
    }

    #[test]
    fn tool_names() {
        assert_eq!(tool_name("mygame", "install-test"), "mygame__install-test");
        assert_eq!(tool_name("my game", "a b/c"), "my_game__a_b_c");
        assert_eq!(tool_name("中文", "x"), "____x");
        assert_eq!(tool_name(&"a".repeat(80), "b").len(), 64);
    }

    #[test]
    fn rel_paths() {
        assert!(safe_rel("web/index.html"));
        assert!(safe_rel("./web/index.html"));
        assert!(!safe_rel("/etc/passwd"));
        assert!(!safe_rel("..\\x"));
        assert!(!safe_rel("web/../../x"));
        assert!(!safe_rel("C:\\x"));
        assert!(!safe_rel(""));
    }
}
