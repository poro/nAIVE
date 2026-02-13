-- Torch flicker: oscillates point_light intensity for atmosphere

function init()
    self.base_intensity = 8.0
    self.time = math.random() * 10 -- random phase offset per torch
    self.speed = 3.0 + math.random() * 2.0
    self.amount = 0.3
end

function update(dt)
    self.time = self.time + dt * self.speed

    -- Layered sine waves for organic flicker
    local f = math.sin(self.time) * 0.5
            + math.sin(self.time * 2.3) * 0.3
            + math.sin(self.time * 5.7) * 0.2

    local intensity = self.base_intensity + f * self.base_intensity * self.amount
    entity.set_light(_entity_string_id, math.max(intensity, 0.5))
end
