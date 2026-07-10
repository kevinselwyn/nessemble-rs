//! `.incrle` run-length encoding, ported from the reference `pseudo/incrle.c`.
//!
//! The scheme distinguishes *runs* (three or more identical bytes, emitted as a
//! `count`/`value` pair with the high bit clear) from *literals* (buffered runs
//! of one or two distinct bytes, flushed as `0x80 | length` followed by the raw
//! bytes). A terminating `0xFF` marks the end of the stream.

/// Run-length encode `data` into the reference's `.incrle` format.
pub fn rle_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut run: Vec<u8> = Vec::new();
    let mut has_run = false;
    let mut i = 0;

    while i < data.len() {
        let byte = data[i];
        let last = i;
        let mut count = 0u32;
        while i < data.len() && data[i] == byte {
            count += 1;
            i += 1;
        }

        if count > 2 {
            if has_run {
                out.push(0x80 + run.len() as u8);
                out.extend_from_slice(&run);
                run.clear();
            }
            // Split runs longer than 0x7F into repeated maximal chunks.
            while count > 0x7F {
                out.push(0x7F);
                out.push(byte);
                count -= 0x7F;
            }
            out.push(count as u8);
            out.push(byte);
            has_run = false;
        } else if !has_run {
            run.clear();
            run.extend_from_slice(&data[last..i]);
            has_run = true;
        } else if run.len() > 0x7C {
            // Flush the buffered literal run; the reference drops the current
            // bytes here and does not start a new run.
            out.push(0x80 + run.len() as u8);
            out.extend_from_slice(&run);
            run.clear();
            has_run = false;
        } else {
            run.extend_from_slice(&data[last..i]);
            has_run = true;
        }
    }

    if has_run {
        out.push(0x80 + run.len() as u8);
        out.extend_from_slice(&run);
    }

    out.push(0xFF);
    out
}
