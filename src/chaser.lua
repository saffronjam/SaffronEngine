local Chaser = {}
function Chaser.on_update(self, dt)
  assert(not sa.get_entity_by_name("No Such Entity"):valid(), "missing lookup must be invalid")
  local target = sa.get_entity_by_name("Target")
  if target:valid() then
    local p = target:get_position()
    target:set_position(p + sa.vec3(0, 0, dt))
  end
end
return Chaser
