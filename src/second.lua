local Second = {}
function Second.on_update(self, dt)
  local p = self.entity:get_position()
  self.entity:set_position(sa.vec3(p.x, p.x * 2, p.z))
end
return Second
