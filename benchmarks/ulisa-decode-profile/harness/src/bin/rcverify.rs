//! Correctness gate: every variant must reproduce the production decoder's samples, the stored pixel CRC, and the same number of
//! consumed payload bytes. usage: rcverify <file.pkon>...   (exit code 1 on any mismatch)
#[path = "../rc.rs"]
mod rc;

fn main() {
    let mut bad = 0;
    for f in std::env::args().skip(1) {
        let pr = rc::prep(&f);
        let name = std::path::Path::new(&f).file_name().unwrap().to_string_lossy().to_string();
        let mut line = format!("{name:44}");
        let mut ref_pos = None;
        for v in rc::ALL {
            let mut planes = rc::new_planes(&pr);
            let pos = rc::decode_planes(v, &pr, &mut planes);
            let (same, crc) = rc::check(&pr, &planes);
            let rp = *ref_pos.get_or_insert(pos);
            let ok = same && crc && pos == rp;
            if !ok {
                bad += 1;
            }
            line += &format!(" {:?}={}", v, if ok { "ok" } else { "MISMATCH" });
        }
        println!("{line}");
    }
    println!("{}", if bad == 0 { "ALL VARIANTS EXACT" } else { "FAILURES" });
    std::process::exit((bad != 0) as i32);
}
