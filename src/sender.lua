local Sender = {}
function Sender.on_create(self)
  sa.broadcast("boom")        -- faulting handler, contained
  sa.broadcast("ping", 7)     -- still delivered after the boom
end
function Sender.on_update(self, dt) end
return Sender
