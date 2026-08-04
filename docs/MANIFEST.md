# Rust Engineering and Performance Docs Manifest

Use this file as a routing map for writing, reviewing, measuring, and optimizing Rust. Prefer the live linked documentation over copied notes so agents follow current language, library, toolchain, and performance guidance.

When starting a task:

1. Identify the surface: language and API design, testing and documentation, benchmarking and profiling, CPU and memory use, I/O and concurrency, build configuration, binary footprint, or unsafe code.
2. Open that surface's overview first to establish its model, constraints, and recommended workflow.
3. Open the exact reference page when choosing an API, lint, Cargo setting, compiler option, or safety contract.
4. Use `/stable/` links by default; when the repository pins another Rust channel, swap `/stable/` for `/beta/` or `/nightly/`, and use `rustup doc` when exact installed-release documentation is required.

## Core Rust Engineering References

### The Rust Programming Language
Use when: Learning or checking idiomatic Rust fundamentals, ownership, types, modules, errors, tests, iterators, smart pointers, concurrency, and Cargo workflows.
Overview: https://doc.rust-lang.org/stable/book/

### Standard Library
Use when: Selecting stable standard types, traits, functions, macros, and modules or checking their exact behavior, complexity notes, and examples.
Reference: https://doc.rust-lang.org/stable/std/

### Rust Reference
Use when: Resolving exact language semantics for types, expressions, attributes, generics, lifetimes, layout, linkage, unsafe behavior, or conditional compilation.
Reference: https://doc.rust-lang.org/stable/reference/

### Cargo Book and Reference
Use when: Creating or maintaining packages, workspaces, manifests, dependencies, features, targets, build scripts, profiles, publishing, or Cargo configuration.
Overview: https://doc.rust-lang.org/stable/cargo/
Full index: https://doc.rust-lang.org/stable/cargo/reference/

### Rust API Guidelines
Use when: Designing or reviewing a public crate API for idiomatic naming, interoperability, predictability, flexibility, safety, dependability, debuggability, and future compatibility.
Overview: https://rust-lang.github.io/api-guidelines/
Full index: https://rust-lang.github.io/api-guidelines/checklist.html

### Rust Style Guide
Use when: Resolving source-formatting conventions or configuring code to match the default style implemented by rustfmt.
Reference: https://doc.rust-lang.org/stable/style-guide/

### rustdoc Book
Use when: Writing crate and item documentation, examples, doctests, intra-doc links, or generated API documentation.
Overview: https://doc.rust-lang.org/stable/rustdoc/
Reference: https://doc.rust-lang.org/stable/rustdoc/how-to-write-documentation.html

### Clippy
Use when: Finding correctness, suspicious-code, style, complexity, or performance lints and checking the rationale and fix for a specific lint.
Overview: https://doc.rust-lang.org/stable/clippy/
Full index: https://rust-lang.github.io/rust-clippy/stable/index.html

### Testing and Error Handling
Use when: Designing Rust tests or choosing between recoverable `Result` errors and unrecoverable panics.
Link: https://doc.rust-lang.org/stable/book/ch11-00-testing.html
Link: https://doc.rust-lang.org/stable/book/ch09-00-error-handling.html

## Measurement and Performance Workflow

### Rust Performance Book
Use when: Beginning a runtime speed, CPU, allocation, memory use, I/O, binary size, or compile-time optimization task and selecting practical techniques after measuring.
Unofficial: https://nnethercote.github.io/perf-book/introduction.html
Unofficial: https://nnethercote.github.io/perf-book/profiling.html

### Benchmarking
Use when: Running benchmark targets, designing repeatable measurements, or preventing the optimizer from removing the work being measured.
Unofficial: https://nnethercote.github.io/perf-book/benchmarking.html
Reference: https://doc.rust-lang.org/stable/cargo/commands/cargo-bench.html
Reference: https://doc.rust-lang.org/stable/std/hint/fn.black_box.html

### Cargo Build Performance
Use when: Diagnosing slow builds, inspecting compilation concurrency and critical paths, or reducing iteration time without changing runtime behavior.
Overview: https://doc.rust-lang.org/stable/cargo/guide/build-performance.html
Reference: https://doc.rust-lang.org/stable/cargo/reference/timings.html

## Runtime CPU, Memory, and Responsiveness

### Ownership and Borrowing
Use when: Restructuring ownership and lifetimes to avoid unnecessary cloning, reference counting, copying, or allocation while preserving safety.
Overview: https://doc.rust-lang.org/stable/book/ch04-00-understanding-ownership.html

