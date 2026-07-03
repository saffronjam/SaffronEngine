local Caster = {}
function Caster.on_update(self, dt)
  if not self.done then
    self.done = true
    local hit = sa.spherecast(0, 5, 0, 0, -1, 0, 0.5, 20)
    if hit.hit then self.entity:set_position(sa.vec3(1, hit.point.y, 0)) end
  end
end
return Caster
