Yes. **Lua 5.5.1 and LuaJIT are substantially different from the Lua 5.4 picture
I described earlier**, especially at the bytecode level.

The most useful way to compare them is:

```
Lua 5.5.1

Lua source
    ↓
parser/compiler
    ↓
Proto
    ↓
32-bit interpreter bytecode
    ↓
interpreter

LuaJIT 2.1

Lua source
    ↓
parser/compiler
    ↓
LuaJIT bytecode
    ↓
interpreter
    ↓
hot code
    ↓
trace recorder
    ↓
JIT compiler
    ↓
native machine code
```

Lua 5.5.1 is the current PUC-Rio Lua release; it was released in August 2026.
The official source identifies 5.5.1 as the current 5.5 release. Lua+1

The fascinating part is that **Lua 5.5 actually changed the bytecode instruction
formats compared with 5.4**, while LuaJIT has an entirely different bytecode
architecture.

---

# 1\. Lua 5.5.1: the instruction word

Lua 5.5.1 still uses **32-bit instructions**, but its format is more
sophisticated than the 5.4 format.

The official `lopcodes.h` specifies:

```
32-bit instruction

iABC
┌──────┬──────┬─┬──────┬───────┐
│  C8  │  B8  │k│  A8  │ Op7   │
└──────┴──────┴─┴──────┴───────┘

ivABC
┌────────┬──────┬─┬──────┬───────┐
│ vC(10) │vB(6) │k│  A8  │ Op7   │
└────────┴──────┴─┴──────┴───────┘

iABx
┌─────────────────┬──────┬───────┐
│      Bx(17)     │ A8   │ Op7   │
└─────────────────┴──────┴───────┘

iAsBx
┌─────────────────┬──────┬───────┐
│    sBx(17)      │ A8   │ Op7   │
└─────────────────┴──────┴───────┘

iAx
┌─────────────────────────┬───────┐
│          Ax(25)         │ Op7   │
└─────────────────────────┴───────┘

isJ
┌─────────────────────────┬───────┐
│         sJ(25)          │ Op7   │
└─────────────────────────┴───────┘
```

That's directly from the 5.5.1 source. Lua

There are two particularly important differences from the older representation:

- `ivABC` is a **variant ABC format**.
- `isJ` provides a **25-bit signed jump displacement**.

That is already a clue that Lua 5.5 is trying to make some operations more
compact.

---

# 2\. The opcode is still only 7 bits

Lua 5.5 keeps:

```
opcode = 7 bits
```

So there can be up to:

```
2^7 = 128
```

opcode values.

The remaining 25 bits are distributed differently depending on the instruction.

For ordinary ABC:

```
31                         0
┌────────┬────────┬─┬───────┐
│ C (8)  │ B (8)  │k│ A(8) │
└────────┴────────┴─┴───────┘
                     │
                     └── constant-related flag
```

This is interesting because Lua 5.5 still gets **three operands plus an extra
bit** into one 32-bit word.

---

# 3\. The new `ivABC` format is particularly interesting

Lua 5.5 introduces:

```
ivABC
```

with:

```
A = 8 bits
vB = 6 bits
vC = 10 bits
k = 1 bit
opcode = 7 bits
```

The official source calls these "variant" fields. Lua

Why would you want that?

Because not every instruction needs:

```
B = 8 bits
C = 8 bits
```

Sometimes you want:

```
small B
large C
```

or vice versa.

This lets an instruction specialize its operand widths without going to a second
instruction.

It's a very compiler/VM-oriented optimization.

---

# 4\. Lua 5.5 has more specialized arithmetic bytecodes

Look at the opcode list:

```
ADDI
ADDK
SUBK
MULK
MODK
POWK
DIVK
IDIVK

BANDK
BORK
BXORK

SHLI
SHRI

ADD
SUB
MUL
MOD
POW
DIV
IDIV
...
```

This is quite revealing. Lua

Instead of having one generic:

```
ADD
```

and making every operand combination go through the same machinery, Lua 5.5 has
specialized operations such as:

```
ADDI
ADDK
ADD
```

Conceptually:

```
ADDI
R[A] = R[B] + immediate_integer

ADDK
R[A] = R[B] + constant

ADD
R[A] = R[B] + R[C]
```

The actual operand semantics should be read from `lopcodes.h`/`lvm.c`, but
that's the architectural idea.

