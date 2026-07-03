local Walker = {}
function Walker.on_update(self, dt) self.entity:move_character(sa.vec3(3, 0, 0), false) end
return Walker
