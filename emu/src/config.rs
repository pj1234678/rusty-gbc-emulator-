use std::collections::HashMap;
use std::fs;
use std::path::Path;

use sdl2::keyboard::Keycode;
use sdl2::controller::Button;

pub struct Config {
    pub key_a: Keycode,
    pub key_b: Keycode,
    pub key_select: Keycode,
    pub key_start: Keycode,
    pub key_right: Keycode,
    pub key_left: Keycode,
    pub key_up: Keycode,
    pub key_down: Keycode,
    pub key_r: Keycode,
    pub key_l: Keycode,
    pub key_ff: Keycode,
    pub ctrl_a: Button,
    pub ctrl_b: Button,
    pub ctrl_select: Button,
    pub ctrl_start: Button,
    pub ctrl_right: Button,
    pub ctrl_left: Button,
    pub ctrl_up: Button,
    pub ctrl_down: Button,
    pub ctrl_r: Button,
    pub ctrl_l: Button,
    pub ctrl_ff: Button,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            key_a: Keycode::Z,
            key_b: Keycode::X,
            key_select: Keycode::RShift,
            key_start: Keycode::Return,
            key_right: Keycode::Right,
            key_left: Keycode::Left,
            key_up: Keycode::Up,
            key_down: Keycode::Down,
            key_r: Keycode::S,
            key_l: Keycode::A,
            key_ff: Keycode::Tab,
            ctrl_a: Button::B,
            ctrl_b: Button::A,
            ctrl_select: Button::Back,
            ctrl_start: Button::Start,
            ctrl_right: Button::DPadRight,
            ctrl_left: Button::DPadLeft,
            ctrl_up: Button::DPadUp,
            ctrl_down: Button::DPadDown,
            ctrl_r: Button::RightShoulder,
            ctrl_l: Button::LeftShoulder,
            ctrl_ff: Button::Y,
        }
    }
}

fn string_to_controller_button(s: &str) -> Option<Button> {
    match s {
        "A" => Some(Button::A),
        "B" => Some(Button::B),
        "X" => Some(Button::X),
        "Y" => Some(Button::Y),
        "Back" => Some(Button::Back),
        "Start" => Some(Button::Start),
        "Guide" => Some(Button::Guide),
        "LeftShoulder" => Some(Button::LeftShoulder),
        "RightShoulder" => Some(Button::RightShoulder),
        "LeftStick" => Some(Button::LeftStick),
        "RightStick" => Some(Button::RightStick),
        "DPadUp" => Some(Button::DPadUp),
        "DPadDown" => Some(Button::DPadDown),
        "DPadLeft" => Some(Button::DPadLeft),
        "DPadRight" => Some(Button::DPadRight),
        "Misc1" => Some(Button::Misc1),
        "Paddle1" => Some(Button::Paddle1),
        "Paddle2" => Some(Button::Paddle2),
        "Paddle3" => Some(Button::Paddle3),
        "Paddle4" => Some(Button::Paddle4),
        "Touchpad" => Some(Button::Touchpad),
        _ => None,
    }
}

impl Config {
    fn default_config_text() -> &'static str {
        r#"# GBC Emulator Input Configuration
# Uncomment and edit lines to change button bindings.
# Keyboard mappings (SDL2 scancode names)
#   key_a       = Z
#   key_b       = X
#   key_select  = RShift
#   key_start   = Return
#   key_right   = Right
#   key_left    = Left
#   key_up      = Up
#   key_down    = Down
#   key_r       = S
#   key_l       = A
#
# Controller mappings (SDL2 controller button names)
#   ctrl_a       = B
#   ctrl_b       = A
#   ctrl_select  = Back
#   ctrl_start   = Start
#   ctrl_right   = DPadRight
#   ctrl_left    = DPadLeft
#   ctrl_up      = DPadUp
#   ctrl_down    = DPadDown
#   ctrl_r       = RightShoulder
#   ctrl_l       = LeftShoulder
#
# Fast Forward key/button (held to speed up emulation)
#   key_ff      = Tab
#   ctrl_ff     = Y
"#
    }

    pub fn load() -> Self {
        let path = Path::new("gbc.cfg");
        if !path.exists() {
            if let Err(e) = fs::write(path, Self::default_config_text()) {
                eprintln!("Warning: Could not create gbc.cfg: {}", e);
            }
            return Self::default();
        }

        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => {
                eprintln!("Warning: Could not read gbc.cfg, using defaults");
                return Self::default();
            }
        };

        let mut map = HashMap::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(eq) = line.find('=') {
                let key = line[..eq].trim().to_string();
                let value = line[eq + 1..].trim().to_string();
                map.insert(key, value);
            }
        }

        let mut config = Self::default();

        macro_rules! parse_key {
            ($field:ident) => {
                if let Some(v) = map.get(stringify!($field)) {
                    if let Some(k) = Keycode::from_name(v) {
                        config.$field = k;
                    }
                }
            };
        }

        parse_key!(key_a);
        parse_key!(key_b);
        parse_key!(key_select);
        parse_key!(key_start);
        parse_key!(key_right);
        parse_key!(key_left);
        parse_key!(key_up);
        parse_key!(key_down);
        parse_key!(key_r);
        parse_key!(key_l);
        parse_key!(key_ff);

        macro_rules! parse_ctrl {
            ($field:ident) => {
                if let Some(v) = map.get(stringify!($field)) {
                    if let Some(b) = string_to_controller_button(v) {
                        config.$field = b;
                    }
                }
            };
        }

        parse_ctrl!(ctrl_a);
        parse_ctrl!(ctrl_b);
        parse_ctrl!(ctrl_select);
        parse_ctrl!(ctrl_start);
        parse_ctrl!(ctrl_right);
        parse_ctrl!(ctrl_left);
        parse_ctrl!(ctrl_up);
        parse_ctrl!(ctrl_down);
        parse_ctrl!(ctrl_r);
        parse_ctrl!(ctrl_l);
        parse_ctrl!(ctrl_ff);

        config
    }
}
