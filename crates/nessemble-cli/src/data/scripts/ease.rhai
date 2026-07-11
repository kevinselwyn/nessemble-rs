// `.ease "<type>"[, start, end, steps]` — emit an easing curve as bytes.
//
// Ported from the reference `ease.lua`. Easing functions take (t, b, c, d) and
// are evaluated in f64 to match the reference's double arithmetic byte-for-byte.

fn easeInQuad(t, b, c, d) {
    let n = t / d;
    c * n * n + b
}

fn easeOutQuad(t, b, c, d) {
    let n = t / d;
    -c * n * (n - 2.0) + b
}

fn easeInOutQuad(t, b, c, d) {
    let n = t / (d / 2.0);
    if n < 1.0 {
        return c / 2.0 * n * n + b;
    }
    let m = n - 1.0;
    -c / 2.0 * (m * (m - 2.0) - 1.0) + b
}

fn easeInCubic(t, b, c, d) {
    let n = t / d;
    c * n * n * n + b
}

fn easeOutCubic(t, b, c, d) {
    let n = (t / d) - 1.0;
    c * (n * n * n + 1.0) + b
}

fn easeInOutCubic(t, b, c, d) {
    let n = t / (d / 2.0);
    if n < 1.0 {
        return c / 2.0 * n * n * n + b;
    }
    let m = n - 2.0;
    c / 2.0 * (m * m * m + 2.0) + b
}

fn easeInQuint(t, b, c, d) {
    let n = t / d;
    c * n * n * n * n * n + b
}

fn easeOutQuint(t, b, c, d) {
    let n = (t / d) - 1.0;
    c * (n * n * n * n * n + 1.0) + b
}

fn easeInOutQuint(t, b, c, d) {
    let n = t / (d / 2.0);
    if n < 1.0 {
        return c / 2.0 * n * n * n * n * n + b;
    }
    let m = n - 2.0;
    c / 2.0 * (m * m * m * m * m + 2.0) + b
}

fn easeOutBounce(t, b, c, d) {
    let n = t / d;
    let magic = 7.5625;

    if n < (1.0 / 2.75) {
        c * (magic * n * n) + b
    } else if n < (2.0 / 2.75) {
        let m = n - (1.5 / 2.75);
        c * (magic * m * m + 0.75) + b
    } else if n < (2.5 / 2.75) {
        let m = n - (2.25 / 2.75);
        c * (magic * m * m + 0.9375) + b
    } else {
        let m = n - (2.625 / 2.75);
        c * (magic * m * m + 0.984375) + b
    }
}

fn easeInBounce(t, b, c, d) {
    c - easeOutBounce(d - t, 0.0, c, d) + b
}

fn easeInOutBounce(t, b, c, d) {
    if t < d / 2.0 {
        return easeInBounce(t * 2.0, 0.0, c, d) * 0.5 + b;
    }
    easeOutBounce(t * 2.0 - d, 0.0, c, d) * 0.5 + c * 0.5 + b
}

// Dispatch an easing function by name, throwing on an unknown type.
fn ease_apply(name, t, b, c, d) {
    switch name {
        "easeInQuad" => easeInQuad(t, b, c, d),
        "easeOutQuad" => easeOutQuad(t, b, c, d),
        "easeInOutQuad" => easeInOutQuad(t, b, c, d),
        "easeInCubic" => easeInCubic(t, b, c, d),
        "easeOutCubic" => easeOutCubic(t, b, c, d),
        "easeInOutCubic" => easeInOutCubic(t, b, c, d),
        "easeInQuint" => easeInQuint(t, b, c, d),
        "easeOutQuint" => easeOutQuint(t, b, c, d),
        "easeInOutQuint" => easeInOutQuint(t, b, c, d),
        "easeInBounce" => easeInBounce(t, b, c, d),
        "easeOutBounce" => easeOutBounce(t, b, c, d),
        "easeInOutBounce" => easeInOutBounce(t, b, c, d),
        _ => throw "Invalid easing type `" + name + "`",
    }
}

fn custom(ints, texts) {
    if texts.len() == 0 {
        throw "Missing easing type";
    }
    let arg_ease = texts[0];

    let arg_start = 0.0;
    let arg_end = 16.0;
    let arg_steps = 16;
    if ints.len() > 0 { arg_start = ints[0].to_float(); }
    if ints.len() > 1 { arg_end = ints[1].to_float(); }
    if ints.len() > 2 { arg_steps = ints[2]; }
    if arg_steps > 255 { arg_steps = 255; }

    let result = [];
    for i in 0..arg_steps {
        let value = 0.0;
        if arg_end > arg_start {
            let e = ease_apply(arg_ease, i.to_float() / (arg_steps - 1).to_float(), 0.0, 1.0, 1.0);
            value = e * (arg_end - arg_start);
            value = (value + arg_start).floor();
        } else {
            let e = ease_apply(arg_ease, (arg_steps - i - 1).to_float() / (arg_steps - 1).to_float(), 0.0, 1.0, 1.0);
            value = e * (arg_start - arg_end);
            value = (value + arg_end).floor();
        }
        result.push(value.to_int() % 256);
    }
    result
}
