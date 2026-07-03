local Life = {}
function Life.on_update(self, dt)
  if self.done then return end
  self.done = true
  local a = sa.spawn("Alpha")
  local b = sa.spawn("Beta")
  assert(b:set_parent(a), "set_parent should succeed")
  assert(b:set_parent(b) == false, "self-parent must fail")
  assert(b:parent():uuid() == a:uuid(), "b's parent is a")
  local kids = a:children()
  assert(#kids == 1, "a has one child")
  assert(kids[1]:uuid() == b:uuid(), "a's child is b")
  assert(#sa.find_all_by_name("Alpha") >= 1, "find_all_by_name finds Alpha")
  assert(sa.find_by_uuid(a:uuid()):uuid() == a:uuid(), "find_by_uuid round-trips")
end
return Life
