//! 沙箱环境与 `__host` 底层函数（给 src/plugin_prelude.lua 用）。

use super::json;
use super::local;
use super::state::Ctl;
use mlua::debug::Debug;
use mlua::{HookTriggers, Lua, Table, Thread, Value, VmState};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

const PRELUDE: &str = include_str!("../plugin_prelude.lua");

/// 调度令牌：Lua 侧 `coroutine.yield(TOKEN, req)`，驱动线程据此识别调度请求
pub const SCHED_TOKEN: &str = "\u{1}db.sched";

/// 给协程安装 CPU 上限钩子（每 N 条指令检查一次）
pub fn set_thread_hook(thread: &Thread, deadline: &Rc<Cell<Option<Instant>>>, instr_step: u32) -> mlua::Result<()> {
    let d = deadline.clone();
    thread.set_hook(
        HookTriggers::new().every_nth_instruction(instr_step.max(1)),
        move |lua: &Lua, _dbg: &Debug| {
            if let Some(dl) = d.get() {
                if Instant::now() >= dl {
                    // 带上卡住的位置，控制台里能直接看到是哪一行在死循环
                    let at = where_impl(lua).map(|w| format!("{w}: ")).unwrap_or_default();
                    return Err(mlua::Error::RuntimeError(format!("{at}script timeout (ran over 3 s without yielding)")));
                }
            }
            Ok(VmState::Continue)
        },
    )
}

/// 找到第一个属于插件代码的调用栈位置（跳过宿主 prelude）
pub fn where_impl(lua: &Lua) -> Option<String> {
    for level in 0..24usize {
        let hit = lua.inspect_stack(level, |d: &Debug| {
            let line = d.current_line()?;
            let source = d.source();
            let short = source.short_src.as_deref()?.to_string();
            if short.starts_with("db-prelude") {
                return None;
            }
            Some(format!("{short}:{line}"))
        });
        if let Some(Some(s)) = hit {
            return Some(s);
        }
    }
    None
}

fn err(msg: impl Into<String>) -> mlua::Error {
    mlua::Error::RuntimeError(msg.into())
}

fn as_bytes(v: &Value) -> Vec<u8> {
    match v {
        Value::String(s) => s.as_bytes().to_vec(),
        other => as_text(other).into_bytes(),
    }
}

fn as_text(v: &Value) -> String {
    match v {
        Value::Nil => String::new(),
        Value::String(s) => s.to_string_lossy().to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Boolean(b) => b.to_string(),
        other => other.type_name().to_string(),
    }
}

fn fs_err(r: Result<(), String>) -> mlua::Result<bool> {
    r.map(|_| true).map_err(err)
}

