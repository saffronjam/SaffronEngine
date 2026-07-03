local Destroyer = {}
function Destroyer.on_update(self, dt)
  if not self.spawned then
    self.spawned = sa.spawn("Doomed")
  elseif not self.killed then
    assert(self.spawned:valid(), "spawned must be valid")
    self.spawned:destroy()
    assert(self.spawned:valid(), "destroy is deferred; valid until flush")
    self.killed = true
  else
    assert(not self.spawned:valid(), "after flush the entity is invalid")
  end
end
return Destroyer
