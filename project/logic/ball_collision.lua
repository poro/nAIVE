function init()
    self.cooldown = 0
end

function update(dt)
    if self.cooldown > 0 then
        self.cooldown = self.cooldown - dt
    end
end

function on_collision(other_id)
    if self.cooldown and self.cooldown > 0 then
        return
    end
    self.cooldown = 0.1
    local vol = 0.3 + math.random() * 0.4
    audio.play_sfx("hit_" .. tostring(math.random(1000)), "assets/audio/collision.wav", vol)
end
