## For developers

If you want enable `clang` to find the kernel-only headers in your editor (for example, `#include <linux/skbuff.h>`).

1. `sudo apt install bear`
2. `bear -- make all`, and `compile_commands.json` will be created. `clang` can find the compile commands now!