#[inline(always)]
fn masked_b(w: u32) -> i32 {
    ((((w as i32 >> 19) as u32) & 0xfffff000)
        | ((w << 4) & 0x800)
        | ((w >> 20) & 0x7e0)
        | ((w >> 7) & 0x1e)) as i32
}

#[inline(always)]
fn masked_j(w: u32) -> i32 {
    ((((w as i32 >> 11) as u32) & 0xfff00000)
        | (w & 0xff000)
        | ((w >> 9) & 0x800)
        | ((w >> 20) & 0x7fe)) as i32
}

fn main() {
    let bytes = std::fs::read(std::env::args().nth(1).unwrap()).unwrap();
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    for &w in &words {
        assert_eq!(masked_b(w), btype_immediate(w));
        assert_eq!(masked_j(w), jtype_immediate(w));
    }
    for pass in 0..12 {
        let order = if pass % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        };
        for masked in order {
            let now = std::time::Instant::now();
            let mut checksum = 0i64;
            for _ in 0..1024 {
                let input = std::hint::black_box(&words);
                let value = if masked {
                    input
                        .iter()
                        .map(|&w| {
                            if w & 127 == 0x63 {
                                masked_b(w)
                            } else {
                                masked_j(w)
                            }
                        })
                        .map(i64::from)
                        .sum::<i64>()
                } else {
                    input
                        .iter()
                        .map(|&w| {
                            if w & 127 == 0x63 {
                                btype_immediate(w)
                            } else {
                                jtype_immediate(w)
                            }
                        })
                        .map(i64::from)
                        .sum::<i64>()
                };
                checksum = checksum.wrapping_add(std::hint::black_box(value));
            }
            println!(
                "{{\"masked\":{masked},\"pass\":{pass},\"ns\":{},\"checksum\":{checksum},\"fields\":{}}}",
                now.elapsed().as_nanos(),
                words.len() * 1024
            );
        }
    }
}