This is a very important evolution from the conceptual model I gave you earlier.

---

# 5\. Why specialize `ADDI`?

Suppose the source is:

```
x = x + 1
```

A generic VM might generate:

```
LOADK   R2 1
ADD     R0 R0 R2
```

Lua 5.5 can instead represent the operation more directly:

```
ADDI    R0 R0 1
```

So:

```
source
    ↓
x = x + 1
    ↓
one VM instruction
    ↓
ADDI
```

That reduces:

- bytecode size,
- instruction dispatch,
- temporary-register usage.

The 5.5 compiler explicitly contains logic for these specialized arithmetic
operations. Lua

---

# 6\. `ADDK` is the other important specialization

Consider:

```
x = x + 100
```

The compiler can represent:

```
ADDK
```

rather than materializing `100` into a register first.

Conceptually:

```
ADDK R0 R0 K
```

becomes:

```
R0 = R0 + constants[K]
```

So Lua 5.5's bytecode is increasingly expressing **the shape of the operation**
rather than forcing everything through generic register-register instructions.

---

# 7\. Lua 5.5 also has `EQ`, `EQK`, `EQI`, etc.

The same principle applies to comparisons:

```
EQ
EQK
EQI

LT
LTI
LE
LEI
GTI
GEI
```

So:

```
if x == 10 then
```

can use a comparison specialized for an integer literal instead of:

```
LOADK
EQ
```

Again, the VM gets more work done per instruction.

The 5.5 opcode table explicitly contains these specialized forms. Lua

---

# 8\. `isJ` makes jumps different

Lua 5.4 used:

```
sBx = 17 bits
```

for signed jumps.

Lua 5.5 adds:

```
sJ = 25 bits
```

So a jump can use essentially the whole operand area:

```
┌─────────────────────────┬───────┐
│       signed 25-bit     │ Op7   │
│       displacement      │       │
└─────────────────────────┴───────┘
```

That gives a vastly larger jump range than a 17-bit displacement.

This matters for large functions and generated code.

The 5.5 source explicitly defines `isJ` alongside the other formats. Lua

---

# 9\. Lua 5.5's opcode set

A simplified grouping looks like:

```
Data movement
─────────────
MOVE
LOADI
LOADF
LOADK
LOADKX
LOADFALSE
LFALSESKIP
LOADTRUE
LOADNIL

Upvalues/globals/tables
───────────────────────
GETUPVAL
SETUPVAL
GETTABUP
GETTABLE
GETI
GETFIELD
SETTABUP
SETTABLE
SETI
SETFIELD

Objects
───────
NEWTABLE
SELF

Arithmetic
──────────
ADDI
ADDK
SUBK
MULK
MODK
POWK
DIVK
IDIVK
ADD
SUB
MUL
...

Bitwise
───────
BANDK
BORK
BXORK
SHLI
SHRI
BAND
BOR
BXOR
SHL
SHR

Unary
─────
UNM
BNOT
NOT
LEN

Control flow
────────────
JMP
EQ
LT
LE
...
TEST
TESTSET

Calls
─────
CALL
TAILCALL
RETURN
RETURN0
RETURN1

Loops
─────
FORLOOP
FORPREP
TFORPREP
TFORCALL
TFORLOOP

Functions
─────────
CLOSURE
VARARG
GETVARG
VARARGPREP

Misc
────
CLOSE
TBC
SETLIST
ERRNNIL
EXTRAARG
```

This isn't just a cosmetic reorganization: the specialized opcode set is part of
how Lua 5.5 keeps the interpreter efficient. Lua

---

# 10\. Lua 5.5's bytecode isn't just "register machine"

A better description is:

> **Lua 5.5 is a compact, statically encoded, register-oriented instruction set
> with specialized operand forms.**

The compiler is doing quite a lot of work.

For example:

```
x = a + 1
```

can be lowered directly into an operation whose operands say:

```
destination = register
source = register
literal = immediate integer
```

instead of constructing an artificial expression stack.

That's a fairly sophisticated little instruction set.

---

# 11\. Now LuaJIT

LuaJIT is a completely different beast.

Its bytecode instruction is also **32 bits**, but the layout is simpler:

```
ABC:

31          24 23      16 15       8 7        0
┌────────────┬──────────┬──────────┬──────────┐
│     B      │    C     │    A     │    OP    │
│   8 bits   │ 8 bits   │ 8 bits   │ 8 bits   │
└────────────┴──────────┴──────────┴──────────┘
```

