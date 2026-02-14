-- Inferno arena player: health, combat, wave tracking
function init()
    game.player_health = 100
    game.game_over = false
    game.level_complete = false
    game.wave = 1
    game.enemies_alive = 6
    game.kills = 0
    self.invuln_timer = 0
    log("INFERNO: Wave 1 - Fight!")
end

function update(dt)
    if game.game_over then return end
    if game.level_complete then return end

    if self.invuln_timer > 0 then
        self.invuln_timer = self.invuln_timer - dt
    end

    if input.just_pressed("interact") then
        events.emit("player.interacted", {})
    end

    if input.just_pressed("attack") then
        events.emit("player.attacked", {})
    end

    if game.enemies_alive <= 0 then
        if game.wave >= 3 then
            game.level_complete = true
            log("INFERNO COMPLETE! Total kills: " .. game.kills)
            events.emit("level.complete", {kills = game.kills})
        else
            game.wave = game.wave + 1
            game.enemies_alive = 6
            log("Wave " .. game.wave .. " incoming!")
            events.emit("wave.start", {wave = game.wave})
        end
    end
end
