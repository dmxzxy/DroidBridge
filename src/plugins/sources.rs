//! 插件源：从清单（index.json）列出、安装、更新插件。说明见 docs/plugins/SOURCES.md。
//!
//! 插件源是一个清单地址（http(s) 或本机文件夹/文件）。清单列出插件和插件包（zip）的位置；
//! 安装时下载插件包，把对应目录复制到 `plugins/<id>/`，并写入 `.source.json` 记录来源和版本。

use super::*;
use serde::{Deserialize, Serialize};
use std::io::Read;

const INDEX_LIMIT: u64 = 4 << 20;
const ZIP_LIMIT: u64 = 200 << 20;
const UNZIP_LIMIT: u64 = 500 << 20;
const MARKER: &str = ".source.json";
const CHECK_EVERY: u64 = 6 * 3600;

/// 插件源的安装、更新、卸载依次进行
static BUSY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Serialize, Deserialize, Clone, Default)]
pub(crate) struct Source {
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token: String,
    /// 令牌所在的请求头，默认 `Authorization: Bearer`
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub header: String,
    #[serde(default)]
    pub index: Option<Index>,
    /// 自动更新这个插件源里的插件
    #[serde(default)]
    pub auto: bool,
    /// 上次检查的时间（秒）
    #[serde(default)]
    pub checked: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub(crate) struct Index {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub zip: String,
    #[serde(default)]
    pub plugins: Vec<Entry>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub(crate) struct Entry {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    /// 插件自己的版本；清单没给时用清单整体的版本
    #[serde(default)]
    pub version: String,
    /// 这个插件单独的包；清单没给时用清单整体的包
    #[serde(default)]
    pub zip: String,
    /// 更新说明
    #[serde(default)]
    pub notes: String,
    /// 需要的最低 DroidBridge 版本
    #[serde(default, rename = "minHost")]
    pub min_host: String,
}

impl Entry {
    /// 插件自己的版本，缺省时用清单整体的版本
    fn version_in(&self, idx: &Index) -> String {
        let v = self.version.trim();
        if v.is_empty() { idx.version.trim().to_string() } else { v.to_string() }
    }

    /// 插件自己的包，缺省时用清单整体的包
    fn zip_in<'a>(&'a self, idx: &'a Index) -> &'a str {
        let z = self.zip.trim();
        if z.is_empty() { idx.zip.trim() } else { z }
    }
}

/// 插件目录里的 `.source.json`
#[derive(Serialize, Deserialize, Clone, Default)]
pub(crate) struct Marker {
    pub source: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub path: String,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn list_path(root: &Path) -> PathBuf {
    root.join(".sources.json")
}

pub(crate) fn load(root: &Path) -> Vec<Source> {
    std::fs::read(list_path(root)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save(root: &Path, list: &[Source]) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(list).map_err(|e| e.to_string())?;
    let tmp = root.join(".sources.json.tmp");
    std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, list_path(root)).map_err(|e| e.to_string())
}

/// 规范化用户输入的地址：去掉首尾空白和引号；本机路径统一成 `/`
pub(crate) fn normalize(url: &str) -> String {
    let u = url.trim().trim_matches('"').trim();
    if is_http(u) { u.to_string() } else { u.replace('\\', "/") }
}

fn is_http(u: &str) -> bool {
    let l = u.to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://")
}

/// `scheme://host[:port]`（小写）
fn origin(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else { return String::new() };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    format!("{}://{}", scheme.to_ascii_lowercase(), host.to_ascii_lowercase())
}

/// 相对地址按 base 解析（http 地址或本机路径）
pub(crate) fn resolve(base: &str, rel: &str) -> String {
    if is_http(rel) {
        return rel.to_string();
    }
    if is_http(base) {
        if rel.starts_with("//") {
            let scheme = base.split("://").next().unwrap_or("https");
            return format!("{scheme}:{rel}");
        }
        if rel.starts_with('/') {
            return origin(base) + rel;
        }
        let path_end = base.find(['?', '#']).unwrap_or(base.len());
        let dir = &base[..path_end];
        let dir = &dir[..dir.rfind('/').map(|i| i + 1).unwrap_or(dir.len())];
        return format!("{dir}{rel}");
    }
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return rel.to_string();
    }
    let parent = Path::new(base).parent().map(Path::to_path_buf).unwrap_or_default();
    parent.join(rel).to_string_lossy().replace('\\', "/")
}

/// 只允许相对路径，且不含 `..`
fn safe_rel(p: &str) -> Option<PathBuf> {
    let p = p.trim().trim_matches('/');
    let mut out = PathBuf::new();
    for part in p.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return None,
            s if s.contains(':') => return None,
            s => out.push(s),
        }
    }
    Some(out)
}

