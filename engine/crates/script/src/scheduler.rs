//! The Roblox-task-style coroutine scheduler: verbatim Luau installed onto the `sa` table after the
//! bindings are bound. `_sa_advance(dt)` resumes ready coroutines timed off accumulated `dt`, so
//! scheduling is deterministic and never reads a wall clock.
//!
//! `sa.wait`'s "am I inside a scheduler task?" guard cannot use `coroutine.running()`: mlua's Luau
//! backend runs every `Function::call` — a script's `on_update` included — on an auxiliary Lua
//! thread, so `ismain` reads `false` even outside a task. The prelude instead tracks the coroutine it
//! is currently resuming in `_sa_active` and yields only from that one, so a bare-`on_update`
//! `sa.wait` becomes the documented no-op rather than a "yield across a C-call boundary" error.

use mlua::Lua;

use crate::error::{Error, Result};

/// The scheduler prelude source.
const SCHEDULER_PRELUDE: &str = r#"
local _tasks, _accum, _sa_active = {}, 0, nil
rawset(sa, "spawn_task", function(fn, ...)
  local co = coroutine.create(fn)
  local prev = _sa_active
  _sa_active = co
  local ok, waitFor = coroutine.resume(co, ...)
  _sa_active = prev
  if not ok then sa.log("sa: task error: " .. tostring(waitFor))
  elseif coroutine.status(co) ~= "dead" then
    _tasks[#_tasks + 1] = { co = co, wake = _accum + (type(waitFor) == "number" and waitFor or 0) }
  end
  return co
end)
rawset(sa, "wait", function(seconds)
  local running = coroutine.running()
  if running ~= _sa_active or _sa_active == nil then
    sa.log("sa.wait called outside a coroutine is ignored")
    return
  end
  return coroutine.yield(seconds or 0)
end)
rawset(sa, "delay", function(seconds, fn)
  return sa.spawn_task(function() sa.wait(seconds) fn() end)
end)
function _sa_advance(dt)
  _accum = _accum + dt
  local ready, keep = {}, {}
  for _, t in ipairs(_tasks) do
    if t.wake <= _accum then ready[#ready + 1] = t else keep[#keep + 1] = t end
  end
  _tasks = keep
  for _, t in ipairs(ready) do
    local prev = _sa_active
    _sa_active = t.co
    local ok, waitFor = coroutine.resume(t.co)
    _sa_active = prev
    if not ok then sa.log("sa: coroutine error: " .. tostring(waitFor))
    elseif coroutine.status(t.co) ~= "dead" then
      _tasks[#_tasks + 1] = { co = t.co, wake = _accum + (type(waitFor) == "number" and waitFor or 0) }
    end
  end
end
"#;

/// Installs the scheduler prelude (`sa.spawn_task`/`sa.wait`/`sa.delay` + the global
/// `_sa_advance`) onto the already-bound `sa` table.
///
/// Run once per session after [`crate::register_no_scene_globals`] and the scene
/// bindings, so the prelude's `rawset(sa, …)` lands on the live `sa` table. A failure
/// (a missing `sa` global, a Luau error) is surfaced as [`Error::Runtime`] for the
/// caller to log.
pub fn install(lua: &Lua) -> Result<()> {
    lua.load(SCHEDULER_PRELUDE)
        .set_name("sa:scheduler")
        .exec()
        .map_err(|e| Error::Runtime(e.to_string()))
}
