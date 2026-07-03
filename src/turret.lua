local Turret = {}
Turret.properties = {
  speed = 2.0,
  label = "idle",
  enabled = true,
  offset = sa.vec3(0, 1, 0),
  weird = { 1, 2 },
}
function Turret.on_update(self, dt)
  assert(self.label == "idle" or self.label == "fast", "label: " .. tostring(self.label))
  assert(type(self.enabled) == "boolean", "enabled must be a bool")
  assert(self.offset.y == 1, "offset must inject as an sa.Vec3")
  if self.enabled then
    local p = self.entity:get_position()
    self.entity:set_position(p + sa.vec3(self.speed * dt, 0, 0))
  end
end
return Turret