// ---------------------------------------------------------------- 读取

type Resp = ureq::http::Response<ureq::Body>;

/// GET（手动跟随跳转，令牌只发给插件源所在的主机）
fn get(src: &Source, url: &str, timeout: Duration) -> Result<Resp, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .into();
    let home = origin(&src.url);
    let mut url = url.to_string();
    for _ in 0..8 {
        let mut req = agent.get(&url);
        if !src.token.is_empty() && origin(&url) == home {
            let (name, value) = auth_header(src);
            req = req.header(name.as_str(), value.as_str());
        }
        let resp = req.call().map_err(|e| tr!("连接失败：{e}", "Connection failed: {e}"))?;
        let code = resp.status().as_u16();
        if (300..400).contains(&code) {
            if let Some(loc) = resp.headers().get("location").and_then(|v| v.to_str().ok()) {
                url = resolve(&url, loc);
                continue;
            }
        }
        if code == 401 || code == 403 {
            return Err(tr!("没有权限（HTTP {code}），需要令牌或开放下载", "Access denied (HTTP {code}); a token or public access is required"));
        }
        if !(200..300).contains(&code) {
            return Err(tr!("下载失败：HTTP {code}", "Download failed: HTTP {code}"));
        }
        return Ok(resp);
    }
    Err(tr!("跳转次数过多", "Too many redirects"))
}

fn auth_header(src: &Source) -> (String, String) {
    let h = src.header.trim();
    if h.is_empty() || h.eq_ignore_ascii_case("authorization") {
        ("Authorization".into(), format!("Bearer {}", src.token))
    } else {
        (h.to_string(), src.token.clone())
    }
}

/// 清单所在位置：本机文件夹时为其中的 index.json
fn index_location(url: &str) -> String {
    if is_http(url) {
        return url.to_string();
    }
    let p = Path::new(url);
    if p.is_dir() { p.join("index.json").to_string_lossy().replace('\\', "/") } else { url.to_string() }
}

/// 取清单并校验（阻塞）
pub(crate) fn fetch_index(src: &Source) -> Result<Index, String> {
    let loc = index_location(&src.url);
    let data = if is_http(&loc) {
        let mut resp = get(src, &loc, Duration::from_secs(30))?;
        resp.body_mut().with_config().limit(INDEX_LIMIT).read_to_vec().map_err(|e| tr!("读取清单失败：{e}", "Failed to read the index: {e}"))?
    } else {
        std::fs::read(&loc).map_err(|e| tr!("读取清单失败：{e}", "Failed to read the index: {e}"))?
    };
    let mut idx: Index = serde_json::from_slice(&data).map_err(|e| tr!("清单格式不正确：{e}", "Invalid index: {e}"))?;
    let mut seen = HashSet::new();
    for e in &mut idx.plugins {
        e.id = e.id.trim().to_string();
        if !manifest::valid_id(&e.id) {
            return Err(tr!("清单里的插件 id 不合法：{0}", "Invalid plugin id in the index: {0}", e.id));
        }
        if !seen.insert(e.id.clone()) {
            return Err(tr!("清单里有重复的插件 id：{0}", "Duplicate plugin id in the index: {0}", e.id));
        }
        if safe_rel(&e.path).is_none() {
            return Err(tr!("清单里的路径不合法：{0}", "Invalid path in the index: {0}", e.path));
        }
        if !e.zip.trim().is_empty() && safe_rel(&e.zip).is_none() {
            return Err(tr!("清单里的插件包路径不合法：{0}", "Invalid package path in the index: {0}", e.zip));
        }
        if e.name.trim().is_empty() {
            e.name = e.id.clone();
        }
    }
    Ok(idx)
}

