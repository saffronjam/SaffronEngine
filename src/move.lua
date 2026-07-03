local Mover = {}
function Mover.on_update(self, dt)
  local p = self.entity:get_position()
  self.entity:set_position(p + sa.vec3(dt, 0, 0))
end
return Mover
