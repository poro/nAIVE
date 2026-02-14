-- VOID Camera: extremely slow, majestic orbit through space
function init()
    self.time = 0
    self.total_duration = 90.0
    self.keyframes = {
        {t=0,   x=20,  y=5,  z=20,   pitch=-10, yaw=225},
        {t=10,  x=30,  y=3,  z=0,    pitch=-3,  yaw=90},
        {t=20,  x=15,  y=12, z=-25,  pitch=-20, yaw=150},
        {t=30,  x=0,   y=25, z=0.1,  pitch=-85, yaw=0},
        {t=40,  x=-25, y=4,  z=15,   pitch=-5,  yaw=305},
        {t=50,  x=5,   y=2,  z=5,    pitch=-3,  yaw=225},
        {t=60,  x=-18, y=15, z=-20,  pitch=-30, yaw=30},
        {t=70,  x=22,  y=1,  z=8,    pitch=5,   yaw=110},
        {t=80,  x=0,   y=10, z=25,   pitch=-15, yaw=180},
        {t=90,  x=20,  y=5,  z=20,   pitch=-10, yaw=225},
    }
end

function update(dt)
    self.time = self.time + dt
    if self.time >= self.total_duration then
        self.time = self.time - self.total_duration
    end
    local kf = self.keyframes
    local a, b = kf[1], kf[2]
    for i = 1, #kf - 1 do
        if self.time >= kf[i].t and self.time < kf[i+1].t then
            a = kf[i]
            b = kf[i+1]
            break
        end
    end
    local span = b.t - a.t
    local raw_t = 0
    if span > 0 then raw_t = (self.time - a.t) / span end
    local t = raw_t * raw_t * (3 - 2 * raw_t)
    local x = a.x + (b.x - a.x) * t
    local y = a.y + (b.y - a.y) * t
    local z = a.z + (b.z - a.z) * t
    local pitch = a.pitch + (b.pitch - a.pitch) * t
    local yaw = a.yaw + (b.yaw - a.yaw) * t
    entity.set_position(_entity_string_id, x, y, z)
    entity.set_rotation(_entity_string_id, pitch, yaw, 0)
end
