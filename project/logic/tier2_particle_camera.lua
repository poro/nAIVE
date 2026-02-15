-- tier2_particle_camera.lua — Orbiting camera for the particle system demo.
-- Slowly circles the particle emitters. Press [Space] for burst effects.

function init()
    self.time = 0
    self.radius = 14
    self.height = 6
    self.speed = 0.12
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

    -- Look at origin
    local yaw = math.deg(angle + math.pi)
    entity.set_rotation(_entity_string_id, -15, yaw, 0)

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
    local sw = ui.screen_width()
    ui.text(20, 20, "TIER 2: PARTICLE SYSTEM", 28, 1, 0.6, 0.2, 1)
    ui.text(20, 52, "5 emitter types: fire, magic, poison, sparkle, void", 14, 0.8, 0.8, 0.8, 1)
    ui.text(20, 70, "[Space] = burst particles on all emitters", 14, 0.6, 0.6, 0.6, 1)
end
