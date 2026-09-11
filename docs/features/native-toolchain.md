# Native toolchain and performance tooling

> Status: SIMD specialization, profile-guided warm start, AOT, FFI, and
> introspection tooling are implemented with the platform limits below.

**Purpose**: implement the remaining items `faster_lua.md` frames as the actual
differentiators for beating LuaJIT specifically on numeric workloads (§21,
§41-42), plus practical-adoption features (§23, §25).

**Prerequisite**: vectorization and PGO require a mature optimizer pipeline
and real profiling data to act on.

- [x] **Loop vectorization** (§21): `codegen.rs`'s
      `try_vectorize_elementwise_loop` recognizes exactly `for i = start,
      stop do out[i] = a[i] OP b[i] end` (step 1, `out`/`a`/`b` all
      `Array<f64>`, `OP` one of `+ - * /`) - a narrow pattern match, not
      general auto-vectorization, matching the optimizer's bounds-check-
      elimination precedent. Emits Cranelift's own vector type (`F64X2` -
      portable IR, no hand-rolled per-ISA intrinsics needed; lowers to
      NEON on this session's ARM64 host, and should lower to SSE2 on
      x86-64 the same way, not independently verified there) 2 lanes at a
      time, with a scalar tail for any leftover odd element and a single
      whole-range bounds check up front (sound here specifically because
      the matched body has no side effects besides the three array
      accesses). Verified: odd/even lengths, all four operators,
      out-of-bounds still traps, and the emitted assembly actually
      contains a `.2d` vector op (`tests/programs.rs`). Measured: a real
      but modest ~1.06-1.10× on `benchmarks/vector_add.sol` - the loop is
      memory-bandwidth-bound, not compute-bound, so halving arithmetic
      instruction count doesn't halve wall-clock time; see
      `benchmarks/RESULTS.md`'s native-toolchain section for the honest accounting.
