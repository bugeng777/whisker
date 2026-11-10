#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pose {
    Idle,
    Scanning,
    Working,
    Success,
}

impl Pose {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "idle" => Some(Pose::Idle),
            "scanning" => Some(Pose::Scanning),
            "working" => Some(Pose::Working),
            "success" => Some(Pose::Success),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CellSpec {
    pub ch: char,
    pub fg: Option<u8>,
    pub bg: Option<u8>,
}

// Orange cat sprite: O body, D outline, C muzzle, W eye white, E eye, P purr.
const BASE: &[&str] = &[
    ".............DDD............DDDD....................",
    ".............DDDD...........DDDD....................",
    "...........DDOOODD........DDOODD....................",
    "...........DDOOODD.......DDOOODD....................",
    "...........DDOOOOODD....DDOOOOODD...................",
    ".........DDDDOOOOODDDDDDDOOOOOODD...................",
    ".........DDOOOOOOODDDDDDOOOOOOODD...................",
    ".........DDOOOOOOOOOOOOOOOOOOOOODDD.................",
    ".........DDOOOOOOOOOOOOOOOOOOOOOODD.................",
    ".......DDOOOOOOOOOOOOOOOOOOOOOOOODD.................",
    ".......DDOOOOOOOOOOOOOOOOOOOOOOOOODDD...............",
    ".......DDOOOOOOOOOOOOOOOOOOOOOOOOOODD...............",
    "......DDOEEEEEEEEEEOOOEEEEEEEEEOOOODDDD.............",
    "......DOOOWWWWWEEEOOOOWWWWWEEEEOOOOOODD.............",
    "......DOOOWWWWWEEEOOOOWWWWWEEEOOOOOOODD.............",
    "....DDDOOOOOWWOOOOOOOOOOOOOOOOOOOOOOODDDD...........",
    "....DDOOOOOOOOOOOOOOOOOOOOOOOOOEOOOOOOODD...........",
    "....DDOOOOOOOOOOOOOOOOOOOOOOOOEEOOOOOOODD...........",
    "...DDDOOOOOOOOOOOOOOOOOOOOOOOEEOOOOOOOODDDD.........",
    "..DDOOOOOOOOOCCCCCCCCCCCEEEEEOOOOOOOOOOOODD.........",
    "..DDOOOOOOOOOCCCCCCCCCCCCCOOOOOOOOOOOOOOODDDDDDDDD..",
    ".DDDOOOOOOOOCCCCCCCCCCCCCCCOOOOOOOOOOOOOODDDDDDDDDD.",
    "DDOOOOOOOOCCCCCCCCCCCCCCCCCCCOOOOOOOOOOOOODOOOOOOODD",
    "DDOOOOOOOOCCCCCCCCCCCCCCCCCCCOOOOOOOOOOOOODOOOOOOODD",
    "DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD",
    "DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD",
];

pub const MARK: &str = "🐈";

fn color_enabled() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    !matches!(std::env::var("TERM").as_deref(), Ok("dumb"))
}

fn color_for(key: char) -> Option<u8> {
    // xterm-256 approximations; Terminal.app does not support 24-bit colors.
    match key {
        'O' => Some(208),
        'D' => Some(94),
        'C' => Some(223),
        'W' => Some(230),
        'E' => Some(236),
        'P' => Some(218),
        _ => None,
    }
}

fn grid(pose: Pose) -> Vec<Vec<char>> {
    let mut rows: Vec<Vec<char>> = BASE.iter().map(|row| row.chars().collect()).collect();
    match pose {
        Pose::Idle => {}
        Pose::Scanning => {
            for y in [15usize, 16] {
                for x in [1usize, 2, 3, 47, 48, 49] {
                    if rows[y][x] == '.' {
                        rows[y][x] = 'W';
                    }
                }
            }
        }
        Pose::Working => {
            for y in [13usize, 14] {
                let line: String = rows[y].iter().collect();
                let line = line
                    .replace("WWWWWEEE", "WWWWWWEE")
                    .replace("WWWWWEEEE", "WWWWWWEEE");
                rows[y] = line.chars().collect();
            }
        }
        Pose::Success => {
            for y in [13usize, 14] {
                let line: String = rows[y].iter().collect();
                let line = line
                    .replace("WWWWWEEE", "OOOOOOOO")
                    .replace("WWWWWEEEE", "OOOOOOOOO");
                rows[y] = line.chars().collect();
            }
            for x in 11..=17 {
                rows[14][x] = 'D';
            }
            for x in 23..=29 {
                rows[14][x] = 'D';
            }
            for (y, x) in [
                (16usize, 2usize),
                (17, 1),
                (17, 2),
                (18, 2),
                (18, 3),
                (16, 49),
                (17, 50),
                (17, 49),
                (18, 49),
                (18, 48),
            ] {
                if rows[y][x] == '.' {
                    rows[y][x] = 'P';
                }
            }
        }
    }
    rows
}

