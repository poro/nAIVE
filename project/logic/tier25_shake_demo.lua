-- Tier 2.5 Demo: Camera Shake
-- Press 1/2/3 for light/medium/heavy shake with explosion VFX on the barrels.

local barrels = { "barrel_1", "barrel_2", "barrel_3" }
local shake_configs = {
    { intensity = 0.3, duration = 0.5, label = "Light",  impulse = 8  },
    { intensity = 0.7, duration = 1.0, label = "Medium", impulse = 15 },
    { intensity = 1.5, duration = 1.5, label = "Heavy",  impulse = 25 },
}
local slot_keys = { "slot1", "slot2", "slot3" }

function init()
    self.last_shake = ""
    self.shake_timer = 0

    -- Color barrels red/orange to look like explosives
    for i, bid in ipairs(barrels) do
        entity.set_base_color(bid, 0.8, 0.15, 0.05)
        entity.set_emission(bid, 0.4, 0.05, 0.0)
        entity.set_roughness(bid, 0.7)
    end
end

function update(dt)
    if self.shake_timer > 0 then
        self.shake_timer = self.shake_timer - dt
    end

    for i = 1, 3 do
        if input.just_pressed(slot_keys[i]) then
            local cfg = shake_configs[i]

            -- Camera shake
            camera.shake(cfg.intensity, cfg.duration)

            -- Launch the corresponding barrel upward
            local bid = barrels[i]
            if entity.exists(bid) then
                physics.apply_impulse(bid, 0, cfg.impulse, 0)

                -- Explosion particle burst at barrel position
                local bx, by, bz = entity.get_position(bid)
                particles.spawn_burst(bx, by, bz, 30 + i * 20, {
                    speed_min = 2, speed_max = 6 + i * 2,
                    lifetime_min = 0.2, lifetime_max = 0.6,
                    dir_y = 1, spread = 180,
                    size_start = 0.25, size_end = 0.03,
                    r = 1.0, g = 0.5, b = 0.1, a = 1,
                    r_end = 1.0, g_end = 0.1, b_end = 0.0,
                    gravity_scale = -0.3,
                })

                -- Screen flash
                local flash_a = 0.1 + i * 0.1
                ui.flash(1.0, 0.6, 0.1, flash_a, 0.2)
            end

            self.last_shake = cfg.label
            self.shake_timer = cfg.duration
        end
    end

    -- Draw HUD
    local sw = ui.screen_width()
    local sh = ui.screen_height()

    ui.text(sw * 0.5 - 110, 20, "CAMERA SHAKE DEMO", 24, 1, 1, 1, 1)

    ui.rect(15, 55, 310, 100, 0, 0, 0, 0.6)
    ui.text(20, 60,  "1: Light shake   (0.3, 0.5s)", 16, 0.6, 1.0, 0.6, 1)
    ui.text(20, 82,  "2: Medium shake  (0.7, 1.0s)", 16, 1.0, 1.0, 0.4, 1)
    ui.text(20, 104, "3: Heavy shake   (1.5, 1.5s)", 16, 1.0, 0.4, 0.4, 1)
    ui.text(20, 130, "Barrels launch on detonation!", 14, 0.6, 0.6, 0.6, 1)

    -- Active shake indicator
    if self.shake_timer > 0 then
        ui.rect(sw - 200, 55, 180, 35, 0, 0, 0, 0.6)
        ui.text(sw - 190, 62, "SHAKING: " .. self.last_shake, 18, 1.0, 0.5, 0.1, 1)
    end

    -- Barrel labels
    for i, bid in ipairs(barrels) do
        if entity.exists(bid) then
            local bx, by, bz = entity.get_position(bid)
            local sx, sy, vis = camera.world_to_screen(bx, by + 1.5, bz)
            if vis then
                ui.text(sx - 25, sy, shake_configs[i].label, 14, 1, 0.7, 0.3, 0.9)
            end
        end
    end
end
