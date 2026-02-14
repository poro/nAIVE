-- Key lights: always bright and dramatic, slow color shifts

function init()
    self.time = math.random() * 6.28
    -- Start bright immediately
    entity.set_light(_entity_string_id, 15)
    entity.set_light_color(_entity_string_id, 1.0, 0.85, 0.6)
end

function update(dt)
    self.time = self.time + dt

    -- Dramatic pulsing between warm and cool
    local t = math.sin(self.time * 0.15) * 0.5 + 0.5
    local intensity = 12 + t * 13
    local r = 0.6 + 0.4 * t
    local g = 0.5 + 0.4 * t
    local b = 0.3 + 0.4 * (1.0 - t)
    entity.set_light(_entity_string_id, intensity)
    entity.set_light_color(_entity_string_id, r, g, b)
end