LuaJIT also has:

```
AD:

31                         16 15       8 7       0
┌───────────────────────────┬──────────┬─────────┐
│             D             │    A     │   OP    │
│          16 bits          │ 8 bits   │ 8 bits  │
└───────────────────────────┴──────────┴─────────┘
```

And a jump variant:

```
AJ
```

where `D` is interpreted as a signed, biased jump offset.

The LuaJIT source defines this directly in `lj_bc.h`. GitHub

---

# 12\. Notice the major difference: LuaJIT has an 8-bit opcode

Lua 5.5:

```
opcode = 7 bits
```

LuaJIT:

```
opcode = 8 bits
```

So LuaJIT gets:

```
256 possible opcodes
```

rather than Lua's:

```
128 possible opcodes
```

And LuaJIT takes advantage of this.

Its bytecode instruction layout is essentially:

```
[ OP ][ A ][ C ][ B ]
```

when viewed from least-significant to most-significant bits.

The source defines:

```
bc_op(i)
bc_a(i)
bc_b(i)
bc_c(i)
bc_d(i)
```

for extracting these fields. GitHub

---

# 13\. LuaJIT's operands have semantic modes

This is where LuaJIT becomes really interesting.

The bytecode isn't merely:

```
OP A B C
```

LuaJIT also maintains metadata describing what each operand means.

For example, an operand can be:

```
BCMvar
BCMstr
BCMnum
BCMpri
BCMlit
BCMfunc
BCMtab
BCMjump
BCMuv
...
```

The source calls these **bytecode operand modes**. GitHub

So an instruction might have an operand that means:

```
variable/register
```

or:

```
string constant
```

or:

```
numeric constant
```

or:

```
primitive
```

or:

```
jump offset
```

This is much more explicit than merely saying:

```
operand B
```

---

# 14\. LuaJIT bytecode is heavily optimized for the JIT

This is the crucial difference.

Stock Lua's bytecode is primarily an **interpreter target**.

LuaJIT's bytecode is both:

```
interpreter input
```

and:

```
JIT compiler input
```

So the design is influenced by what the trace recorder wants to see.

The pipeline is:

```
                  Lua source
                      │
                      ▼
                LuaJIT compiler
                      │
                      ▼
                  bytecode
                      │
             ┌────────┴────────┐
             ▼                 ▼
        interpreter       trace recorder
                               │
                               ▼
                          IR / traces
                               │
                               ▼
                         JIT compiler
                               │
                               ▼
                       native machine code
```

This fundamentally changes how you should think about LuaJIT bytecode.

---

# 15\. LuaJIT bytecode has instructions like `ISLT`, `ISEQV`, etc.

LuaJIT's opcode vocabulary is quite different.

For comparisons, it has things such as:

```
ISLT
ISGE
ISLE
ISGT

ISEQV
ISNEV
ISEQS
ISNES
ISEQN
ISNEN
ISEQP
ISNEP
```

Notice the specialization:

```
ISEQV   variable == variable
ISEQS   variable == string
ISEQN   variable == number
ISEQP   variable == primitive
```

This is very different from a minimalist:

```
EQ
```

instruction.

The bytecode itself tells the interpreter/JIT what kind of operation is being
performed. GitHub

---

# 16\. LuaJIT's type specialization goes further

Look at:

```
ISEQV
ISEQS
ISEQN
ISEQP
```

The suffix communicates operand type.

Likewise LuaJIT has many instructions with suffixes indicating operand
interpretation.

The metadata in `lj_bc.h` describes operand modes and metamethod behavior.
GitHub

This is very useful for the JIT because it can reason about operations without
rediscovering as much information.

---

# 17\. LuaJIT has interpreter-specific and JIT-specific opcodes

This is one of the strangest things if you're coming from ordinary Lua.

Look at:

```
FUNCF
IFUNCF
JFUNCF

FUNCV
IFUNCV
JFUNCV

FUNCC
FUNCCW
```

and:

```
FORL
IFORL
JFORL

ITERL
IITERL
JITERL

LOOP
ILOOP
JLOOP
```

The prefixes encode different execution contexts.

Roughly:

```
normal interpreter
I... = interpreter-specific variant
J... = JIT-specific variant
```