/// 准备插件包，返回解压后的根目录（本机文件夹且没有 zip 时直接用清单所在目录）
fn fetch_package(src: &Source, idx: &Index, e: &Entry, tmp: &Path) -> Result<PathBuf, String> {
    let loc = index_location(&src.url);
    let zip = e.zip_in(idx);
    if zip.is_empty() {
        if is_http(&loc) {
            return Err(tr!("清单没有写插件包（zip）地址", "The index has no package (zip) address"));
        }
        return Ok(Path::new(&loc).parent().map(Path::to_path_buf).unwrap_or_default());
    }
    let zip_loc = resolve(&loc, zip);
    std::fs::create_dir_all(tmp).map_err(|e| e.to_string())?;
    let zip_path = if is_http(&zip_loc) {
        let path = tmp.join("package.zip");
        let mut resp = get(src, &zip_loc, Duration::from_secs(1800))?;
        let mut f = std::io::BufWriter::new(std::fs::File::create(&path).map_err(|e| e.to_string())?);
        let mut r = resp.body_mut().as_reader().take(ZIP_LIMIT + 1);
        let n = std::io::copy(&mut r, &mut f).map_err(|e| tr!("下载插件包失败：{e}", "Failed to download the package: {e}"))?;
        if n > ZIP_LIMIT {
            return Err(tr!("插件包超过 200 MB", "The package is larger than 200 MB"));
        }
        std::io::Write::flush(&mut f).map_err(|e| e.to_string())?;
        path
    } else {
        PathBuf::from(&zip_loc)
    };
    let out = tmp.join("pkg");
    unzip(&zip_path, &out)?;
    Ok(out)
}

/// 解压：拒绝绝对路径、`..` 和链接，总大小有上限
pub(crate) fn unzip(zip_path: &Path, out: &Path) -> Result<(), String> {
    let f = std::fs::File::open(zip_path).map_err(|e| tr!("打开插件包失败：{e}", "Failed to open the package: {e}"))?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| tr!("插件包不是有效的 zip：{e}", "The package is not a valid zip: {e}"))?;
    let mut total = 0u64;
    for i in 0..z.len() {
        let mut file = z.by_index(i).map_err(|e| e.to_string())?;
        if file.is_symlink() {
            continue;
        }
        let Some(rel) = file.enclosed_name() else {
            return Err(tr!("插件包里有不安全的路径：{0}", "Unsafe path in the package: {0}", file.name()));
        };
        let dst = out.join(rel);
        if file.is_dir() {
            std::fs::create_dir_all(&dst).map_err(|e| e.to_string())?;
            continue;
        }
        total += file.size();
        if total > UNZIP_LIMIT {
            return Err(tr!("插件包解压后超过 500 MB", "The unpacked package is larger than 500 MB"));
        }
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        let mut w = std::fs::File::create(&dst).map_err(|e| e.to_string())?;
        std::io::copy(&mut file, &mut w).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 插件包里某个插件的目录；包外面多包了一层目录时也能找到
fn plugin_dir_in(pkg: &Path, rel: &Path) -> Option<PathBuf> {
    let direct = pkg.join(rel);
    if direct.join("plugin.json").is_file() {
        return Some(direct);
    }
    let dirs: Vec<PathBuf> = std::fs::read_dir(pkg).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    if let [only] = dirs.as_slice() {
        let nested = only.join(rel);
        if nested.join("plugin.json").is_file() {
            return Some(nested);
        }
    }
    None
}

// ---------------------------------------------------------------- 文件

/// 递归复制（跳过 `.git` 和链接）
fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for e in std::fs::read_dir(from).map_err(|e| e.to_string())?.flatten() {
        let name = e.file_name();
        if name == ".git" {
            continue;
        }
        let ft = e.file_type().map_err(|e| e.to_string())?;
        if ft.is_symlink() {
            continue;
        }
        let dst = to.join(&name);
        if ft.is_dir() {
            copy_dir(&e.path(), &dst)?;
        } else {
            std::fs::copy(e.path(), &dst).map_err(|err| format!("{}: {err}", dst.display()))?;
        }
    }
    Ok(())
}

/// 让 dst 和 src 内容一致：覆盖同名文件，删除多出来的（保留 `.source.json`）
fn mirror(src: &Path, dst: &Path) -> Result<(), String> {
    copy_dir(src, dst)?;
    prune(src, dst)
}

