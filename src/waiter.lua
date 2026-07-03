local Waiter = {}
function Waiter.on_create(self)
  sa.spawn_task(function()
    sa.wait(0.5)
    self.entity:set_position(sa.vec3(42, 0, 0))
  end)
end
function Waiter.on_update(self, dt)
  sa.wait(0.1)  -- outside a coroutine: logged + ignored, never a tick error
end
return Waiter
