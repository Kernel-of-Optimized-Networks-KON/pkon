//! Interleaved timing of the range-decoder variants on `decode_planes`-equivalent work.
//! usage: rctime <file.pkon> <reps> <cooldown_s> [variants, e.g. ABCDEF]
//! Variants run round-robin with a rotating start, a cooldown sleep before every timed decode, so thermal drift is spread evenly.
//! Output lines: `T rep variant seconds unix_ms` (raw), then a summary with first-rep, min, median and paired ratios vs A.
#[path = "../rc.rs"]
mod rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let reps: usize = a[2].parse().unwrap();
    let cool: u64 = a[3].parse().unwrap();
    let pick: Vec<rc::V> = rc::ALL.iter().copied().filter(|v| a.get(4).map_or(true, |s| s.contains(&format!("{v:?}")))).collect();
    let pr = rc::prep(&a[1]);
    println!("file={} samples={} variants={}", a[1], pr.samples, pick.len());
    let mut planes = rc::new_planes(&pr);
    let mut t = vec![vec![0f64; reps]; pick.len()];
    for r in 0..reps {
        for j in 0..pick.len() {
            let i = (j + r) % pick.len();
            std::thread::sleep(Duration::from_secs(cool));
            let s = Instant::now();
            let pos = rc::decode_planes(pick[i], &pr, &mut planes);
            let dt = s.elapsed().as_secs_f64();
            std::hint::black_box(pos);
            let (same, crc) = rc::check(&pr, &planes);
            assert!(same && crc, "variant {:?} produced wrong output", pick[i]);
            t[i][r] = dt;
            let ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
            println!("T {r} {:?} {dt:.4} {ms}", pick[i]);
        }
    }
    let med = |v: &Vec<f64>| {
        let mut s = v.clone();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap());
        s[s.len() / 2]
    };
    println!("summary (seconds; ratio = variant/A, paired per repetition):");
    let base = pick.iter().position(|v| *v == rc::V::A);
    for (i, v) in pick.iter().enumerate() {
        let mn = t[i].iter().cloned().fold(f64::MAX, f64::min);
        let ratios: Vec<f64> = match base {
            Some(b) => (0..reps).map(|r| t[i][r] / t[b][r]).collect(),
            None => vec![],
        };
        println!(
            "  {:34} first {:8.3} min {:8.3} median {:8.3} | paired ratio median {:.3} range [{:.3}, {:.3}]",
            v.name(),
            t[i][0],
            mn,
            med(&t[i]),
            if ratios.is_empty() { f64::NAN } else { med(&ratios) },
            ratios.iter().cloned().fold(f64::MAX, f64::min),
            ratios.iter().cloned().fold(0.0, f64::max)
        );
    }
}