The exact semantics depend on the opcode.

This is a consequence of LuaJIT's architecture: the bytecode isn't merely an
abstract language instruction set; it is tightly connected to the interpreter
and tracing JIT implementation.

The source explicitly comments on this for loop/function instructions. GitHub

---

# 18\. `LOOP` is particularly important

LuaJIT needs to identify hot loops.

Conceptually:

```
        ┌─────────────┐
        │             │
        ▼             │
      LOOP ───────────┘
        │
        ▼
   execute body
```

When a loop becomes hot enough, LuaJIT's recorder can start recording it.

So bytecode contains explicit loop-related instructions that serve not merely
language semantics but also JIT machinery.

That's a huge architectural distinction from stock Lua.

---

# 19\. LuaJIT's bytecode is closer to a JIT IR front-end

Not literally an IR—the actual JIT IR comes later—but conceptually:

```
Lua source
    ↓
LuaJIT bytecode
    ↓
trace recording
    ↓
SSA-like JIT IR
    ↓
machine code
```

So you can think of LuaJIT bytecode as an **execution-oriented intermediate
representation**.

The interpreter can execute it directly, while the trace recorder uses it as its
source language.

---

# 20\. LuaJIT doesn't JIT arbitrary bytecode functions wholesale

This is another important distinction.

A traditional method might be:

```
function
   ↓
compile entire function
   ↓
machine code
```

LuaJIT's classic architecture is **trace based**.

For example:

```
for i = 1, 1000000 do
    x = x + i
end
```

initially executes as bytecode.

Eventually LuaJIT notices a hot path:

```
LOOP
 ↓
body
 ↓
FORL
 └──────────┐
            │
            └── back edge
```

The recorder follows that path and builds a trace.

Conceptually:

```
bytecode path

A → B → C → D → B
          │
          ▼
       hot loop

          ↓

trace

T1:
  load i
  load x
  add
  store x
  increment i
  branch
```

Then the JIT compiler turns that trace into machine code.

---

# 21\. Side exits

This is another concept you need to understand to really understand LuaJIT.

Suppose the JIT compiled the assumption:

```
x is a number
```

but later:

```
x = "hello"
```

The machine code can't blindly continue.

Instead, it has a **guard**:

```
if x is number
    continue native code
else
    side exit
```

Conceptually:

```
        JIT trace
            │
        type guard
       /         \
    yes           no
     │            │
     ▼            ▼
native code     side exit
                    │
                    ▼
                interpreter
```

This is one of the fundamental mechanisms that makes a tracing JIT work.

---

# 22\. LuaJIT bytecode vs Lua 5.5 bytecode

Here's the high-level comparison:

| Property             | Lua 5.5.1               | LuaJIT 2.1              |
| -------------------- | ----------------------- | ----------------------- |
| Instruction size     | 32 bits                 | 32 bits                 |
| Opcode bits          | 7                       | 8                       |
| Register-based       | Yes                     | Yes                     |
| ABC format           | Yes                     | Yes                     |
| Extended operands    | Several formats         | AD/AJ                   |
| Specialized ops      | Extensive               | Extensive               |
| Interpreter          | Yes                     | Yes                     |
| JIT                  | No                      | Yes                     |
| Trace-based          | No                      | Yes                     |
| JIT-aware bytecode   | No                      | Yes                     |
| Operand modes        | relatively simple       | explicit semantic modes |
| Main execution model | bytecode interpretation | bytecode + tracing JIT  |

Lua 5.5's actual instruction layouts are defined in its `lopcodes.h`; LuaJIT's
are defined in `lj_bc.h`. Lua+1

---

# 23\. The really interesting difference: compiler philosophy

Lua 5.5:

```
                 compiler
                    │
                    ▼
           efficient bytecode
                    │
                    ▼
              interpreter
```

LuaJIT:

```
                 compiler
                    │
                    ▼
             JIT-friendly
               bytecode
                    │
            ┌───────┴────────┐
            ▼                ▼
       interpreter       trace recorder
                              │
                              ▼
                         JIT compiler
```

That changes what "good bytecode" means.

For Lua 5.5, you primarily want:

```
compact
easy to decode
fast to interpret
```

For LuaJIT, you additionally want:

```
easy to trace
easy to specialize
easy to map to IR
easy to attach guards
```

---

# 24\. One striking LuaJIT design choice

LuaJIT uses:

