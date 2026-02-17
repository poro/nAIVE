-- Tier 2.5 Demo: Collider Materials (Restitution & Friction)
-- Pyramid with increasing restitution from base to top.
-- Press 1 to release a wave of balls from above.
-- Balls accumulate — watch them bounce differently off each layer.

function init()
    self.ball_count = 0
    self.wave = 0

    -- Color pyramid layers by restitution: blue(dead) → green → yellow → red(super bouncy)
    -- Layer 1 (base) — r=0.0 — dark blue
    for i = 0, 4 do
        entity.set_base_color("pyr_base_" .. i, 0.1, 0.15, 0.5)
    end
    -- Layer 2 — r=0.3 — teal
    for i = 0, 3 do
        entity.set_base_color("pyr_mid1_" .. i, 0.1, 0.4, 0.4)
    end
    -- Layer 3 — r=0.6 — green
    for i = 0, 2 do
        entity.set_base_color("pyr_mid2_" .. i, 0.2, 0.6, 0.15)
    end
    -- Layer 4 — r=0.85 — orange
    entity.set_base_color("pyr_top1_0", 0.8, 0.5, 0.1)
    entity.set_base_color("pyr_top1_1", 0.8, 0.5, 0.1)
    -- Cap — r=1.0 — bright red
    entity.set_base_color("pyr_cap", 0.9, 0.15, 0.1)
    entity.set_emission("pyr_cap", 1.0, 0.2, 0.1)

    -- Floor
    entity.set_base_color("floor", 0.45, 0.45, 0.48)
end

function update(dt)
    -- Press 1: Release a wave of balls
    if input.just_pressed("slot1") then
        self.wave = self.wave + 1
        local count = 15
        for i = 1, count do
            self.ball_count = self.ball_count + 1
            local ox = -4 + (i - 1) * (8 / (count - 1))
            local oy = 12 + math.random() * 3
            local oz = -4 + (math.random() - 0.5) * 4
            entity.spawn_dynamic(
                "procedural:sphere",
                "assets/materials/default.yaml",
                ox, oy, oz,          -- position
                0, -0.5, 0,          -- velocity: gentle downward
                0.3,                 -- radius
                0.5,                 -- mass
                0.7,                 -- restitution (bouncy)
                0.3,                 -- friction
                60.0                 -- lifetime
            )
        end

        -- Particle burst for visual flair
        particles.spawn_burst(0, 12, -4, 40, {
            speed_min = 1, speed_max = 4,
            lifetime_min = 0.3, lifetime_max = 0.8,
            dir_y = -1, spread = 120,
            size_start = 0.15, size_end = 0.02,
            r = 1, g = 0.8, b = 0.3, a = 1,
            gravity_scale = 1,
        })
    end

    -- Press 2: Big wave (50 balls)
    if input.just_pressed("slot2") then
        self.wave = self.wave + 1
        local count = 50
        for i = 1, count do
            self.ball_count = self.ball_count + 1
            local ox = -5 + math.random() * 10
            local oy = 10 + math.random() * 6
            local oz = -6 + math.random() * 4
            entity.spawn_dynamic(
                "procedural:sphere",
                "assets/materials/default.yaml",
                ox, oy, oz,
                (math.random() - 0.5) * 0.5,
                -0.3,
                (math.random() - 0.5) * 0.5,
                0.3,                 -- radius
                0.5,                 -- mass
                0.7,                 -- restitution
                0.3,                 -- friction
                60.0                 -- lifetime
            )
        end
    end

    -- Draw HUD
    local sw = ui.screen_width()

    ui.text(sw * 0.5 - 130, 20, "MATERIAL PROPERTIES DEMO", 24, 1, 1, 1, 1)

    ui.rect(15, 55, 320, 95, 0, 0, 0, 0.6)
    ui.text(20, 60, "1: Drop 15 balls", 16, 0.8, 0.8, 0.8, 1)
    ui.text(20, 82, "2: Drop 50 balls", 16, 0.8, 0.8, 0.8, 1)
    ui.text(20, 104, string.format("Balls: %d  Waves: %d", self.ball_count, self.wave), 16, 0.3, 1.0, 0.3, 1)
    ui.text(20, 128, "Pyramid: blue(dead) -> red(super bouncy)", 12, 0.6, 0.6, 0.6, 1)

    -- Layer labels
    local labels = {
        { x = 0, y = 5.5, text = "r=1.0", r = 0.9, g = 0.2, b = 0.1 },
        { x = 0, y = 4.3, text = "r=0.85", r = 0.8, g = 0.5, b = 0.1 },
        { x = 0, y = 3.3, text = "r=0.6", r = 0.2, g = 0.6, b = 0.15 },
        { x = 0, y = 2.3, text = "r=0.3", r = 0.1, g = 0.4, b = 0.4 },
        { x = 0, y = 1.3, text = "r=0.0", r = 0.1, g = 0.15, b = 0.5 },
    }
    for _, l in ipairs(labels) do
        local sx, sy, vis = camera.world_to_screen(6, l.y, -4)
        if vis then
            ui.text(sx, sy, l.text, 12, l.r, l.g, l.b, 0.9)
        end
    end
end
