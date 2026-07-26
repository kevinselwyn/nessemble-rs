---
nessemble: minor
---
Formatter stride hints can now be written as `; @nessemble-format stride=N[,N,...]`. The old `; @fmt stride=N` spelling keeps working — it is a deprecated alias, not a removal — and both are honored (or disabled) together by `respectStrideHints`. Under the hood, `nessemble-core::tooling` gains a comment-directive registry (`scan_directives` / `scan_directives_with_errors`) that both spellings parse through, so a comment addressed to a nessemble tool is recognized in one place.
