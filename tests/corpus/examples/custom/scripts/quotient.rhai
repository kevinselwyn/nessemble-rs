// `.quotient a, b, …` — emit a / b / … with truncating float division (mod 256).
fn custom(ints, texts) {
    let q = 0;
    let first = true;
    for i in ints {
        if first { q = i; first = false; }
        else { q = (q.to_float() / i.to_float()).to_int(); }
    }
    [q % 256]
}
