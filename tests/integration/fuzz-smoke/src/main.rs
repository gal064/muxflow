use tmux_control::ControlParser;

const CORPUS: &[&[u8]] = &[
    include_bytes!("../../../fixtures/tmux-control/interleaved.control"),
    include_bytes!("../../../fixtures/tmux-control/malformed.control"),
    include_bytes!("../../../fixtures/tmux-control/tmux-3.3a-capture.control"),
    include_bytes!("../../../fixtures/tmux-control/tmux-3.7-capture.control"),
    b"%output %1 split\\3",
    b"%output %1 nonutf8-\xff-\xfe\n",
    b"%output %1 bad-\\777\n%exit disconnect\n",
];

fn main() {
    let iterations = std::env::args()
        .nth(1)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(10_000)
        .clamp(1, 100_000);
    let mut random = 0x9e37_79b9_7f4a_7c15_u64;
    for iteration in 0..iterations {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let source = CORPUS[iteration % CORPUS.len()];
        let mut mutated = source.to_vec();
        if !mutated.is_empty() {
            let changes = 1 + (random as usize % 8);
            for offset in 0..changes {
                let index =
                    random.rotate_left(offset.try_into().unwrap_or(0)) as usize % mutated.len();
                mutated[index] ^= (random >> (offset % 56)) as u8;
            }
        }
        if iteration % 97 == 0 {
            mutated.extend(std::iter::repeat_n(b'x', 2 * 1024 * 1024));
            mutated.push(b'\n');
        }
        exercise(&mutated, random);
    }
    println!("tmux-control fuzz smoke: {iterations} bounded mutations passed");
}

fn exercise(input: &[u8], random: u64) {
    let mut parser = ControlParser::default();
    let mut offset = 0;
    let mut records = 0_usize;
    while offset < input.len() {
        let width = 1 + ((random.rotate_left((offset % 63) as u32) as usize) % 257);
        let end = offset.saturating_add(width).min(input.len());
        parser.push(&input[offset..end]);
        while parser.next_record().is_some() {
            records += 1;
            assert!(records <= input.len().saturating_add(1));
        }
        offset = end;
    }
    parser.finish();
    while parser.next_record().is_some() {
        records += 1;
        assert!(records <= input.len().saturating_add(1));
    }
}
