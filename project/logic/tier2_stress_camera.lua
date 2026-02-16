-- tier2_stress_camera.lua — Orbiting camera for the stress test.
-- Slowly rotates around the origin to showcase 300+ entities.

function init()
    self.time = 0
    self.radius = 30
    self.height = 15
    self.speed = 0.15
end

function update(dt)
    self.time = self.time + dt
    local angle = self.time * self.speed
    local x = math.sin(angle) * self.radius
    local z = math.cos(angle) * self.radius
    entity.set_position(_entity_string_id, x, self.height, z)

    -- Look at origin (atan2(x,z) pattern from gallery/cosmic cameras)
    local yaw = math.deg(math.atan2(x, z))
    local pitch = -math.deg(math.atan2(self.height, self.radius))
    entity.set_rotation(_entity_string_id, pitch, yaw, 0)

    -- HUD
    ui.text(20, 20, "TIER 2 STRESS TEST", 28, 1, 0.6, 0.2, 1)
    ui.text(20, 52, "Dynamic GPU buffer: 300+ entities, no crash", 14, 0.8, 0.8, 0.8, 1)
    ui.text(20, 70, "Old limit was 256 — entity #257 would panic", 14, 0.6, 0.6, 0.6, 1)
end
