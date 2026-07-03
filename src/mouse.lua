local Mouse = {}
function Mouse.on_update(self, dt)
  local p = sa.mouse_position()
  self.entity:set_position(sa.vec3(p.x, p.y, sa.is_mouse_down("left") and 1 or 0))
end
return Mouse
