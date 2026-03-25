# EmScript2: Extremely Modular Script 2
This is an experimental language built on the core ideas of [EmScript](https://github.com/Bobxcat/emscript).
So far, the focus has been on the low-level implementation of important EmScript features and this project only contains a mid-level
representation that compiles to machine code, without parsing from raw EmScript code or an AST

* EmScript2 compiles using cranelift instead of WASM, which makes security more vulnerable but allows for a
better custom GC implementation with reference passing between Rust and Emscript runtimes. Currently, passing references between code is
not implementated (the GC works correctly outside of passing between Rust and Emscript) because it's hard to follow Rust's borrowing rules
without putting every GC value behind a lock (such as a Mutex). Even if every value was locked, it'd be too easy to create a deadlock.
* EmScript2 uses a mid level control flow graph (CFG) representation which compiles fairly directly into the cranelift format.
The mid level repr has a text format that can be written, avoiding having to hand-build an MIR struct to test the final compilation
