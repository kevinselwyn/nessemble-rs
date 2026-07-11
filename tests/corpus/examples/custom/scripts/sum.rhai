// `.sum a, b, …` — emit the sum of its arguments (mod 256).
fn custom(ints, texts) {
    let s = 0;
    for i in ints { s += i; }
    [s % 256]
}
