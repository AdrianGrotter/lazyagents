use alacritty_terminal::Term;
use alacritty_terminal::event::EventListener;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};

pub fn render_term<T: EventListener>(
    term: &Term<T>,
    area: Rect,
    buf: &mut Buffer,
) -> Option<Position> {
    let content = term.renderable_content();
    let offset = content.display_offset as i32;
    let to_screen = |line: i32, col: usize| -> Option<Position> {
        let row = u16::try_from(line + offset).ok()?;
        let col = u16::try_from(col).ok()?;
        (row < area.height && col < area.width).then(|| Position::new(area.x + col, area.y + row))
    };

    for indexed in content.display_iter {
        let cell = indexed.cell;
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let Some(pos) = to_screen(indexed.point.line.0, indexed.point.column.0) else {
            continue;
        };
        let Some(target) = buf.cell_mut(pos) else {
            continue;
        };
        let symbol = if cell.flags.contains(Flags::HIDDEN) {
            ' '
        } else {
            cell.c
        };
        target
            .set_char(symbol)
            .set_style(style_for(cell.fg, cell.bg, cell.flags));
    }

    let cursor = content.cursor;
    let visible =
        content.mode.contains(TermMode::SHOW_CURSOR) && cursor.shape != CursorShape::Hidden;
    visible
        .then(|| to_screen(cursor.point.line.0, cursor.point.column.0))
        .flatten()
}

fn style_for(fg: AnsiColor, bg: AnsiColor, flags: Flags) -> Style {
    let mut modifier = Modifier::empty();
    for (flag, m) in [
        (Flags::BOLD, Modifier::BOLD),
        (Flags::DIM, Modifier::DIM),
        (Flags::ITALIC, Modifier::ITALIC),
        (Flags::ALL_UNDERLINES, Modifier::UNDERLINED),
        (Flags::INVERSE, Modifier::REVERSED),
        (Flags::STRIKEOUT, Modifier::CROSSED_OUT),
    ] {
        if flags.intersects(flag) {
            modifier |= m;
        }
    }
    Style::new()
        .fg(map_color(fg))
        .bg(map_color(bg))
        .add_modifier(modifier)
}

fn map_color(color: AnsiColor) -> Color {
    match color {
        AnsiColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        AnsiColor::Indexed(i) => Color::Indexed(i),
        AnsiColor::Named(named) => {
            let index = named as usize;
            let dim_black = NamedColor::DimBlack as usize;
            match index {
                0..=15 => Color::Indexed(index as u8),
                i if (dim_black..dim_black + 8).contains(&i) => {
                    Color::Indexed((i - dim_black) as u8)
                }
                _ => Color::Reset,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;

    struct Size(usize, usize);

    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            self.1
        }
        fn screen_lines(&self) -> usize {
            self.1
        }
        fn columns(&self) -> usize {
            self.0
        }
    }

    fn term_with(bytes: &[u8]) -> Term<VoidListener> {
        let mut term = Term::new(Config::default(), &Size(20, 4), VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, bytes);
        term
    }

    fn draw(term: &Term<VoidListener>, area: Rect) -> (Buffer, Option<Position>) {
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 8));
        let cursor = render_term(term, area, &mut buf);
        (buf, cursor)
    }

    #[test]
    fn text_lands_at_the_area_offset() {
        let term = term_with(b"hi");
        let (buf, _) = draw(&term, Rect::new(5, 2, 20, 4));
        assert_eq!(buf[(5, 2)].symbol(), "h");
        assert_eq!(buf[(6, 2)].symbol(), "i");
    }

    #[test]
    fn sgr_colors_and_modifiers_map() {
        let term = term_with(b"\x1b[1;31mR\x1b[0m\x1b[38;2;1;2;3;48;5;200mX");
        let (buf, _) = draw(&term, Rect::new(0, 0, 20, 4));
        let r = &buf[(0, 0)];
        assert_eq!(r.fg, Color::Indexed(1));
        assert!(r.modifier.contains(Modifier::BOLD));
        let x = &buf[(1, 0)];
        assert_eq!(x.fg, Color::Rgb(1, 2, 3));
        assert_eq!(x.bg, Color::Indexed(200));
    }

    #[test]
    fn default_colors_are_reset() {
        let term = term_with(b"d");
        let (buf, _) = draw(&term, Rect::new(0, 0, 20, 4));
        assert_eq!(buf[(0, 0)].fg, Color::Reset);
        assert_eq!(buf[(0, 0)].bg, Color::Reset);
    }

    #[test]
    fn wide_chars_keep_column_alignment() {
        let term = term_with("a日b".as_bytes());
        let (buf, _) = draw(&term, Rect::new(0, 0, 20, 4));
        assert_eq!(buf[(0, 0)].symbol(), "a");
        assert_eq!(buf[(1, 0)].symbol(), "日");
        assert_eq!(buf[(3, 0)].symbol(), "b");
    }

    #[test]
    fn cursor_position_is_reported() {
        let term = term_with(b"abc\r\nde");
        let (_, cursor) = draw(&term, Rect::new(2, 1, 20, 4));
        assert_eq!(cursor, Some(Position::new(4, 2)));
    }

    #[test]
    fn hidden_cursor_is_not_reported() {
        let term = term_with(b"abc\x1b[?25l");
        let (_, cursor) = draw(&term, Rect::new(0, 0, 20, 4));
        assert_eq!(cursor, None);
    }

    #[test]
    fn area_smaller_than_grid_clips() {
        let term = term_with(b"0123456789");
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
        render_term(&term, Rect::new(0, 0, 4, 1), &mut buf);
        assert_eq!(buf[(3, 0)].symbol(), "3");
    }
}
