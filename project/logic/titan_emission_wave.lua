-- Cycles emission color through a rainbow wave pattern
function init()
    self.time = math.random() * 6.28
    self.speed = 0.4 + math.random() * 0.3
end

function update(dt)
    self.time = self.time + dt * self.speed
    local r = math.sin(self.time) * 0.5 + 0.5
    local g = math.sin(self.time + 2.094) * 0.5 + 0.5
    local b = math.sin(self.time + 4.189) * 0.5 + 0.5
    entity.set_emission(_entity_string_id, r * 2.0, g * 2.0, b * 2.0)
end
