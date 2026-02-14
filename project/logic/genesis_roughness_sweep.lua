-- Live PBR material property animation: roughness ping-pongs over time

function init()
    self.time = 0
    self.speed = 0.3
end

function update(dt)
    self.time = self.time + dt
    -- Ping-pong roughness between 0.05 and 0.95
    local t = (math.sin(self.time * self.speed) + 1.0) / 2.0
    local roughness = 0.05 + t * 0.9
    entity.set_roughness(_entity_string_id, roughness)
end