fn flick_tail(rows: &mut [Vec<char>]) {
    rows[20][49] = 'D';
    rows[20][50] = 'D';
    rows[21][49] = 'O';
    rows[21][50] = 'O';
    rows[22][50] = 'D';
    rows[23][51] = '.';
}

fn extend_whiskers(rows: &mut [Vec<char>]) {
    for x in [4usize, 46] {
        if rows[15][x] == '.' {
            rows[15][x] = 'W';
        }
    }
    for x in [0usize, 50] {
        if rows[16][x] == '.' {
            rows[16][x] = 'W';
        }
    }
}

fn shift_purr(rows: &mut [Vec<char>]) {
    for (y, x) in [
        (15usize, 2usize),
        (16, 1),
        (16, 2),
        (17, 2),
        (17, 3),
        (15, 49),
        (16, 50),
        (16, 49),
        (17, 49),
        (17, 48),
    ] {
        if rows[y][x] == '.' {
            rows[y][x] = 'P';
        }
    }
}

fn grid_at(pose: Pose, frame: usize) -> Vec<Vec<char>> {
    let mut rows = grid(pose);
    match pose {
        Pose::Idle => {
            if (frame / 8) % 2 == 1 {
                flick_tail(&mut rows);
            }
        }
        Pose::Working => {
            if (frame / 2) % 2 == 1 {
                flick_tail(&mut rows);
            }
        }
        Pose::Scanning => {
            if (frame / 4) % 2 == 1 {
                extend_whiskers(&mut rows);
            }
        }
        Pose::Success => {
            if (frame / 3) % 2 == 1 {
                shift_purr(&mut rows);
            }
        }
    }
    rows
}

fn to_cells(rows: &[Vec<char>]) -> Vec<Vec<CellSpec>> {
    let mut out = Vec::new();
    for pair in rows.chunks(2) {
        let top = &pair[0];
        let bottom = pair.get(1);
        let mut line = Vec::with_capacity(top.len());
        for x in 0..top.len() {
            let t = color_for(top[x]);
            let b = bottom
                .and_then(|row| row.get(x))
                .and_then(|&ch| color_for(ch));
            let cell = match (t, b) {
                (None, None) => CellSpec {
                    ch: ' ',
                    fg: None,
                    bg: None,
                },
                (Some(fg), None) => CellSpec {
                    ch: '▀',
                    fg: Some(fg),
                    bg: None,
                },
                (None, Some(fg)) => CellSpec {
                    ch: '▄',
                    fg: Some(fg),
                    bg: None,
                },
                (Some(fg), Some(bg)) if fg == bg => CellSpec {
                    ch: '█',
                    fg: Some(fg),
                    bg: None,
                },
                (Some(fg), Some(bg)) => CellSpec {
                    ch: '▀',
                    fg: Some(fg),
                    bg: Some(bg),
                },
            };
            line.push(cell);
        }
        out.push(line);
    }
    out
}

pub fn art_cells_at(pose: Pose, frame: usize) -> Vec<Vec<CellSpec>> {
    to_cells(&grid_at(pose, frame))
}

fn key_index(ch: char) -> usize {
    match ch {
        'O' => 1,
        'D' => 2,
        'C' => 3,
        'W' => 4,
        'E' => 5,
        'P' => 6,
        _ => 0,
    }
}

fn index_key(index: usize) -> char {
    match index {
        1 => 'O',
        2 => 'D',
        3 => 'C',
        4 => 'W',
        5 => 'E',
        6 => 'P',
        _ => '.',
    }
}

pub fn compact_cells(pose: Pose, frame: usize) -> Vec<Vec<CellSpec>> {
    let full = grid_at(pose, frame);
    let (x0, x1, y0, y1) = (2usize, 44usize, 0usize, 22usize);
    let (out_w, out_h) = (14usize, 8usize);
    let mut small = vec![vec!['.'; out_w]; out_h];
    let cw = x1 - x0;
    let ch = y1 - y0;
    for ty in 0..out_h {
        let sy0 = y0 + ty * ch / out_h;
        let sy1 = y0 + (ty + 1) * ch / out_h;
        for tx in 0..out_w {
            let sx0 = x0 + tx * cw / out_w;
            let sx1 = x0 + (tx + 1) * cw / out_w;
            let mut counts = [0usize; 7];
            for y in sy0..sy1 {
                for x in sx0..sx1 {
                    counts[key_index(full[y][x])] += 1;
                }
            }
            let total: usize = counts.iter().sum();
            let nonbg = total - counts[0];
            if nonbg * 100 >= total * 35 {
                let mut best = 1;
                for index in 2..7 {
                    if counts[index] > counts[best] {
                        best = index;
                    }
                }
                small[ty][tx] = index_key(best);
            }
        }
    }
    match pose {
        Pose::Scanning => {
            if small[5][0] == '.' {
                small[5][0] = 'W';
            }
            if small[5][13] == '.' {
                small[5][13] = 'W';
            }
        }
        Pose::Success => {
            if small[6][0] == '.' {
                small[6][0] = 'P';
            }
            if small[6][13] == '.' {
                small[6][13] = 'P';
            }
        }
        _ => {}
    }
    to_cells(&small)
}

