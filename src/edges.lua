local Edges = {}
function Edges.on_create(self) self.count = 0 end
function Edges.on_update(self, dt)
  if sa.is_key_pressed("e") then
    self.count = self.count + 1
    self.entity:set_position(sa.vec3(self.count, 0, 0))
  end
end
return Edges