fn prune(src: &Path, dst: &Path) -> Result<(), String> {
    for e in std::fs::read_dir(dst).map_err(|e| e.to_string())?.flatten() {
        let name = e.file_name();
        if name == MARKER {
            continue;
        }
        let s = src.join(&name);
        let p = e.path();
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if s.is_dir() {
                prune(&s, &p)?;
            } else {
                std::fs::remove_dir_all(&p).map_err(|err| format!("{}: {err}", p.display()))?;
            }
        } else if !s.exists() {
            std::fs::remove_file(&p).map_err(|err| format!("{}: {err}", p.display()))?;
        }
    }
    Ok(())
}

pub(crate) fn read_marker(dir: &Path) -> Option<Marker> {
    std::fs::read(dir.join(MARKER)).ok().and_then(|b| serde_json::from_slice(&b).ok())
}

/// 插件目录是不是链接（本地开发挂进来的）
pub(crate) fn is_linked(root: &Path, dir: &Path) -> bool {
    if std::fs::symlink_metadata(dir).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        return true;
    }
    match (std::fs::canonicalize(root), std::fs::canonicalize(dir)) {
        (Ok(r), Ok(d)) => d.parent() != Some(r.as_path()),
        _ => false,
    }
}

// ---------------------------------------------------------------- 状态

/// 插件在这个源里的状态：install | installed | update | local | other（registered 为已添加的插件源地址）
fn status(plugs: &Plugins, src: &Source, idx: &Index, e: &Entry, registered: &HashSet<String>) -> (&'static str, String) {
    let dir = plugs.get(&e.id).map(|p| p.dir.clone()).unwrap_or_else(|| plugs.root().join(&e.id));
    if !dir.exists() {
        return ("install", String::new());
    }
    if is_linked(plugs.root(), &dir) {
        return ("local", String::new());
    }
    match read_marker(&dir) {
        None => ("local", String::new()),
        Some(m) if m.source != src.url && registered.contains(&m.source) => ("other", m.source),
        Some(m) if m.source == src.url && m.version == e.version_in(idx) => ("installed", m.version),
        Some(m) => ("update", m.version),
    }
}

/// 当前版本是否满足插件要求的最低版本
pub(crate) fn host_ok(min_host: &str) -> bool {
    let v = min_host.trim();
    v.is_empty() || !manifest::version_cmp(env!("CARGO_PKG_VERSION"), v).is_lt()
}

/// 给界面的数据：插件源列表（不含令牌）和合并后的插件目录（同一 id 只出现一次）
pub(crate) fn view(st: &AppState) -> Value {
    let plugs = &st.plugins;
    let list = load(plugs.root());
    let registered: HashSet<String> = list.iter().map(|s| s.url.clone()).collect();
    let mut updates = Vec::new();
    let mut catalog: Vec<Value> = Vec::new();
    let mut seen = HashSet::new();
    // 已安装的插件归它的来源；其余按插件源的顺序取第一个
    let owner = |id: &str| -> Option<String> {
        let dir = plugs.get(id).map(|p| p.dir.clone()).unwrap_or_else(|| plugs.root().join(id));
        read_marker(&dir).map(|m| m.source).filter(|u| registered.contains(u))
    };
    let sources: Vec<Value> = list
        .iter()
        .map(|s| {
            let idx = s.index.clone().unwrap_or_default();
            let names: Vec<String> = idx.plugins.iter().map(|e| e.name.clone()).collect();
            json!({"url": s.url, "name": idx.name, "version": idx.version, "checked": s.checked, "error": s.error,
                   "hasToken": !s.token.is_empty(), "header": s.header, "auto": s.auto, "count": idx.plugins.len(),
                   "plugins": names})
        })
        .collect();
    for pass in 0..2 {
        for s in &list {
            let Some(idx) = s.index.as_ref() else { continue };
            for e in &idx.plugins {
                if seen.contains(&e.id) || (pass == 0 && owner(&e.id).as_deref() != Some(s.url.as_str())) {
                    continue;
                }
                seen.insert(e.id.clone());
                let (mut state, extra) = status(plugs, s, idx, e, &registered);
                // 需要更高的 DroidBridge 版本：不显示安装/更新，只提示
                if matches!(state, "install" | "update") && !host_ok(&e.min_host) {
                    state = "incompatible";
                }
                if state == "update" {
                    updates.push(e.id.clone());
                }
                catalog.push(json!({"id": e.id, "name": e.name, "description": e.description, "permissions": e.permissions,
                                    "url": s.url, "source": idx.name, "version": e.version_in(idx), "notes": e.notes,
                                    "minHost": e.min_host, "status": state, "installed": extra}));
            }
        }
    }
    json!({"sources": sources, "catalog": catalog, "updates": updates})
}

