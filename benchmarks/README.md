# Benchmarks: this project's VM vs. reference Lua

Compares `crates/vm` (this project's fork of
[piccolo](https://github.com/kyren/piccolo), a Lua VM written in Rust - see
`crates/vm/README.md`) against the reference PUC-Rio Lua interpreter, with
LuaJIT included as a bonus third data point.

## Methodology

Each `.lua` script here is run to completion by each interpreter, timed
end-to-end (process startup + compile + execute) with
[hyperfine](https://github.com/sharkdp/hyperfine) - at least 10 runs plus 3
warmup runs per command, reporting mean ± standard deviation. This is a
whole-program comparison (how long does `lua script.lua` actually take to run),
not an internal-clock microbenchmark, so interpreter startup cost is included
identically for every implementation - a fair basis for comparison since that's
how all three are actually invoked. After the individual Hyperfine reports, the
runner prints one consolidated table with every benchmark/runtime mean, standard
deviation, and runtime-to-Sol ratio. Markdown exports include the same summary.

Run it yourself:

```
scripts/benchmark.sh                                   # prints to stdout
scripts/benchmark.sh --export-markdown results.md       # also writes a report
scripts/test-sol-benchmarks.sh                          # coverage + output check
```

Requires `cargo`, `hyperfine`, and a `lua` binary on `PATH`; `luajit` is used
automatically if present, otherwise skipped.

## What each benchmark exercises

| Script              | Exercises                                                                            |
| ------------------- | ------------------------------------------------------------------------------------ |
| `fib.lua`           | Function-call/recursion overhead (naive recursive Fibonacci, no tables/strings)      |
| `loop_sum.lua`      | Raw interpreter dispatch/arithmetic throughput (a single tight numeric loop)         |
| `table_array.lua`   | Table array-part write-then-read throughput                                          |
| `string_concat.lua` | String allocation/GC pressure (repeated `..` concatenation - deliberately quadratic) |
| `nested_loop.lua`   | A second, larger raw-throughput data point (9M iterations via nested loops)          |
| `function_calls.lua` | Direct, non-inlined function-call overhead                                           |
| `function_calls_closure.lua` | Escaping closure-call overhead with one captured value                      |
| `gc_alloc.lua`      | Short-lived record/table allocation and collection pressure                         |
| `hashmap_lookup.lua` | Integer-keyed map/table lookup throughput                                          |
| `matrix.lua`        | Dense matrix multiplication over flat numeric arrays                                 |
| `objects.lua`       | Record/table allocation and field access                                             |

Every Lua benchmark has a same-named typed Sol equivalent except
`function_calls_closure.lua`, whose returned closure requires the heap-backed
captured environments that Sol deliberately still rejects. `any_strict.sol`,
`any_dynamic.sol`, and `vector_add.sol` are typed-only measurements and are
included by the runner in a separate Sol-only pass.

## Results (one measured run, Apple Silicon Mac; regenerate for your own hardware)

| Benchmark     | lua (reference) | vm (this project) |  vm ÷ lua | luajit (bonus) |
| ------------- | --------------: | ----------------: | --------: | -------------: |
| fib           |        125.3 ms |          631.1 ms | **5.04×** |        16.5 ms |
| loop_sum      |         78.7 ms |          400.4 ms | **5.09×** |        23.6 ms |
| nested_loop   |         38.3 ms |          187.9 ms | **4.91×** |         5.9 ms |
| string_concat |         19.3 ms |           90.2 ms | **4.67×** |        17.9 ms |
| table_array   |         59.3 ms |          295.2 ms | **4.98×** |        21.5 ms |

`vm ÷ lua` is the mean time ratio (this project's VM's mean ÷ reference Lua's
mean) - lower is better for `vm`. The striking thing isn't any single number,
it's the _consistency_: every benchmark, despite exercising completely different
VM subsystems (calls, arithmetic, tables, strings), lands at almost exactly
**5×** the reference interpreter's time. That points to a fixed per-opcode
dispatch overhead rather than a weakness in any one area (table handling, string
handling, etc.) - unsurprising for an unoptimized tree-of-enums bytecode
interpreter written in safe Rust compared against PUC-Lua's decades-tuned,
hand-optimized C interpreter, and very far from a JIT (LuaJIT is itself 5-40×
faster than reference Lua here). Full per-benchmark statistics (min/max/σ) are
reproduced below from the same run.

### fib.lua

| Command             |    Mean [ms] | Min [ms] | Max [ms] |     Relative |
| :------------------ | -----------: | -------: | -------: | -----------: |
| `lua (reference)`   |  125.3 ± 2.5 |    121.6 |    131.4 |  7.58 ± 0.94 |
| `luajit`            |   16.5 ± 2.0 |     13.8 |     27.1 |         1.00 |
| `vm (this project)` | 631.1 ± 87.9 |    586.3 |    868.5 | 38.14 ± 7.08 |

### loop_sum.lua

| Command             |   Mean [ms] | Min [ms] | Max [ms] |     Relative |
| :------------------ | ----------: | -------: | -------: | -----------: |
| `lua (reference)`   |  78.7 ± 3.0 |     74.9 |     88.2 |  3.34 ± 0.49 |
| `luajit`            |  23.6 ± 3.4 |     20.7 |     48.1 |         1.00 |
| `vm (this project)` | 400.4 ± 5.6 |    394.4 |    413.8 | 16.99 ± 2.43 |

### nested_loop.lua

| Command             |   Mean [ms] | Min [ms] | Max [ms] |     Relative |
| :------------------ | ----------: | -------: | -------: | -----------: |
| `lua (reference)`   |  38.3 ± 7.3 |     34.1 |     86.6 |  6.48 ± 2.03 |
| `luajit`            |   5.9 ± 1.5 |      4.3 |     11.5 |         1.00 |
| `vm (this project)` | 187.9 ± 5.5 |    180.3 |    196.1 | 31.81 ± 7.95 |

### string_concat.lua

| Command             |   Mean [ms] | Min [ms] | Max [ms] |    Relative |
| :------------------ | ----------: | -------: | -------: | ----------: |
| `lua (reference)`   |  19.3 ± 6.8 |     14.0 |     57.3 | 1.07 ± 0.41 |
| `luajit`            |  17.9 ± 2.7 |     15.3 |     35.1 |        1.00 |
| `vm (this project)` | 90.2 ± 16.5 |     76.7 |    165.8 | 5.03 ± 1.18 |

### table_array.lua

| Command             |   Mean [ms] | Min [ms] | Max [ms] |     Relative |
| :------------------ | ----------: | -------: | -------: | -----------: |
| `lua (reference)`   |  59.3 ± 2.5 |     54.9 |     65.2 |  2.75 ± 0.30 |
| `luajit`            |  21.5 ± 2.1 |     18.3 |     29.0 |         1.00 |
| `vm (this project)` | 295.2 ± 3.4 |    288.6 |    299.3 | 13.70 ± 1.37 |
