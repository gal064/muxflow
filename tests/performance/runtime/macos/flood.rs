pub fn run_flood_writer(lines: Option<u64>) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let colors = ["31", "32", "33", "34", "35", "36", "1;37", "2;36", "4;33", "7;35"];
    // Text differs on every line, so GPUI's frame-to-frame line layout cache
    // cannot serve it. Hex only: the typing markers never appear in it.
    let mut rng = 0x9e37_79b9_7f4a_7c15_u64;
    let mut payload = || {
        let mut text = String::with_capacity(128);
        for _ in 0..8 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let _ = std::fmt::Write::write_fmt(&mut text, format_args!("{rng:016x}"));
        }
        text
    };
    let mut counter: u64 = 0;
    let mut block = Vec::with_capacity(1 << 16);
    while lines.is_none_or(|limit| counter < limit) {
        block.clear();
        for _ in 0..64 {
            let color = colors[(counter % colors.len() as u64) as usize];
            let _ = write!(
                block,
                "\x1b[{color}m{counter:010x} {}\x1b[0m {}\r\n",
                payload(),
                payload()
            );
            counter = counter.wrapping_add(1);
        }
        if out.write_all(&block).is_err() || out.flush().is_err() {
            return;
        }
    }
}

fn main() { run_flood_writer(std::env::args().nth(1).map(|n|n.parse().unwrap())); }
