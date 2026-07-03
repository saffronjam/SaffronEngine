local Logger = {}
function Logger:on_create()
  sa.log("hello from " .. self.entity:name())
end
function Logger:on_update(dt) end
return Logger
