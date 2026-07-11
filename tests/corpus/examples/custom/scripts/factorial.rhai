// `.factorial n` — emit n! (mod 256).
fn factorial(n) {
    if n <= 0 { 1 } else { n * factorial(n - 1) }
}
fn custom(ints, texts) {
    [factorial(ints[0]) % 256]
}
