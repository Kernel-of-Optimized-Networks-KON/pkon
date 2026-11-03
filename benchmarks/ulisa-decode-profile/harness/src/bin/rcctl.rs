//! Verbatim-copy control: A = library Stage4Dec, K = verbatim copy in this crate (rc::V::B, `Copy0`), B = branch-free (rc::V::C).
//! usage: rcctl verify <file>... | rcctl time <file> <reps> <cooldown_s> <warmups>   (3-variant Williams-style balanced orders)
#[path = "../rc.rs"]
mod rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const VARS: [(char, rc::V); 3] = [('A', rc::V::A), ('K', rc::V::B), ('B', rc::V::C)];
// all 6 permutations: every variant occupies every position twice and follows every other variant twice per 6 reps
const ORDERS: [[usize; 3]; 6] = [[0, 1, 2], [1, 2, 0], [2, 0, 1], [0, 2, 1], [2, 1, 0], [1, 0, 2]];

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a[1] == "verify" {
        let mut bad = 0;
        for f in &a[2..] {
            let pr = rc::prep(f);
            let mut ref_pos = None;
            for (l, v) in VARS {
                let mut planes = rc::new_planes(&pr);
                let pos = rc::decode_planes(v, &pr, &mut planes);
                let (same, crc) = rc::check(&pr, &planes);
                let rp = *ref_pos.get_or_insert(pos);
                let ok = same && crc && pos == rp;
                bad += !ok as i32;
                println!("V {f} {l} samples_equal={same} pixel_crc_equal={crc} consumed={pos} equal_to_A={} -> {}", pos == rp, if ok { "OK" } else { "FAIL" });
            }
        }
        println!("{}", if bad == 0 { "ALL EXACT" } else { "FAILURES" });
        std::process::exit((bad != 0) as i32);
    }
    let (reps, cool, warm): (usize, u64, usize) = (a[3].parse().unwrap(), a[4].parse().unwrap(), a[5].parse().unwrap());
    let pr = rc::prep(&a[2]);
    println!("file={} samples={} reps={reps} cooldown_s={cool} warmups={warm}", a[2], pr.samples);
    let mut planes = rc::new_planes(&pr);
    let mut run = |i: usize, tag: &str, r: usize| {
        std::thread::sleep(Duration::from_secs(cool));
        let s = Instant::now();
        let pos = rc::decode_planes(VARS[i].1, &pr, &mut planes);
        let dt = s.elapsed().as_secs_f64();
        std::hint::black_box(pos);
        let (same, crc) = rc::check(&pr, &planes);
        assert!(same && crc, "variant {} wrong output", VARS[i].0);
        let ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
        println!("{tag} {r} {} {dt:.4} {ms}", VARS[i].0);
    };
    for r in 0..warm {
        for i in ORDERS[r % 6] {
            run(i, "W", r);
        }
    }
    for r in 0..reps {
        for i in ORDERS[r % 6] {
            run(i, "T", r);
        }
    }
}
