local Player = {}
function Player.on_update(self, dt)
  if sa.is_key_down("w") then
    local p = self.entity:get_position()
    self.entity:set_position(p + sa.vec3(dt, 0, 0))
  end
end
return Player
