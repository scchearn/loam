---
title: Hostile front matter
anchor: &big [1, 2, 3]
alias: *big
tricky: !!js/function "function(){ return 1 }"
deep:
  a:
    b:
      c:
        d:
          e:
            f:
              g:
                h: bottom
list:
  - one
  - two
---

# Body after front matter

Text.