### Collections and Allocation
Use when: Choosing a collection, managing capacity, reducing reallocations, selecting heap ownership types, or working without the full standard library.
Overview: https://doc.rust-lang.org/stable/std/collections/
Reference: https://doc.rust-lang.org/stable/alloc/

### Iterators
Use when: Building lazy data-processing paths, avoiding intermediate collections, or checking iterator adapters and consumption semantics.
Reference: https://doc.rust-lang.org/stable/std/iter/

### Buffered I/O
Use when: Improving file, stream, or terminal throughput and latency by reducing small reads, writes, allocations, or operating-system calls.
Reference: https://doc.rust-lang.org/stable/std/io/

### Type Layout
Use when: Investigating value size, alignment, padding, enum representation, cache density, FFI layout, or memory-footprint tradeoffs.
Reference: https://doc.rust-lang.org/stable/reference/type-layout.html

### Threads and Synchronization
Use when: Parallelizing CPU work or choosing threads, channels, locks, atomics, and shared ownership while accounting for contention and scheduling.
Overview: https://doc.rust-lang.org/stable/std/thread/
Reference: https://doc.rust-lang.org/stable/std/sync/

### Async Rust
Use when: Designing responsive concurrent I/O, futures, task scheduling, cancellation, pinning, or async interfaces and deciding whether async is appropriate.
Overview: https://rust-lang.github.io/async-book/

### Architecture-Specific Intrinsics
Use when: A measured hotspot justifies CPU feature detection, SIMD, or architecture-specific intrinsics beyond portable optimized Rust.
Reference: https://doc.rust-lang.org/stable/std/arch/

## Build, Binary, and Deployment Efficiency

### Cargo Profiles
Use when: Tuning optimization level, debug information, stripping, LTO, panic strategy, incremental compilation, or codegen units for a measured speed-size-build-time tradeoff.
Reference: https://doc.rust-lang.org/stable/cargo/reference/profiles.html

### rustc Code Generation
Use when: Evaluating compiler-level controls such as target CPU and features, LTO, relocation, symbol stripping, codegen units, or optimization remarks.
Reference: https://doc.rust-lang.org/stable/rustc/codegen-options/index.html

### Profile-Guided Optimization
Use when: Representative production workloads justify feeding execution profiles back into rustc for branch, layout, inlining, and register-allocation optimization.
Reference: https://doc.rust-lang.org/stable/rustc/profile-guided-optimization.html

### Dependency Features and Duplication
Use when: Reducing compile time, executable size, or unused functionality by controlling crate features and finding duplicate dependency versions.
Reference: https://doc.rust-lang.org/stable/cargo/reference/features.html
Reference: https://doc.rust-lang.org/stable/cargo/commands/cargo-tree.html

### Constrained and no_std Builds
Use when: Optimizing for limited RAM, ROM, startup runtime, or allocator availability and balancing execution speed against binary size.
Overview: https://doc.rust-lang.org/stable/embedded-book/intro/no-std.html
Reference: https://doc.rust-lang.org/stable/embedded-book/unsorted/speed-vs-size.html

## Safety Boundaries

### Unsafe Rust and Undefined Behavior
Use when: A measured optimization requires raw pointers, manual allocation, custom data structures, intrinsics, FFI, or another unsafe contract that safe Rust cannot express.
Overview: https://doc.rust-lang.org/stable/nomicon/
Reference: https://doc.rust-lang.org/stable/reference/behavior-considered-undefined.html

## Agent Routing Notes

- Establish a representative baseline before optimizing, then compare complete workloads as well as isolated hot paths.
- Start with safe, idiomatic Rust and standard-library APIs; use unsafe code or architecture-specific intrinsics only for a measured bottleneck with a documented safety contract and validation.
- Treat latency, throughput, CPU time, peak memory, allocation count, binary size, startup time, and build time as separate metrics with explicit targets.
- Benchmark optimized builds and realistic inputs; debug-profile timing is not evidence of production performance.
- Read collection, I/O, and concurrency references before adding caches, buffers, threads, locks, channels, or async tasks because each exchanges one resource cost for another.
- Tune Cargo profiles and rustc options experimentally on every supported target; higher optimization and smaller-size settings are not universally faster or smaller.
- Prefer stable documentation matching the repository's toolchain, and verify feature availability before using beta, nightly, or target-specific APIs.
