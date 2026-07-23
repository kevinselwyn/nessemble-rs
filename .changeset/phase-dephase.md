---
nessemble: minor
---
Add `.phase` / `.dephase` directives for bank-swapped code. Labels defined inside
a `.phase ADDRESS` block take the run-time (post-swap) address while ROM layout
keeps flowing from `.org`, so there's no need to subtract the swap offset from
every label by hand. The block ends at `.dephase` or a bank/segment switch.