/// 构建沙箱全局表并加载 Lua 侧封装，返回（沙箱 `_G`，`__host` 表）
pub fn install(
    lua: &Lua,
    ctl: Rc<Ctl>,
    deadline: Rc<Cell<Option<Instant>>>,
    instr_step: u32,
) -> mlua::Result<(Table, Table)> {
    let g = lua.create_table()?;
    let g0 = lua.globals();
    // 基础函数（去掉 dofile/loadfile）
    for k in [
        "assert", "error", "ipairs", "next", "pairs", "pcall", "xpcall", "rawequal", "rawget", "rawlen", "rawset",
        "select", "setmetatable", "getmetatable", "tonumber", "tostring", "type", "_VERSION", "load",
        "collectgarbage",
    ] {
        g.set(k, g0.get::<Value>(k)?)?;
    }
    for lib in ["string", "table", "math", "utf8", "coroutine"] {
        g.set(lib, g0.get::<Value>(lib)?)?;
    }
    // os 只留时间相关
    let os = lua.create_table()?;
    let os0: Table = g0.get("os")?;
    for k in ["time", "clock", "date", "difftime"] {
        os.set(k, os0.get::<Value>(k)?)?;
    }
    g.set("os", os)?;
    g.set("_G", g.clone())?;

    // ---- __host
    let h = lua.create_table()?;
    h.set("token", SCHED_TOKEN)?;
    h.set("null", json::NULL_SENTINEL)?;
    h.set("lang", ctl.lang.clone())?;
    h.set("node", ctl.node.clone())?;
    h.set("globals", g.clone())?;

    h.set(
        "log",
        lua.create_function({
            let ctl = ctl.clone();
            move |_lua, (level, msg, src): (String, String, Option<String>)| {
                ctl.log(&level, &msg, src.as_deref());
                Ok(())
            }
        })?,
    )?;
    h.set("where", lua.create_function(|lua, ()| Ok(where_impl(lua)))?)?;
    h.set(
        "tb",
        lua.create_function(|lua, msg: Value| {
            let msg = match &msg {
                Value::String(s) => s.to_string_lossy().to_string(),
                // 钩子超时 / 内存不足等由 mlua 包成错误 userdata，要取回真实信息
                Value::Error(e) => format!("{e}").trim().to_string(),
                Value::Table(_) => serde_json::to_string(&json::to_json(&msg)).unwrap_or_default(),
                other => as_text(other),
            };
            let msg = msg.strip_prefix("runtime error: ").unwrap_or(&msg).to_string();
            // 嵌套的 xpcall 会多次经过这里：已经带 traceback 的不再追加
            if msg.contains("\nstack traceback:") {
                return Ok(msg);
            }
            let tb = lua.traceback(Some(&msg), 2)?;
            Ok(tb.to_string_lossy().to_string())
        })?,
    )?;
    h.set(
        "has_perm",
        lua.create_function({
            let ctl = ctl.clone();
            move |_lua, p: String| Ok(ctl.has_perm(&p))
        })?,
    )?;
    h.set(
        "uid",
        lua.create_function({
            let ctl = ctl.clone();
            move |_lua, ()| Ok(ctl.next_uid())
        })?,
    )?;
    h.set(
        "plugin",
        lua.create_function({
            let ctl = ctl.clone();
            move |lua, ()| {
                let t = lua.create_table()?;
                t.set("id", ctl.info.id.clone())?;
                t.set("name", ctl.info.name.clone())?;
                t.set("version", ctl.info.version.clone())?;
                t.set("dir", ctl.info.dir.to_string_lossy().to_string())?;
                t.set("dataDir", ctl.info.data_dir.to_string_lossy().to_string())?;
                Ok(t)
            }
        })?,
    )?;
    h.set(
        "settings",
        lua.create_function({
            let ctl = ctl.clone();
            move |lua, ()| json::to_lua(lua, &ctl.settings.borrow().clone())
        })?,
    )?;
    // 插件目录内只读读取（require 用）
    h.set(
        "plugin_read",
        lua.create_function({
            let ctl = ctl.clone();
            move |_lua, rel: String| {
                let full = local::resolve(&ctl, &format!("plugin://{rel}"), false).map_err(err)?;
                std::fs::read_to_string(&full).map_err(|e| err(format!("读取插件文件失败 {}: {e}", full.display())))
            }
        })?,
    )?;

    // ---- 协程
    h.set(
        "new_thread",
        lua.create_function({
            let deadline = deadline.clone();
            move |lua, f: mlua::Function| {
                let th = lua.create_thread(f)?;
                set_thread_hook(&th, &deadline, instr_step)?;
                Ok(th)
            }
        })?,
    )?;
    h.set(
        "spawn",
        lua.create_function({
            let ctl = ctl.clone();
            let deadline = deadline.clone();
            move |lua, body: mlua::Function| {
                let th = lua.create_thread(body)?;
                set_thread_hook(&th, &deadline, instr_step)?;
                let client = ctl.client.borrow().clone();
                ctl.spawn_queue.borrow_mut().push((th, client));
                Ok(())
            }
        })?,
    )?;
    h.set(
        "unpark",
        lua.create_function({
            let ctl = ctl.clone();
            move |_lua, key: u64| {
                ctl.unpark_queue.borrow_mut().push(key);
                Ok(())
            }
        })?,
    )?;

    // ---- 常驻子进程（db.proc；exec 权限在 Lua 层检查，这里再查一次）
    h.set(
        "proc_start",
        lua.create_function({
            let ctl = ctl.clone();
            move |_lua, (program, args, cwd, env): (String, Option<Table>, Option<String>, Option<Table>)| {
                if !ctl.has_perm("exec") {
                    return Err(err("db.proc 需要 exec 权限"));
                }
                let args = super::str_list(args.as_ref());
                let env = super::str_pairs(env.as_ref());
                ctl.procs.borrow_mut().start(&ctl.tx, &program, &args, cwd.as_deref(), &env).map_err(err)
            }
        })?,
    )?;
    h.set(
        "proc_write",
        lua.create_function({
            let ctl = ctl.clone();
            move |_lua, (id, data): (u64, Value)| ctl.procs.borrow_mut().write(id, as_bytes(&data)).map_err(err)
        })?,
    )?;
    h.set(
        "proc_close",
        lua.create_function({
            let ctl = ctl.clone();
            move |_lua, id: u64| {
                ctl.procs.borrow_mut().close_stdin(id);
                Ok(())
            }
        })?,
    )?;
    h.set(
        "proc_kill",
        lua.create_function({
            let ctl = ctl.clone();
            move |_lua, id: u64| {
                ctl.procs.borrow_mut().kill(id);
                Ok(())
            }
        })?,
    )?;
    h.set(
        "proc_alive",
        lua.create_function({
            let ctl = ctl.clone();
            move |_lua, id: u64| Ok(ctl.procs.borrow().alive(id))
        })?,
    )?;

    // ---- 定时器
    h.set(
        "new_timer",
        lua.create_function({
            let ctl = ctl.clone();
            move |_lua, (delay, repeat): (f64, Option<bool>)| {
                let id = ctl.timer_seq.get() + 1;
                ctl.timer_seq.set(id);
                ctl.new_timers.borrow_mut().push((id, delay.max(0.0), repeat.unwrap_or(false)));
                Ok(id)
            }
        })?,
    )?;
    h.set(
        "cancel_timer",
        lua.create_function({
            let ctl = ctl.clone();
            move |_lua, id: u64| {
                ctl.timer_cancel.borrow_mut().push(id);
                Ok(())
            }
        })?,
    )?;

    // ---- 文件
    h.set(
        "fs_read",
        lua.create_function({
            let ctl = ctl.clone();
            // 返回字节 Lua 字符串，二进制文件也不会被破坏
            move |lua, path: String| {
                let data = local::fs_read_bytes(&ctl, &path).map_err(err)?;
                lua.create_string(&data)
            }
        })?,
    )?;
    h.set(
        "fs_write",
        lua.create_function({
            let ctl = ctl.clone();
            move |_lua, (path, data): (String, Value)| local::fs_write(&ctl, &path, &as_bytes(&data), false).map_err(err)
        })?,
    )?;
    h.set(
        "fs_append",
        lua.create_function({
            let ctl = ctl.clone();
            move |_lua, (path, data): (String, Value)| local::fs_write(&ctl, &path, &as_bytes(&data), true).map_err(err)
        })?,
    )?;
    h.set(
        "fs_list",
        lua.create_function({
            let ctl = ctl.clone();
            move |lua, dir: String| {
                let v = local::fs_list(&ctl, &dir).map_err(err)?;
                json::to_lua(lua, &v)
            }
        })?,
    )?;
    h.set(
        "resolve",
        lua.create_function({
            let ctl = ctl.clone();
            // 把本机路径解析成绝对路径并做权限检查（push/pull/install/media.save 交给主进程前使用）
            move |_lua, (path, write): (String, Option<bool>)| {
                let p = local::resolve(&ctl, &path, write.unwrap_or(false)).map_err(err)?;
                Ok(p.to_string_lossy().to_string())
            }
        })?,
    )?;
    h.set(
        "fs_exists",
        lua.create_function({
            let ctl = ctl.clone();
            move |_lua, path: String| Ok(local::fs_exists(&ctl, &path))
        })?,
    )?;
    h.set(
        "fs_stat",
        lua.create_function({
            let ctl = ctl.clone();
            move |lua, path: String| {
                let v = local::fs_stat(&ctl, &path).map_err(err)?;
                json::to_lua(lua, &v)
            }
        })?,
    )?;
    h.set(
        "fs_mkdir",
        lua.create_function({
            let ctl = ctl.clone();
            move |_lua, path: String| fs_err(local::fs_mkdir(&ctl, &path))
        })?,
    )?;
    h.set(
        "fs_remove",
        lua.create_function({
            let ctl = ctl.clone();
            move |_lua, path: String| fs_err(local::fs_remove(&ctl, &path))
        })?,
    )?;
    h.set(
        "fs_copy",
        lua.create_function({
            let ctl = ctl.clone();
            move |_lua, (a, b): (String, String)| fs_err(local::fs_copy(&ctl, &a, &b))
        })?,
    )?;

    // ---- 存储
    h.set(
        "store_get",
        lua.create_function({
            let ctl = ctl.clone();
            move |lua, key: String| json::to_lua(lua, &local::store_get(&ctl, &key))
        })?,
    )?;
    h.set(
        "store_set",
        lua.create_function({
            let ctl = ctl.clone();
            move |_lua, (key, value): (String, Value)| {
                let v = json::to_json(&value);
                local::store_set(&ctl, &key, v).map_err(err)?;
                Ok(())
            }
        })?,
    )?;

    // ---- JSON
    h.set(
        "json_encode",
        lua.create_function(|_lua, value: Value| {
            let v = json::to_json(&value);
            serde_json::to_string(&v).map_err(|e| err(format!("JSON 编码失败: {e}")))
        })?,
    )?;
    h.set(
        "json_decode",
        lua.create_function(|lua, text: String| {
            let v: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| err(format!("JSON 解析失败: {e}")))?;
            json::to_lua(lua, &v)
        })?,
    )?;

    g.set("__host", h.clone())?;

    // 加载 Lua 侧封装
    let chunk = lua
        .load(PRELUDE)
        .set_name("@db-prelude")
        .set_environment(g.clone())
        .into_function()?;
    chunk.call::<()>(())?;
    // 插件不需要（也不应该）直接看到 __host
    g.set("__host", Value::Nil)?;
    Ok((g, h))
}