```
A = 8 bits
B = 8 bits
C = 8 bits
```

or:

```
A = 8 bits
D = 16 bits
```

That means the register file directly addressable by an instruction is
essentially:

```
256 slots
```

because:

```
BCMAX_A = 0xff
BCMAX_B = 0xff
BCMAX_C = 0xff
```

The official source defines exactly those limits. GitHub

This makes the instruction decoder extremely cheap:

```
op = i & 0xff;
a  = (i >> 8) & 0xff;
c  = (i >> 16) & 0xff;
b  = i >> 24;
```

That's wonderfully simple.

---

# 25\. Compare that with Lua 5.5

Lua 5.5 has:

```
opcode = 7 bits
A      = 8 bits
B/C    = variable
```

and uses several instruction formats:

```
iABC
ivABC
iABx
iAsBx
iAx
isJ
```

LuaJIT instead mostly has:

```
ABC
AD
AJ
```

The LuaJIT instruction decoder is therefore structurally simpler.

The complexity is pushed elsewhere:

```
Lua 5.5:
    complex instruction encoding
    relatively simple interpreter

LuaJIT:
    simple instruction encoding
    sophisticated JIT/trace machinery
```

That's a useful architectural contrast.

---

# 26\. LuaJIT's `BCIns` is literally a 32-bit word

The source defines:

```
typedef uint32_t BCIns;
```

and:

```
typedef uint32_t BCPos;
typedef uint32_t BCReg;
```

for bytecode instructions, positions, and registers. GitHub+1

So:

```
BCIns
```

is essentially:

```
uint32_t
```

and:

```
bytecode array
```

is a contiguous array of these 32-bit instructions.

---

# 27\. LuaJIT's bytecode isn't serialized exactly like PUC Lua's

Another major distinction.

PUC Lua has:

```
source
 ↓
Proto
 ↓
luac serialization
 ↓
binary chunk
```

LuaJIT has its own bytecode/chunk representation.

You shouldn't expect:

```
Lua 5.5 .luac
```

to be readable by:

```
LuaJIT
```

or vice versa.

They're different virtual machines with different object models, instruction
sets, and binary formats.

Lua itself explicitly notes that different Lua versions can have different VMs
and that precompiled chunks are not expected to be cross-version compatible. Lua

---

# 28\. If you're reverse-engineering them

For **Lua 5.5.1**, read these in order:

```
lopcodes.h
    ↓
lopnames.h
    ↓
lcode.c
    ↓
lobject.h
    ↓
lvm.c
    ↓
ldump.c / lundump.c
```

The official 5.5 source tree exposes all of these directly. Lua

For **LuaJIT**, I'd use:

```
lj_bc.h
    ↓
lj_bcdef.h
    ↓
lj_parse.c
    ↓
lj_record.c
    ↓
lj_ir.c
    ↓
lj_asm_*.c
    ↓
lj_vm_*.dasc
```

The key conceptual transition is:

```
bytecode
   ↓
trace recording
   ↓
IR
   ↓
assembly
```

rather than simply:

```
bytecode
   ↓
switch(opcode)
```

---

# 29\. The most important distinction

If you're learning VM implementation, I would summarize them this way:

### Lua 5.5.1

```
          ┌─────────────┐
source →  │   compiler  │
          └──────┬──────┘
                 │
                 ▼
        ┌─────────────────┐
        │ 32-bit bytecode │
        │                 │
        │ register VM     │
        │ specialized ops │
        └────────┬────────┘
                 │
                 ▼
             interpreter
```

The bytecode is the **final execution representation**.

### LuaJIT

```
          ┌─────────────┐
source →  │   compiler  │
          └──────┬──────┘
                 │
                 ▼
        ┌─────────────────┐
        │ 32-bit bytecode │
        └────────┬────────┘
                 │
          ┌──────┴──────┐
          ▼             ▼
     interpreter    trace recorder
                        │
                        ▼
                    JIT IR
                        │
                        ▼
                  native code
```

The bytecode is **an intermediate execution representation feeding two
engines**.

That's the fundamental architectural difference.

And if your eventual goal is to **design your own VM**, Lua 5.5 is probably the
cleaner system to study first, while LuaJIT is much more instructive once you
want to understand **tracing JITs, guards, side exits, specialization, and how a
bytecode interpreter becomes the front-end to a native-code compiler**.
