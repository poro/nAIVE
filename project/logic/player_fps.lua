-- Player FPS controller script: health, interaction, death

function init()
    game.player_health = 100
    game.game_over = false
    game.level_complete = false
    self.invuln_timer = 0
    log("Player ready - 100 HP")
end

function update(dt)
    if game.game_over or game.level_complete then return end

    -- Invulnerability cooldown
    if self.invuln_timer > 0 then
        self.invuln_timer = self.invuln_timer - dt
    end

    -- Interact (E key) - nearby objects handle their own logic
    if input.just_pressed("interact") then
        events.emit("player.interacted", {})
    end

    -- Attack (left click)
    if input.just_pressed("attack") then
        events.emit("player.attacked", {})
    end

    -- Log health periodically
    if self.log_timer == nil then self.log_timer = 0 end
    self.log_timer = self.log_timer + dt
    if self.log_timer > 10 then
        self.log_timer = 0
        log("Player HP: " .. game.player_health)
    end
end
