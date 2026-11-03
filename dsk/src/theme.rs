use crate::analyze::Cat;
use ratatui::style::{Color, Modifier, Style};

pub struct Theme {
    pub mono: bool,
    pub fg: Color,
    pub dim: Color,
    pub accent: Color,
    pub dir: Color,
    pub size: Color,
    pub track: Color,
    pub sel: Color,
    pub border: Color,
    pub ok: Color,
    pub warn: Color,
    pub bad: Color,
    pub mark: Color,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

impl Theme {
    pub fn new(mono: bool) -> Self {
        if mono {
            let r = Color::Reset;
            return Self { mono, fg: r, dim: r, accent: r, dir: r, size: r, track: r, sel: r, border: r, ok: r, warn: r, bad: r, mark: r };
        }
        Self {
            mono,
            fg: Color::Reset,
            dim: rgb(112, 122, 160),
            accent: rgb(122, 162, 247),
            dir: rgb(125, 207, 255),
            size: rgb(224, 175, 104),
            track: rgb(40, 44, 62),
            sel: rgb(44, 56, 92),
            border: rgb(65, 72, 104),
            ok: rgb(158, 206, 106),
            warn: rgb(224, 175, 104),
            bad: rgb(247, 118, 142),
            mark: rgb(255, 158, 100),
        }
    }

    pub fn fg(&self, c: Color) -> Style {
        Style::new().fg(c)
    }

    pub fn sel_style(&self) -> Style {
        if self.mono {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new().bg(self.sel).add_modifier(Modifier::BOLD)
        }
    }

    pub fn badge(&self, c: Color) -> Style {
        if self.mono {
            Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::new().fg(Color::Black).bg(c).add_modifier(Modifier::BOLD)
        }
    }

    pub fn usage(&self, f: f64) -> Color {
        if f < 0.75 { self.ok } else if f < 0.9 { self.warn } else { self.bad }
    }

    pub fn cat(&self, c: Cat) -> Color {
        if self.mono {
            return Color::Reset;
        }
        match c {
            Cat::Video => rgb(247, 118, 142),
            Cat::Audio => rgb(187, 154, 247),
            Cat::Image => rgb(224, 175, 104),
            Cat::Archive => rgb(255, 158, 100),
            Cat::Document => rgb(122, 162, 247),
            Cat::Code => rgb(158, 206, 106),
            Cat::Data => rgb(125, 207, 255),
            Cat::Binary => rgb(115, 218, 202),
            Cat::Other => rgb(130, 138, 170),
        }
    }
}
