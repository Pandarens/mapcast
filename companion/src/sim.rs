//! Runs the real addon Lua against a mock WoW API and renders its strip to pixels.
//!
//! Used by the tests and by `mapcast --sim` to try the whole chain without the game. The mock
//! (sim/wowmock.lua) and the fake map addons (sim/mapaddons.lua) are built into the program,
//! and so is the addon itself.

use crate::capture::{Source, DEFAULT_REGION};
use crate::protocol::Frame;
use mlua::{Function, Lua, Table, Value as LuaValue};
use std::time::Instant;

const WOWMOCK: &str = include_str!("../sim/wowmock.lua");
const MAPADDONS: &str = include_str!("../sim/mapaddons.lua");
pub const ADDON_FILES: [(&str, &str); 4] = [
    ("Protocol.lua", include_str!("../../addon/MapCast/Protocol.lua")),
    ("Strip.lua", include_str!("../../addon/MapCast/Strip.lua")),
    ("MapCast.lua", include_str!("../../addon/MapCast/MapCast.lua")),
    ("Pins.lua", include_str!("../../addon/MapCast/Pins.lua")),
];

pub struct SimGame {
    pub lua: Lua,
}

#[cfg_attr(not(test), allow(dead_code))]
impl SimGame {
    pub fn new(openmap: bool, map_addons: bool) -> mlua::Result<SimGame> {
        Self::with_size((1920, 1080), openmap, map_addons)
    }

    pub fn with_size(phys: (u32, u32), openmap: bool, map_addons: bool) -> mlua::Result<SimGame> {
        let lua = Lua::new(); // with LuaJIT's bit library, as the game's Lua has it
        lua.load(WOWMOCK).set_name("@wowmock.lua").exec()?;
        let mock: Table = lua.globals().get("MockWoW")?;
        mock.set("physW", phys.0)?;
        mock.set("physH", phys.1)?;
        if !openmap {
            lua.load("OpenMapAPI = nil").exec()?;
        }
        if map_addons {
            // HandyNotes, TomTom, GatherMate2, Questie, RestedXP: loaded before MapCast, as in the game
            lua.load(MAPADDONS).set_name("@mapaddons.lua").exec()?;
        }
        let ns = lua.create_table()?;
        for (name, src) in ADDON_FILES {
            let chunk: Function = lua.load(src).set_name(format!("@{name}")).into_function()?;
            chunk.call::<()>(("MapCast", ns.clone()))?;
        }
        lua.globals().set("MapCastNS", ns)?;
        let sim = SimGame { lua };
        sim.fire("ADDON_LOADED", Some("MapCast"))?;
        sim.fire("PLAYER_ENTERING_WORLD", None)?;
        Ok(sim)
    }

    pub fn mock(&self) -> Table {
        self.lua.globals().get("MockWoW").expect("MockWoW")
    }

    pub fn fire(&self, event: &str, arg: Option<&str>) -> mlua::Result<()> {
        let fire: Function = self.mock().get("fire")?;
        match arg {
            Some(a) => fire.call::<()>((event, a)),
            None => fire.call::<()>(event),
        }
    }

    pub fn exec(&self, code: &str) -> mlua::Result<()> {
        self.lua.load(code).exec()
    }

    pub fn eval<T: mlua::FromLua>(&self, expr: &str) -> mlua::Result<T> {
        self.lua.load(format!("return {expr}")).eval()
    }

    pub fn step(&self, dt: f64) {
        if let Ok(step) = self.mock().get::<Function>("step") {
            if let Err(e) = step.call::<()>(dt) {
                log::warn!("simulator: {e}");
            }
        }
    }

    pub fn slash(&self, text: &str) -> mlua::Result<()> {
        let list: Table = self.lua.globals().get("SlashCmdList")?;
        let f: Function = list.get("MAPCAST")?;
        f.call::<()>(text)
    }

    pub fn tick(&self) -> mlua::Result<()> {
        let ns: Table = self.lua.globals().get("MapCastNS")?;
        ns.get::<Function>("Tick")?.call::<()>(())
    }

    /// The top-left screen corner as RGB, with the strip drawn the way the GPU would.
    pub fn render(&self, width: usize, height: usize) -> Frame {
        let mut frame = Frame::new(width, height);
        for px in frame.rgb.chunks_exact_mut(3) {
            px.copy_from_slice(&[40, 60, 40]); // "game world" behind the strip
        }
        let mock = self.mock();
        let phys_h: f64 = mock.get("physH").unwrap_or(1080.0);
        let scale = phys_h / 768.0;
        let Ok(rects) = mock.get::<Function>("rects").and_then(|f| f.call::<Table>(())) else { return frame };
        for rect in rects.sequence_values::<Table>().flatten() {
            let v: Vec<f64> = (1..=7).map(|i| match rect.get::<LuaValue>(i) {
                Ok(LuaValue::Number(n)) => n,
                Ok(LuaValue::Integer(n)) => n as f64,
                _ => 0.0,
            }).collect();
            let (x0, y0) = ((v[0] * scale).round() as i64, (v[1] * scale).round() as i64);
            let (x1, y1) = (((v[0] + v[2]) * scale).round() as i64, ((v[1] + v[3]) * scale).round() as i64);
            let color = [(v[4] * 255.0).round() as u8, (v[5] * 255.0).round() as u8, (v[6] * 255.0).round() as u8];
            for y in y0.max(0)..y1.min(height as i64) {
                for x in x0.max(0)..x1.min(width as i64) {
                    let i = (y as usize * width + x as usize) * 3;
                    frame.rgb[i..i + 3].copy_from_slice(&color);
                }
            }
        }
        frame
    }
}

/// Drop-in replacement for the screen capture, driven by SimGame.
pub struct SimSource {
    game: SimGame,
    region: (usize, usize),
    last: Instant,
}

// The Lua state is only used from the capture thread it is moved to.
unsafe impl Send for SimSource {}

impl SimSource {
    pub fn new() -> mlua::Result<SimSource> {
        Ok(SimSource { game: SimGame::new(true, true)?, region: DEFAULT_REGION, last: Instant::now() })
    }
}

impl Source for SimSource {
    fn grab(&mut self) -> Option<Frame> {
        let now = Instant::now();
        self.game.step(now.duration_since(self.last).as_secs_f64());
        self.last = now;
        Some(self.game.render(self.region.0, self.region.1))
    }

    fn resize(&mut self, width: usize, height: usize) {
        self.region = (width.max(16), height.max(8));
    }

    fn window_found(&self) -> bool {
        true
    }
}
