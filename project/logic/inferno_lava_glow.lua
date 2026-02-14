-- Lava pit pulsing emission
function init()
    self.time = math.random() * 6.28
end

function update(dt)
    self.time = self.time + dt * 0.8
    local pulse = math.sin(self.time) * 0.3 + 0.7
    entity.set_emission(_entity_string_id, 4.0 * pulse, 1.0 * pulse, 0.1 * pulse)
end
