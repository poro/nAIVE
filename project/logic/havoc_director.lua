-- HAVOC waves 1 to 3: spawns each wave around the arena's edge, waits for
-- the swarm to be cleared, rests, then sends the next. The state lives in
-- `game` so the player's HUD (and the headless tests) can read it.
--
-- Events out: havoc.wave_started { wave, count }, havoc.wave_cleared { wave },
-- havoc.won, havoc.lost.

local SWARM = "goblins"
local WAVES = { 1000, 2500, 5000 }
local FIRST_WAVE_DELAY = 3
local RESPITE = 6
local HEAL_BETWEEN_WAVES = 40
local SPAWN_DISTANCE = 75   -- metres from the player
local ARENA = 92            -- spawn points stay inside this square

local function start_wave(n)
    local count = WAVES[n]
    game.wave = n
    game.state = "fighting"
    game.wave_size = count

    -- One group per direction; more directions as the waves grow.
    local px, _, pz = entity.get_position("player")
    local groups = 3 + n
    local left = count
    for k = 1, groups do
        local angle = (k / groups) * 2 * math.pi + n * 0.7
        local x = math.clamp(px + math.cos(angle) * SPAWN_DISTANCE, -ARENA, ARENA)
        local z = math.clamp(pz + math.sin(angle) * SPAWN_DISTANCE, -ARENA, ARENA)
        local size = (k == groups) and left or math.floor(count / groups)
        left = left - size
        swarm.spawn(SWARM, size, x, 0, z, 16)
    end
    events.emit("havoc.wave_started", { wave = n, count = count })
    log("Wave " .. n .. ": " .. count .. " goblins")
end

function init()
    game.wave = 0
    game.state = "starting"
    game.kills = 0
    game.wave_size = 0
    self.timer = FIRST_WAVE_DELAY

    events.on("swarm.kills", function(e)
        if e.data.swarm == SWARM then
            game.kills = game.kills + e.data.count
        end
    end)

    events.on("swarm.cleared", function(e)
        if e.data.swarm ~= SWARM or game.state ~= "fighting" then
            return
        end
        events.emit("havoc.wave_cleared", { wave = game.wave })
        entity.heal("player", HEAL_BETWEEN_WAVES)
        if game.wave >= #WAVES then
            game.state = "won"
            events.emit("havoc.won", {})
            log("All waves cleared")
        else
            game.state = "resting"
            self.timer = RESPITE
        end
    end)

    events.on("havoc.player_died", function()
        if game.state ~= "won" then
            game.state = "lost"
            events.emit("havoc.lost", { wave = game.wave })
        end
    end)
end

function update(dt)
    if game.state == "starting" or game.state == "resting" then
        self.timer = self.timer - dt
        game.next_wave_in = math.max(0, self.timer)
        if self.timer <= 0 then
            start_wave(game.wave + 1)
        end
    end
end