pub fn art_cells(pose: Pose) -> Vec<Vec<CellSpec>> {
    art_cells_at(pose, 0)
}

fn ansi_cell(cell: CellSpec) -> String {
    match (cell.fg, cell.bg) {
        (None, None) => cell.ch.to_string(),
        (Some(fg), None) => format!("\x1b[38;5;{fg}m{}\x1b[0m", cell.ch),
        (None, Some(bg)) => format!("\x1b[48;5;{bg}m{}\x1b[0m", cell.ch),
        (Some(fg), Some(bg)) => {
            format!("\x1b[38;5;{fg}m\x1b[48;5;{bg}m{}\x1b[0m", cell.ch)
        }
    }
}

pub fn compact_lines(pose: Pose, frame: usize) -> Vec<String> {
    compact_cells(pose, frame)
        .into_iter()
        .map(|row| row.into_iter().map(ansi_cell).collect::<String>())
        .collect()
}

pub fn art_lines(pose: Pose, color: bool) -> Vec<String> {
    if !color {
        return Vec::new();
    }
    art_cells(pose)
        .into_iter()
        .map(|row| row.into_iter().map(ansi_cell).collect::<String>())
        .collect()
}

pub fn banner(model: &str, session: &str, mode: &str, with_art: bool) -> Vec<String> {
    let color = color_enabled();
    let bold = if color { "\x1b[1;38;5;39m" } else { "" };
    let dim = if color { "\x1b[2m" } else { "" };
    let reset = if color { "\x1b[0m" } else { "" };

    let info = vec![
        format!("{bold}whisker{reset} {dim}· a cat for your codebase{reset}"),
        format!("{dim}model    {reset}{model}"),
        format!("{dim}session  {reset}{session}"),
        format!("{dim}mode     {reset}{mode}"),
        String::new(),
        format!("{dim}Enter send · Esc interrupt · Shift+Tab mode{reset}"),
        format!("{dim}Ctrl+C exit · /help for commands · /cat <pose>{reset}"),
    ];

    let mut lines = Vec::new();
    if with_art && color {
        let cat = compact_lines(Pose::Idle, 0);
        for (index, line) in cat.iter().enumerate() {
            let right = info.get(index).cloned().unwrap_or_default();
            lines.push(format!("  {line}   {right}"));
        }
        for line in info.iter().skip(cat.len()) {
            lines.push(line.clone());
        }
    } else {
        lines.extend(info);
    }
    lines
}

pub fn dots(frame: usize) -> &'static str {
    const FRAMES: [&str; 4] = ["·  ", "·· ", "···", " ··"];
    FRAMES[frame % FRAMES.len()]
}

pub fn spinner(frame: usize) -> String {
    format!("{}  {}", MARK, dots(frame))
}

pub fn terminal_title(model: &str) -> String {
    format!("\x1b]0;{} whisker · {model}\x07", MARK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sprite_has_expected_shape() {
        let cells = art_cells(Pose::Idle);
        assert_eq!(cells.len(), 13);
        assert!(cells.iter().all(|row| row.len() == 52));
    }

    #[test]
    fn poses_differ() {
        fn signature(pose: Pose) -> Vec<(char, Option<u8>, Option<u8>)> {
            art_cells(pose)
                .into_iter()
                .flatten()
                .map(|cell| (cell.ch, cell.fg, cell.bg))
                .collect()
        }
        let idle = signature(Pose::Idle);
        assert_ne!(idle, signature(Pose::Scanning));
        assert_ne!(idle, signature(Pose::Working));
        assert_ne!(idle, signature(Pose::Success));
    }
}

#[cfg(test)]
mod motion_tests {
    use super::*;

    fn sig(pose: Pose, frame: usize) -> Vec<(char, Option<u8>, Option<u8>)> {
        art_cells_at(pose, frame)
            .into_iter()
            .flatten()
            .map(|cell| (cell.ch, cell.fg, cell.bg))
            .collect()
    }

    #[test]
    fn mascot_frames_animate() {
        assert_ne!(sig(Pose::Idle, 0), sig(Pose::Idle, 8));
        assert_ne!(sig(Pose::Working, 0), sig(Pose::Working, 2));
        assert_ne!(sig(Pose::Scanning, 0), sig(Pose::Scanning, 4));
        assert_ne!(sig(Pose::Success, 0), sig(Pose::Success, 3));
    }
}