// ---------------------------------------------------------------- 操作

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

/// 刷新一个或全部插件源的清单（only_stale 时只刷新超过 max_age 秒没检查的）；返回是否真的刷新了
pub(crate) async fn refresh(st: &Arc<AppState>, url: Option<&str>, max_age: u64) -> Result<bool, String> {
    let root = st.plugins.root().to_path_buf();
    let list = load(&root);
    let mut fresh = Vec::new();
    for s in list.iter().filter(|s| url.is_none_or(|u| s.url == u) && now().saturating_sub(s.checked) >= max_age) {
        let s2 = s.clone();
        let r = blocking(move || fetch_index(&s2)).await;
        fresh.push((s.url.clone(), r));
    }
    if fresh.is_empty() {
        return Ok(false);
    }
    let _g = BUSY.lock().await;
    let mut list = load(&root);
    for (u, r) in fresh {
        if let Some(s) = list.iter_mut().find(|s| s.url == u) {
            s.checked = now();
            match r {
                Ok(idx) => {
                    s.index = Some(idx);
                    s.error.clear();
                }
                Err(e) => s.error = e,
            }
        }
    }
    save(&root, &list)?;
    Ok(true)
}

/// 添加插件源：先取一次清单，取不到就不添加
pub(crate) async fn add(st: &Arc<AppState>, url: &str, token: &str, header: &str) -> Result<(), String> {
    let url = normalize(url);
    if url.is_empty() {
        return Err(tr!("请输入插件源地址", "Enter the source address"));
    }
    let root = st.plugins.root().to_path_buf();
    if load(&root).iter().any(|s| s.url == url) {
        return Err(tr!("这个插件源已经添加过了", "This source has already been added"));
    }
    let mut src = Source { url, token: token.trim().into(), header: header.trim().into(), ..Default::default() };
    let s2 = src.clone();
    src.index = Some(blocking(move || fetch_index(&s2)).await?);
    src.checked = now();
    let _g = BUSY.lock().await;
    let mut list = load(&root);
    list.push(src);
    save(&root, &list)
}

/// 修改令牌、请求头或自动更新；token 为 None 时保留原值
pub(crate) async fn edit(st: &Arc<AppState>, url: &str, token: Option<&str>, header: Option<&str>, auto: Option<bool>) -> Result<(), String> {
    let root = st.plugins.root().to_path_buf();
    let reread = {
        let _g = BUSY.lock().await;
        let mut list = load(&root);
        let s = list.iter_mut().find(|s| s.url == url).ok_or_else(|| tr!("插件源不存在", "Source not found"))?;
        if let Some(t) = token {
            s.token = t.trim().into();
            s.checked = 0;
        }
        if let Some(h) = header {
            s.header = h.trim().into();
            s.checked = 0;
        }
        if let Some(a) = auto {
            s.auto = a;
        }
        let reread = s.checked == 0;
        save(&root, &list)?;
        reread
    };
    if !reread {
        return Ok(());
    }
    refresh(st, Some(url), 0).await.map(|_| ())
}

pub(crate) async fn remove(st: &Arc<AppState>, url: &str) -> Result<(), String> {
    let root = st.plugins.root().to_path_buf();
    let _g = BUSY.lock().await;
    let mut list = load(&root);
    list.retain(|s| s.url != url);
    save(&root, &list)
}

