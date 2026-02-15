-- combat_enemy.lua — Enemy entity script for the combat demo.
-- Demonstrates on_damage and on_death callbacks.
-- Flashes red when hit, destroys self on death.

function init()
    self.flash_timer = 0
    self.original_r = 0.8
    self.original_g = 0.2
    self.original_b = 0.2
end

function update(dt)
    -- Flash effect when damaged
    if self.flash_timer > 0 then
        self.flash_timer = self.flash_timer - dt
        -- Flash white then fade back to red
        local t = self.flash_timer / 0.15
        local r = math.lerp(self.original_r, 1.0, t)
        local g = math.lerp(self.original_g, 1.0, t)
        local b = math.lerp(self.original_b, 1.0, t)
        entity.set_base_color(_entity_string_id, r, g, b)
        entity.set_emission(_entity_string_id, r * 0.5, g * 0.5, b * 0.5)
    end

    -- Show health bar above enemy
    local px, py, pz = entity.get_position(_entity_string_id)
    local sx, sy, visible = camera.world_to_screen(px, py + 1.5, pz)
    if visible then
        local hp, max_hp = entity.get_health(_entity_string_id)
        if hp < max_hp then
            local bar_w = 60
            local fill = (hp / max_hp) * bar_w
            ui.rect(sx - bar_w/2, sy - 10, bar_w, 6, 0.2, 0.2, 0.2, 0.7)
            ui.rect(sx - bar_w/2, sy - 10, fill, 6, 0.9, 0.2, 0.2, 0.9)
        end
    end
end

function on_damage(amount, source_id)
    self.flash_timer = 0.15
    local hp, max_hp = entity.get_health(_entity_string_id)
    log(_entity_string_id .. " hit by " .. source_id .. " for " .. amount .. " (hp=" .. hp .. "/" .. max_hp .. ")")
end

function on_death()
    log(_entity_string_id .. " destroyed!")
    entity.destroy(_entity_string_id)
end