- [x] **CPU-aware codegen** (§24): confirmed, not assumed -
      `jit::Jit::new`/`SOL_TARGET_INFO`/`sol run --target-info` prints
      every ISA-specific setting `cranelift_native::builder()` (what
      `JITBuilder::with_flags` calls internally) actually detected for the
      host CPU. Empirically confirmed on this session's ARM64 (Apple Silicon)
      host: `has_lse=1 has_pauth=1 has_fp16=1 has_dotprod=1` - real detected
      extensions, not a generic fallback profile (ARM64's NEON is
      unconditional in Cranelift's AArch64 backend, not a detected extension,
      so there's no separate flag for it to check). x86-64
      (SSE2/AVX2/AVX-512) not independently re-confirmed in this session - no
      x86-64 hardware was available, and this repo has no CI to add a second
      target to (`find .github/workflows` - none exist) - but the detection
      path itself (`cranelift_native::builder()`) is architecture-generic
      code, not something implemented separately per host, so this is a
      real, if partial, confirmation rather than a pure assumption.
- [x] **Profile-guided optimization** (§23), scoped down: `sol run
      --profile-out <file>` dumps which functions actually got promoted/
      speculatively-specialized during that run (`tier::Engine::dump_profile`,
      reusing the live tiering state - `interp::Runtime::
      native_function_ids`/`specialized_speculative_ids`, no new tracking
      needed); `sol run --profile-in <file>` preloads that list at
      startup (`jit::promote`/`promote_speculative` called immediately,
      `interp::Runtime::preload_speculative`/`preload_native` seed the
      slots directly), skipping the interpreted warm-up that produced it
      the first time. This is a real, working slice of §23 - not the full
      vision (no type-distribution/branch-probability/allocation-site
      profiling, and it doesn't yet feed `sol build`'s eager AOT
      compile at all, since AOT has no warm-up to skip and no
      speculative-guard mechanism of its own to inform - see "Remaining
      remaining work section for what a real AOT-targeted PGO would need). Verified:
      forcing `SOL_PROMOTE_THRESHOLD` impossibly high on the *replay*
      run and confirming (via `--jit-log`) that the profile alone still
      promotes `fib`/`main` immediately.
- [x] **AOT compilation to a standalone binary** (§22): `sol build
      program.sol -o program` (`aot.rs`) - `cranelift-object`'s
      `ObjectModule` instead of `cranelift-jit`'s `JITModule`; `codegen.rs`
      needed zero changes (`compile_function`/`declare_functions`/etc.
      already take `&mut dyn Module`). Every function compiles eagerly, no
      tiering - a standalone binary has no interpreter to fall back to. A
      small hand-written C-ABI `main` (not `codegen.rs`'s general
      expression translator - fixed and simple enough not to need it)
      calls the compiled sol `main` and prints its result via a new
      `sol_print_{i64,f64,bool}` helper. The crate now also builds as
      a `staticlib` (`Cargo.toml`'s `[lib]`) exposing `runtime.rs`'s
      `#[no_mangle]` alloc/GC/print functions, which the generated object
      file is linked against via the system `cc`. Needed `is_pic=true` for
      the AOT ISA (unlike the JIT path, which explicitly wants
      `is_pic=false`) - macOS's linker rejects text relocations in a PIE
      main executable. Verified: arrays, structs+GC, `any`/boxing, and
      real FFI all produce identical output to `sol run`, traps still
      exit non-1, and the produced binary is genuinely standalone (`otool
      -L` shows only `libSystem`) - `benchmarks/table_array.sol` AOT'd
      slightly *beats* the tiered JIT (no interpreter warmup at all) and
      is ~1.5x faster than LuaJIT. Only tested on macOS ARM64 this
      session - Linux linking may need extra system libs not yet verified.
- [x] **FFI** (§25): `extern function name(params): T` declares a native
      symbol (typically libc/libm), resolved via `dlsym` at JIT-finalize
      time - `cranelift-jit`'s own unresolved-import fallback does the
      lookup, no extra dependency needed. Calls compile to a plain direct
      call, identical to calling another sol function - `codegen.rs`
      needed no changes at all. Scoped down from §25's `ffi.load("libc")`
      table-returning style: no explicit per-library `dlopen` (resolution
      is against the process's already-loaded global symbol table, which
      always includes libc/libm), and only `i64`/`f64`/`bool` cross the FFI
      boundary (matching the original scalar-boxing precedent). Verified
      calling real `sqrt`/`pow` from both the interpreter and
      promoted-native tiers (`tests/fixtures/ffi_libm.sol`).
- [x] **First-class profiling/introspection tooling** (§32): `sol run
      --dump-ir`/`--dump-asm`/`--jit-log`/`--target-info` (`main.rs`'s flag
      parser sets the same `SOL_*` env vars `jit.rs` already read since
      optimization and tiering work - a documented, discoverable CLI surface over what were ad hoc
      debug prints, not a new mechanism). `--dump-asm` is new: Cranelift's own
      `VCode` textual form (`Context::set_disasm`/`CompiledCode::vcode`) -
      genuinely close to real assembly (the lowered-to-machine-instructions
      representation right before binary emission) but honestly not a
      byte-level disassembly of the emitted bytes (that would need
      `cranelift-codegen`'s capstone-backed `disas` feature - not worth the
      extra dependency here). `--jit-log` is new too: logs every actual
      promotion/OSR/speculative-specialization event as it happens.
- [x] **Full benchmark suite** (§33), scoped to what sol's language can
      actually run: added `benchmarks/{function_calls,objects,matrix}.{fl,lua}`
      (§33's `03_function_calls`/`04_objects`/`14_matrix`) - `02_fib`,
      `05_arrays`, `08_gc` already existed. Five of the doc's 15 categories
      are **structurally unreachable**, not just unwritten - `06_strings`,
      `07_hashmaps`, `09_coroutines`, `11_json`, `15_http` all need a
      language feature sol doesn't have (no strings, no hash tables, no
      coroutines, no I/O - see `docs/sol.md`). `10_parser` (compiler
      throughput) is out of scope for "beat LuaJIT" per se. Results (see
      `benchmarks/RESULTS.md`'s native-toolchain section): Sol beats LuaJIT by
      **2.51×** on `function_calls` and **1.14×** on `matrix`, but LuaJIT
      wins `objects` by **1.46×** - an honest exception, not swept under
      the rug: LuaJIT's table allocator beats sol's simpler
      conservative GC on pure small-struct
      allocation churn. Also reports peak RSS and p95 latency for the three
      new benchmarks (sol uses ~2-2.5× the memory LuaJIT does, not yet
      investigated) - `scripts/benchmark.sh` itself doesn't automate these
      two metrics for the whole suite yet, a real scope cut, not silently
      dropped.

**Remaining work - AOT-targeted PGO design note**: a fuller §23 would let
`sol build --profile app.prof app.sol` bake speculative-`any`-
parameter specialization into the *eager* AOT compile (which today just
emits the general, always-boxed-check path for `any` parameters, with no
tiering to specialize into). The natural shape: compile a function with a
profile-confirmed candidate as one body with an internal branch on the
incoming tag - a fast block (the specialized, `Unbox`-free path) and a
general block (today's path) - rather than the JIT's two-separate-
compiled-functions-plus-runtime-dispatch approach, since AOT has no
`interp::Runtime` object to hold a guard in. Not built this pass.

**Explicitly out of scope** (per `faster_lua.md` §26-27's own
framing as "architecture, not the optimization strategy", and §35's explicit
deprioritization): stackless coroutines and structured exception handling are
real, valid future work, but the document itself doesn't treat them as part of
the performance story - revisit only if sol gains users who need them, not
as part of "beat LuaJIT."

**Files**: `main.rs` (CLI flags/subcommands), `jit.rs` (`--dump-asm`/
`--jit-log`/`--target-info`, `link_externs`), `lexer.rs`/`ast.rs`/
`parser.rs`/`types.rs`/`typeck.rs` (`extern function` declarations -
`ExternFunction`/`TExternFunction`), `bccompile.rs` (unified
function/extern id space), `tier.rs` (extern slots seeded as
always-native), `aot.rs` (new - the `ObjectModule`-based AOT compiler),
`lib.rs` (new - the crate is now a library + binary, so `aot.rs` can build
against `runtime.rs` as a real staticlib), `runtime.rs`
(`#[no_mangle]`, new print/GC-init helpers), `Cargo.toml`
(`cranelift-object`, `cranelift-native`, `[lib]` `crate-type`),
`interp.rs`/`tier.rs` (`Profile`, `--profile-out`/`--profile-in`),
`codegen.rs` (`try_vectorize_elementwise_loop`, `detect_vectorizable_loop`),
`benchmarks/{function_calls,objects,matrix}.{fl,lua}`. No separate
`src/ffi.rs`, `src/tools/`, `src/pgo.rs`, or `src/vectorize.rs` module
ended up needed - every item extended an existing file.
