local VecTest = {}
function VecTest.on_update(self, dt)
  local a = sa.vec3(1, 2, 3)
  assert((a + sa.vec3(0, 1, 0)).y == 3, "add")
  assert((a - sa.vec3(0, 1, 0)).y == 1, "sub")
  assert((a * 2).x == 2, "vec*scalar")
  assert((2 * a).z == 6, "scalar*vec")
  assert(math.abs(sa.vec3(3, 0, 0):length() - 3) < 1e-4, "length")
  assert(a:dot(sa.vec3(1, 0, 0)) == 1, "dot")
  assert(sa.vec3(1, 0, 0):cross(sa.vec3(0, 1, 0)).z == 1, "cross")
  local p = self.entity:get_position()
  p.x = 7
  self.entity:set_position(p)
end
return VecTest
