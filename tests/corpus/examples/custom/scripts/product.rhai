// `.product a, b, …` — emit the product of its arguments (mod 256).
fn custom(ints, texts) {
    let p = 0;
    let first = true;
    for i in ints {
        if first { p = i; first = false; } else { p *= i; }
    }
    [p % 256]
}
