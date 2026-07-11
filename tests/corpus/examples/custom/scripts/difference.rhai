// `.difference a, b, …` — emit a - b - … (mod 256).
fn custom(ints, texts) {
    let d = 0;
    let first = true;
    for i in ints {
        if first { d = i; first = false; } else { d -= i; }
    }
    [d % 256]
}
