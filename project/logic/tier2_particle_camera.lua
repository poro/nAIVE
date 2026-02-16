-- tier2_particle_camera.lua — Orbiting camera for the particle system demo.
-- Slowly circles the particle emitters. Press [Space] for burst effects.

function init()
    self.time = 0
    self.radius = 10
    self.target_y = 1.0  -- look at emitter height
    self.height = 3.0
    self.speed = 0.1
    self.burst_cooldown = 0
end

function update(dt)
    self.time = self.time + dt
    self.burst_cooldown = math.max(0, self.burst_cooldown - dt)

    -- Orbit around origin
    local angle = self.time * self.speed
    local x = math.sin(angle) * self.radius
    local z = math.cos(angle) * self.radius
    entity.set_position(_entity_string_id, x, self.height, z)

    -- Look at emitter center (same pattern as gallery_camera / cosmic_camera)
    -- atan2(x, z) gives the yaw to rotate NEG_Z toward the origin
    local yaw = math.deg(math.atan2(x, z))
    local pitch = -math.deg(math.atan2(self.height - self.target_y, self.radius))
    entity.set_rotation(_entity_string_id, pitch, yaw, 0)

    -- [Space] burst particles on all emitters
    if input.just_pressed("jump") and self.burst_cooldown <= 0 then
        entity.burst("fire_emitter", 50)
        entity.burst("magic_fountain", 40)
        entity.burst("poison_cloud", 30)
        entity.burst("sparkle_trail", 60)
        entity.burst("void_rift", 45)
        self.burst_cooldown = 0.5
        log("[particles] Burst on all emitters!")
    end

    -- HUD
    ui.text(20, 20, "TIER 2: PARTICLE SYSTEM", 28, 1, 0.6, 0.2, 1)
    ui.text(20, 52, "5 emitter types: fire, magic, poison, sparkle, void", 14, 0.8, 0.8, 0.8, 1)
    ui.text(20, 70, "[Space] = burst particles (CPU sim active, GPU billboard TBD)", 14, 0.6, 0.6, 0.6, 1)
end