/// 安装或更新插件源里的一个插件，返回插件 id
pub(crate) async fn install(st: &Arc<AppState>, url: &str, id: &str) -> Result<String, String> {
    let _g = BUSY.lock().await;
    let plugs = &st.plugins;
    let root = plugs.root().to_path_buf();
    let mut list = load(&root);
    let src = list.iter().find(|s| s.url == url).cloned().ok_or_else(|| tr!("插件源不存在", "Source not found"))?;
    let s2 = src.clone();
    let idx = blocking(move || fetch_index(&s2)).await?;
    if let Some(s) = list.iter_mut().find(|s| s.url == url) {
        s.index = Some(idx.clone());
        s.checked = now();
        s.error.clear();
    }
    save(&root, &list)?;
    let entry = idx.plugins.iter().find(|e| e.id == id).cloned().ok_or_else(|| tr!("插件源里没有 {id}", "The source has no plugin {id}"))?;
    if !host_ok(&entry.min_host) {
        return Err(tr!("需要 DroidBridge {0}", "Requires DroidBridge {0}", entry.min_host.trim()));
    }
    if super::builtin::is_builtin(id) {
        return Err(tr!("{id} 是内置插件，不能用插件源覆盖", "{id} is a built-in plugin and cannot be replaced from a source"));
    }
    let (state, from) = status(plugs, &src, &idx, &entry, &list.iter().map(|s| s.url.clone()).collect());
    match state {
        "local" => return Err(tr!("{id} 是本地插件，不能用插件源覆盖", "{id} is a local plugin and cannot be replaced from a source")),
        "other" => return Err(tr!("{id} 来自另一个插件源：{from}", "{id} comes from another source: {from}")),
        _ => {}
    }
    let existing = plugs.get(id);
    let dst = existing.as_ref().map(|p| p.dir.clone()).unwrap_or_else(|| root.join(id));
    let tmp = root.join(".tmp").join(format!("src-{:08x}", crate::util::random_scid()));
    let staged = tmp.join("plugin");
    let marker = Marker { source: src.url.clone(), version: entry.version_in(&idx), path: entry.path.clone() };
    let (src2, idx2, entry2, tmp2, staged2) = (src.clone(), idx.clone(), entry.clone(), tmp.clone(), staged.clone());
    let prep = blocking(move || {
        let pkg = fetch_package(&src2, &idx2, &entry2, &tmp2)?;
        let rel = safe_rel(&entry2.path).unwrap_or_default();
        let dir = plugin_dir_in(&pkg, &rel).ok_or_else(|| tr!("插件包里找不到 {0}/plugin.json", "{0}/plugin.json is missing in the package", entry2.path))?;
        let man = manifest::load(&dir)?;
        if man.id != entry2.id {
            return Err(tr!("插件包里的 id 是 {0}，清单写的是 {1}", "The package says id {0} but the index says {1}", man.id, entry2.id));
        }
        copy_dir(&dir, &staged2)?;
        let data = serde_json::to_vec_pretty(&marker).map_err(|e| e.to_string())?;
        std::fs::write(staged2.join(MARKER), data).map_err(|e| e.to_string())
    })
    .await;
    if let Err(e) = prep {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    let r = if dst.exists() { update_files(st, existing.as_ref(), &staged, &dst).await } else { std::fs::rename(&staged, &dst).map_err(|e| e.to_string()) };
    let _ = std::fs::remove_dir_all(&tmp);
    r?;
    plugs.rescan(st);
    Ok(id.to_string())
}

/// 把新版本复制到已安装的目录；文件被占用（插件进程在用）时先停止插件再试，之后由热重载重新启动
async fn update_files(st: &Arc<AppState>, ph: Option<&Arc<Plugin>>, staged: &Path, dst: &Path) -> Result<(), String> {
    let (s, d) = (staged.to_path_buf(), dst.to_path_buf());
    if blocking(move || mirror(&s, &d)).await.is_ok() {
        return Ok(());
    }
    if let Some(ph) = ph {
        stop_host(ph);
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let (s, d) = (staged.to_path_buf(), dst.to_path_buf());
    let r = blocking(move || mirror(&s, &d)).await;
    if let Some(ph) = ph {
        st.plugins.start(st, ph);
    }
    r.map_err(|e| tr!("更新失败，文件被占用：{e}", "Update failed, a file is in use: {e}"))
}

/// 卸载从插件源安装的插件（插件数据保留）
pub(crate) async fn uninstall(st: &Arc<AppState>, id: &str) -> Result<(), String> {
    let _g = BUSY.lock().await;
    let plugs = &st.plugins;
    let ph = plugs.get(id).ok_or_else(|| tr!("插件不存在", "Plugin not found"))?;
    if plugs.is_builtin(&ph) {
        return Err(tr!("内置插件不能删除，可以停用", "Built-in plugins cannot be removed, only disabled"));
    }
    if is_linked(plugs.root(), &ph.dir) || read_marker(&ph.dir).is_none() {
        return Err(tr!("只能卸载从插件源安装的插件", "Only plugins installed from a source can be uninstalled"));
    }
    stop_host(&ph);
    let dir = ph.dir.clone();
    let mut last = String::new();
    for _ in 0..6 {
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {
                last.clear();
                break;
            }
            Err(e) => last = e.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    plugs.rescan(st);
    if last.is_empty() { Ok(()) } else { Err(tr!("卸载失败：{last}", "Uninstall failed: {last}")) }
}

/// 自动更新：auto 打开的插件源里，新版本不需要新权限、宿主版本也够的插件直接安装
async fn auto_update(st: &Arc<AppState>) {
    let plugs = &st.plugins;
    let list = load(plugs.root());
    let registered: HashSet<String> = list.iter().map(|s| s.url.clone()).collect();
    let mut todo = Vec::new();
    for s in list.iter().filter(|s| s.auto) {
        let Some(idx) = s.index.as_ref() else { continue };
        for e in &idx.plugins {
            if status(plugs, s, idx, e, &registered).0 != "update" || !host_ok(&e.min_host) {
                continue;
            }
            let granted = plugs.get(&e.id).map(|p| p.granted()).unwrap_or_default();
            if !manifest::needs_grant(&e.permissions, &granted) {
                todo.push((s.url.clone(), e.id.clone()));
            }
        }
    }
    let mut done = false;
    for (url, id) in todo {
        if install(st, &url, &id).await.is_ok() {
            done = true;
        }
    }
    if done {
        st.plugins.emit_changed();
    }
}

/// 后台每隔一段时间检查各插件源有没有新版本；刷新到新清单后顺带自动更新
pub async fn run(st: Arc<AppState>) {
    tokio::time::sleep(Duration::from_secs(20)).await;
    loop {
        if refresh(&st, None, CHECK_EVERY).await.unwrap_or(false) {
            auto_update(&st).await;
        }
        tokio::time::sleep(Duration::from_secs(600)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_paths() {
        assert_eq!(resolve("https://h.com/a/b/index.json", "plugins.zip"), "https://h.com/a/b/plugins.zip");
        assert_eq!(resolve("https://h.com/a/index.json?x=1", "p.zip"), "https://h.com/a/p.zip");
        assert_eq!(resolve("https://h.com/a/index.json", "/z/p.zip"), "https://h.com/z/p.zip");
        assert_eq!(resolve("https://h.com/a/index.json", "http://o.com/p.zip"), "http://o.com/p.zip");
        assert_eq!(resolve("D:/x/index.json", "p.zip"), "D:/x/p.zip");
    }

    #[test]
    fn rel_paths() {
        assert!(safe_rel("a/b").is_some());
        assert!(safe_rel("").is_some());
        assert!(safe_rel("../a").is_none());
        assert!(safe_rel("a/../../b").is_none());
        assert!(safe_rel("C:/x").is_none());
    }

    #[test]
    fn unzip_rejects_escape() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("db-src-{:08x}", crate::util::random_scid()));
        std::fs::create_dir_all(&dir).unwrap();
        let mk = |name: &str, entry: &str| {
            let path = dir.join(name);
            let mut w = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            w.start_file(entry, opt).unwrap();
            w.write_all(b"x").unwrap();
            w.finish().unwrap();
            path
        };
        let good = mk("good.zip", "a/plugin.json");
        assert!(unzip(&good, &dir.join("g")).is_ok());
        assert!(dir.join("g/a/plugin.json").is_file());
        let bad = mk("bad.zip", "../evil.txt");
        assert!(unzip(&bad, &dir.join("b")).is_err());
        assert!(!dir.join("evil.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn token_only_same_host() {
        assert_eq!(origin("https://Git.example.com:8443/api/x"), "https://git.example.com:8443");
        assert_ne!(origin("https://git.example.com/a"), origin("https://cdn.example.com/a"));
    }

    #[test]
    fn status_uses_plugin_version() {
        let base = std::env::temp_dir().join(format!("db-src-{:08x}", crate::util::random_scid()));
        let plugs = Plugins::new(&base);
        let dir = plugs.root().join("demo");
        std::fs::create_dir_all(&dir).unwrap();
        let src = Source { url: "https://h.com/p/index.json".into(), ..Default::default() };
        let idx = Index { version: "2026.11.23".into(), ..Default::default() };
        let mut registered = HashSet::new();
        registered.insert(src.url.clone());
        let marker = |version: &str| {
            let m = Marker { source: src.url.clone(), version: version.into(), path: "demo".into() };
            std::fs::write(dir.join(MARKER), serde_json::to_vec(&m).unwrap()).unwrap();
        };
        let entry = |version: &str| Entry { id: "demo".into(), path: "demo".into(), version: version.into(), ..Default::default() };

        // 没装
        let missing = Entry { id: "demo2".into(), path: "demo2".into(), ..Default::default() };
        assert_eq!(status(&plugs, &src, &idx, &missing, &registered).0, "install");

        // 版本相同：已是最新
        marker("1.4.0+3fa2c1d9");
        assert_eq!(status(&plugs, &src, &idx, &entry("1.4.0+3fa2c1d9"), &registered).0, "installed");
        // 版本号没变、内容变了（哈希不同）：有更新
        assert_eq!(status(&plugs, &src, &idx, &entry("1.4.0+9b1e7a20"), &registered).0, "update");
        // 插件自己升了版本
        assert_eq!(status(&plugs, &src, &idx, &entry("1.5.0+9b1e7a20"), &registered).0, "update");
        // 清单没写插件版本：退回清单整体的版本（旧格式）
        assert_eq!(status(&plugs, &src, &idx, &entry(""), &registered).0, "update");
        marker("2026.11.23");
        assert_eq!(status(&plugs, &src, &idx, &entry(""), &registered).0, "installed");
        // 来自另一个已添加的插件源
        let other = Source { url: "https://h.com/other/index.json".into(), ..Default::default() };
        assert_eq!(status(&plugs, &other, &idx, &entry("1.4.0"), &registered).0, "other");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn host_version_gate() {
        assert!(host_ok(""));
        assert!(host_ok("0.0.1"));
        assert!(!host_ok("999.0"));
    }
}

// ---------------------------------------------------------------- HTTP

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::net::SocketAddr;

fn deny(st: &AppState, headers: &HeaderMap, addr: SocketAddr, q: &HashMap<String, String>) -> Option<Response> {
    if crate::is_local_request(st, headers, Some(addr.ip()), q) && !q.contains_key("peer") {
        return None;
    }
    Some((StatusCode::FORBIDDEN, Json(json!({"error": tr!("只能在这台电脑上管理插件", "Plugins can only be managed on this computer")}))).into_response())
}

/// GET `/api/plugins/sources[?refresh=1]`：refresh 时先刷新 5 分钟内没检查过的源
pub async fn sources_get(
    State(st): State<Arc<AppState>>,
    Query(q): Query<HashMap<String, String>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = deny(&st, &headers, addr, &q) {
        return r;
    }
    if q.get("refresh").is_some_and(|v| v == "1") {
        let _ = refresh(&st, None, 300).await;
    }
    Json(view(&st)).into_response()
}

#[derive(Deserialize)]
struct SourceReq {
    action: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    id: String,
    token: Option<String>,
    header: Option<String>,
    auto: Option<bool>,
}

/// POST `/api/plugins/sources`：add | edit | remove | refresh | install | uninstall
pub async fn sources_post(
    State(st): State<Arc<AppState>>,
    Query(q): Query<HashMap<String, String>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(r) = deny(&st, &headers, addr, &q) {
        return r;
    }
    let Ok(req) = serde_json::from_slice::<SourceReq>(&body) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": tr!("请求格式不正确", "Invalid request")}))).into_response();
    };
    let r = match req.action.as_str() {
        "add" => add(&st, &req.url, req.token.as_deref().unwrap_or(""), req.header.as_deref().unwrap_or("")).await.map(|_| json!({"ok": true})),
        "edit" => edit(&st, &req.url, req.token.as_deref(), req.header.as_deref(), req.auto).await.map(|_| json!({"ok": true})),
        "remove" => remove(&st, &req.url).await.map(|_| json!({"ok": true})),
        "refresh" => refresh(&st, (!req.url.is_empty()).then_some(req.url.as_str()), 0).await.map(|_| json!({"ok": true})),
        "install" => install(&st, &req.url, &req.id).await.map(|id| json!({"ok": true, "id": id})),
        "uninstall" => uninstall(&st, &req.id).await.map(|_| json!({"ok": true})),
        _ => Err(tr!("未知操作", "Unknown action")),
    };
    match r {
        Ok(mut v) => {
            v["view"] = view(&st);
            Json(v).into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response(),
    }
}
