-- Dramatic light with slow breathing pulse
function init()
    self.time = math.random() * 6.28
    self.base_intensity = 6.0
    self.pulse_speed = 0.5 + math.random() * 0.5
end

function update(dt)
    self.time = self.time + dt
    local pulse = math.sin(self.time * self.pulse_speed) * 0.5 + 0.5
    local intensity = self.base_intensity * (0.5 + pulse * 0.5)
    entity.set_light(_entity_string_id, intensity)
end
