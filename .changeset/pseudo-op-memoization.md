---
nessemble: patch
---

Custom pseudo-instruction scripts now run once per directive instead of once per assembler pass, halving script work in every build that uses them — and fixing a mis-sized ROM when a script returned a different number of bytes on each pass (as one using `rand` does).
