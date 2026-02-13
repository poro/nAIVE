-- Relic: bobs and rotates, collected when player interacts nearby

function init()
    self.collected = false
    self.interact_range = 2.0
    self.bob_time = 0
    self.base_y = 1.2
    log("Relic awaits...")
end

function update(dt)
    if self.collected or game.game_over then return end

    -- Bob up and down
    self.bob_time = self.bob_time + dt
    local bob_y = self.base_y + math.sin(self.bob_time * 2.0) * 0.15
    local rx, _, rz = entity.get_position("relic")
    entity.set_position("relic", rx, bob_y, rz)

    -- Slow rotation
    local angle = (self.bob_time * 30) % 360
    entity.set_rotation("relic", 0, angle, 0)

    -- Pickup check
    local px, py, pz = entity.get_position("player")
    local dist = math.sqrt((px - rx)^2 + (py - bob_y)^2 + (pz - rz)^2)

    if dist < self.interact_range and input.just_pressed("interact") then
        self.collected = true
        game.level_complete = true
        events.emit("item.collected", {item_id = "relic", item_type = "relic"})
        events.emit("game.level_complete", {})
        entity.set_position("relic", 0, -100, 0)
        log("=== RELIC COLLECTED! LEVEL COMPLETE! ===")
    end
end
