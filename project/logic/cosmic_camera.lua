function init()
    self.time = 0
    self.radius = 6
    self.height = 2
    self.speed = 0.12
    -- Start ambient music with 3-second fade-in
    audio.play_music("assets/audio/cosmic_ambient.wav", 0.4, 3.0)
end

function update(dt)
    self.time = self.time + dt * self.speed
    local x = self.radius * math.cos(self.time)
    local z = self.radius * math.sin(self.time)
    entity.set_position(_entity_string_id, x, self.height, z)
    -- atan2(x, z) gives the yaw to rotate NEG_Z toward the origin
    local yaw = math.deg(math.atan2(x, z))
    local pitch = -math.deg(math.atan2(self.height, self.radius))
    entity.set_rotation(_entity_string_id, pitch, yaw, 0)
end
