function init()
    self.time = math.random() * 6.28
    self.base_intensity = 8.0
    self.pulse_speed = 1.5 + math.random() * 1.0
    self.pulse_amount = 0.5
end

function update(dt)
    self.time = self.time + dt * self.pulse_speed
    local pulse = math.sin(self.time) * 0.5 + 0.5
    local intensity = self.base_intensity * (1.0 - self.pulse_amount + pulse * self.pulse_amount)
    entity.set_light(_entity_string_id, intensity)
end
