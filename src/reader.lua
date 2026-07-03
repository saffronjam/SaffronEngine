local Reader = {}
function Reader.on_update(self, dt)
  assert(self.entity:valid(), "self.entity must be valid")
  assert(self.entity:name() == "Reader Cube", "name() mismatch: " .. self.entity:name())
  assert(self.entity:get_component("NoSuchComponent") == nil, "unknown component must be nil")
  local t = self.entity:get_component("Transform")
  assert(t ~= nil, "Transform snapshot missing")
  self.entity:set_position(sa.vec3(t.translation.z * 2, 50, t.translation.z))
  self.entity:set_rotation(sa.vec3(0.5, 0, 0))
  self.entity:set_scale(sa.vec3(2, 2, 2))
end
return Reader
