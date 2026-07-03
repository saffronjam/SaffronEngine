local Writer = {}
function Writer.on_update(self, dt)
  if not self.entity:has_component("PointLight") then
    assert(self.entity:add_component("PointLight"), "add_component should succeed")
  end
  assert(self.entity:set_component("PointLight", { intensity = 5.0 }), "set_component should succeed")
  assert(self.entity:set_component("Rigidbody", { mass = 9 }) == false, "structural write must be refused")
  assert(self.entity:add_component("Collider") == false, "structural add must be refused")
  assert(self.entity:has_component("Transform"), "has_component(Transform)")
  assert(self.entity:has_component("Nope") == false, "has_component(unknown)")
end
return Writer
