//! Bytecode instruction execution for the Lua frame trampoline.

use super::*;

impl LuaRuntime {
    /// Runs `frame` from its current `pc` (after first resolving `incoming`
    /// against `frame.pending`, if this is a resumed frame rather than a
    /// fresh one) until it either returns (`StepResult::Done`) or issues
    /// another `Instr::Call` (`StepResult::PushClosure`/`CallLeaf`). A
    /// near-mechanical transform of the old `run_proto` dispatch loop: every
    /// `regs`/`cells`/`varargs` local becomes a `frame` field, and the single
    /// native-recursion point (`Instr::Call`) becomes an early return instead
    /// of an inline blocking call.
    pub(super) fn dispatch_step(
        &mut self,
        frame: &mut LuaFrame,
        incoming: Option<Vec<LuaValue>>,
    ) -> LuaResult<StepResult> {
        frame.header.state = FrameState::Running;
        let mut pc = frame.header.pc as usize;
        if let Some(results) = incoming {
            match std::mem::replace(&mut frame.pending, Pending::None) {
                Pending::TailCall => {
                    self.recycle_frame_buffers(
                        std::mem::take(&mut frame.regs),
                        std::mem::take(&mut frame.cells),
                    );
                    self.recycle_values_buffer(std::mem::take(&mut frame.varargs));
                    frame.header.state = FrameState::Returned;
                    return Ok(StepResult::Done(results));
                }
                Pending::Call {
                    base,
                    results: ValueCount::Open,
                } => {
                    let count = results.len();
                    ensure_regs(&mut frame.regs, &mut frame.cells, base + count);
                    for (i, value) in results.into_iter().enumerate() {
                        reg_set(self, &mut frame.regs, &frame.cells, base + i, value);
                    }
                    frame.header.stack_top = (base + count) as u32;
                }
                Pending::Call {
                    base,
                    results: ValueCount::Fixed(count),
                } => {
                    let n = count as usize;
                    ensure_regs(&mut frame.regs, &mut frame.cells, base + n);
                    for i in 0..n {
                        reg_set(self, &mut frame.regs,
                            &frame.cells,
                            base + i,
                            results.get(i).cloned().unwrap_or(LuaValue::Nil),
                        );
                    }
                    frame.header.stack_top = (base + n) as u32;
                }
                Pending::Index { dest } | Pending::Len { dest } | Pending::Unary { dest } => {
                    let value = results.into_iter().next().unwrap_or(LuaValue::Nil);
                    reg_set(self, &mut frame.regs, &frame.cells, dest, value);
                }
                Pending::SetIndex => {}
                Pending::Binary { dest, continuation } => {
                    let raw = results.into_iter().next().unwrap_or(LuaValue::Nil);
                    let value = match continuation {
                        BinaryContinuation::Raw => raw,
                        BinaryContinuation::Bool => LuaValue::Bool(raw.truthy()),
                        BinaryContinuation::BoolNegated => LuaValue::Bool(!raw.truthy()),
                    };
                    reg_set(self, &mut frame.regs, &frame.cells, dest, value);
                }
                Pending::TForCall { base, nvars } => {
                    ensure_regs(&mut frame.regs, &mut frame.cells, base + 3 + nvars);
                    for i in 0..nvars {
                        let value = results.get(i).cloned().unwrap_or(LuaValue::Nil);
                        let dst = base + 3 + i;
                        if frame
                            .proto
                            .captured_registers
                            .get(dst)
                            .copied()
                            .unwrap_or(false)
                        {
                            self.charge_allocation(std::mem::size_of::<LuaValue>(), Some(&*frame))?;
                        }
                        reg_set_fresh(self, &mut frame.regs, &mut frame.cells, dst, value);
                    }
                }
                Pending::None => {
                    unreachable!("dispatch_step: resumed frame must have a pending continuation")
                }
            }
            pc += 1;
        }
        let proto = frame.proto.clone();
        let mut top = frame.header.stack_top as usize;
        'exec: loop {
            self.tick(&*frame)?;
            // Keep the frame header's pc in step with the instruction about
            // to execute, not just the ones that explicitly save it before
            // suspending. Otherwise an error that propagates straight out
            // via `?` (no call/yield boundary) reports whatever pc was last
            // saved at a suspension point, misattributing the source line.
            frame.header.pc = pc as u32;
            self.fire_line_and_count_hooks(frame, pc)?;
            match &proto.instrs[pc] {
                Instr::LoadConst(dst, k) => {
                    reg_set(self, &mut frame.regs,
                        &frame.cells,
                        *dst as usize,
                        const_to_value(&self.canonical_heap, &proto.consts[*k as usize]),
                    );
                }
                Instr::LoadNil(dst) => {
                    reg_set(self, &mut frame.regs, &frame.cells, *dst as usize, LuaValue::Nil);
                }
                Instr::LoadBool(dst, value) => {
                    reg_set(self, &mut frame.regs,
                        &frame.cells,
                        *dst as usize,
                        LuaValue::Bool(*value),
                    );
                }
                Instr::Move(dst, src) => {
                    let value = reg_get(self, &frame.regs, &frame.cells, *src as usize);
                    reg_set(self, &mut frame.regs, &frame.cells, *dst as usize, value);
                }
                Instr::NewLocal(dst, src, _) => {
                    let dst = *dst as usize;
                    let value = reg_get(self, &frame.regs, &frame.cells, *src as usize);
                    if proto.captured_registers.get(dst).copied().unwrap_or(false) {
                        self.charge_allocation(std::mem::size_of::<LuaValue>(), Some(&*frame))?;
                    }
                    reg_set_fresh(self, &mut frame.regs, &mut frame.cells, dst, value);
                }
                Instr::GetUpval(dst, idx) => {
                    let id = frame.upvals[*idx as usize];
                    let encoded = self
                        .canonical_heap
                        .borrow()
                        .upvalue_value(id)
                        .expect("upvalue id must address a live upvalue object");
                    let value = self
                        .decode_value(encoded)
                        .expect("a value already resident in an upvalue must decode cleanly");
                    reg_set(self, &mut frame.regs, &frame.cells, *dst as usize, value);
                }
                Instr::SetUpval(idx, src) => {
                    let value = reg_get(self, &frame.regs, &frame.cells, *src as usize);
                    let id = frame.upvals[*idx as usize];
                    let encoded = self
                        .encode_value(&value)
                        .expect("a value already resident in a register must encode cleanly");
                    self.canonical_heap
                        .borrow_mut()
                        .set_upvalue(id, encoded)
                        .expect("upvalue id must address a live upvalue object");
                }
                Instr::GetEnvironment(dst) => {
                    let value = frame.globals.as_value();
                    reg_set(self, &mut frame.regs, &frame.cells, *dst as usize, value);
                }
                Instr::SetEnvironment(src) => {
                    let value = reg_get(self, &frame.regs, &frame.cells, *src as usize);
                    // `_ENV` is an ordinary upvalue in Lua and can hold any
                    // value. A later global access indexes that value and
                    // produces the usual Lua indexing error if needed.
                    frame.globals.set_value(value);
                }
                Instr::GetGlobal(dst, name) => {
                    // Real Lua desugars every global read to `_ENV.name`, an
                    // ordinary metatable-aware index - so a caller-supplied
                    // `_ENV` (via `load`'s fourth argument) with `__index`
                    // must see it fire. Sol's own `require`-sandboxed scopes
                    // (`has_base`) never carry a caller metatable, so they
                    // keep the cheaper direct `base`-chain path instead.
                    if frame.globals.has_base() {
                        let value = frame.globals.get(self, name);
                        reg_set(self, &mut frame.regs, &frame.cells, *dst as usize, value);
                    } else {
                        let key = LuaValue::String(self.intern_str(name.as_bytes()));
                        match self.index_resolve(frame.globals.as_value(), key)? {
                            IndexResolution::Value(value) => {
                                reg_set(self, &mut frame.regs, &frame.cells, *dst as usize, value);
                            }
                            IndexResolution::Call { method, args } => {
                                frame.header.pc = pc as u32;
                                frame.header.stack_top = top as u32;
                                frame.header.state = FrameState::Suspended;
                                frame.pending = Pending::Index {
                                    dest: *dst as usize,
                                };
                                self.pending_frame_label = Some("index");
                                return self.step_result_for_call(method, args);
                            }
                        }
                    }
                }
                Instr::SetGlobal(name, src, constant, declare) => {
                    let value = reg_get(self, &frame.regs, &frame.cells, *src as usize);
                    if *declare {
                        frame.globals.define(self, name, value, *constant);
                    } else if frame.globals.has_base() {
                        frame.globals.assign(self, name, value)?;
                    } else {
                        frame.globals.check_writable(name)?;
                        let key = LuaValue::String(self.intern_str(name.as_bytes()));
                        match self.set_index_resolve(frame.globals.as_value(), key, value, Some(&*frame))? {
                            SetIndexResolution::Done => {}
                            SetIndexResolution::Call { method, args } => {
                                frame.header.pc = pc as u32;
                                frame.header.stack_top = top as u32;
                                frame.header.state = FrameState::Suspended;
                                frame.pending = Pending::SetIndex;
                                self.pending_frame_label = Some("newindex");
                                return self.step_result_for_call(method, args);
                            }
                        }
                    }
                }
                Instr::ErrorIfGlobalDefined(reg, name) => {
                    let value = reg_get(self, &frame.regs, &frame.cells, *reg as usize);
                    if value != LuaValue::Nil {
                        return Err(LuaError::new(format!("global '{name}' already defined")));
                    }
                }
                Instr::NewTable(dst) => {
                    let table = self.new_table(Some(&*frame))?;
                    reg_set(
                        self,
                        &mut frame.regs,
                        &frame.cells,
                        *dst as usize,
                        LuaValue::Table(table),
                    );
                }
                Instr::NewClosure(dst, idx) => {
                    let child_proto = proto.nested[*idx as usize].clone();
                    let mut child_upvals = Vec::with_capacity(child_proto.upvals.len());
                    for source in &child_proto.upvals {
                        child_upvals.push(match source {
                            UpvalSource::ParentLocal(reg) => frame.cells[*reg as usize].expect(
                                "compiler marks any ParentLocal-captured register as captured",
                            ),
                            UpvalSource::ParentUpval(idx) => frame.upvals[*idx as usize],
                        });
                    }
                    let closure = self.new_closure(child_proto, child_upvals, frame.globals.clone(), Some(&*frame))?;
                    reg_set(
                        self,
                        &mut frame.regs,
                        &frame.cells,
                        *dst as usize,
                        LuaValue::Closure(closure),
                    );
                }
                Instr::GetField(dst, base, name) => {
                    let base_value = reg_get(self, &frame.regs, &frame.cells, *base as usize);
                    let key = LuaValue::String(self.intern_str(name_const(&proto, *name).as_slice()));
                    match self.index_resolve(base_value, key) {
                        Ok(IndexResolution::Value(value)) => {
                            reg_set(self, &mut frame.regs, &frame.cells, *dst as usize, value);
                        }
                        Ok(IndexResolution::Call { method, args }) => {
                            frame.header.pc = pc as u32;
                            frame.header.stack_top = top as u32;
                            frame.header.state = FrameState::Suspended;
                            frame.pending = Pending::Index {
                                dest: *dst as usize,
                            };
                            self.pending_frame_label = Some("index");
                            return self.step_result_for_call(method, args);
                        }
                        Err(mut err) => {
                            annotate_index_error(&mut err, &proto, pc, *base);
                            return Err(err);
                        }
                    }
                }
                Instr::SetField(base, name, src) => {
                    let base_value = reg_get(self, &frame.regs, &frame.cells, *base as usize);
                    let key = LuaValue::String(self.intern_str(name_const(&proto, *name).as_slice()));
                    let value = reg_get(self, &frame.regs, &frame.cells, *src as usize);
                    match self.set_index_resolve(base_value, key, value, Some(&*frame)) {
                        Ok(SetIndexResolution::Done) => {}
                        Ok(SetIndexResolution::Call { method, args }) => {
                            frame.header.pc = pc as u32;
                            frame.header.stack_top = top as u32;
                            frame.header.state = FrameState::Suspended;
                            frame.pending = Pending::SetIndex;
                            self.pending_frame_label = Some("newindex");
                            return self.step_result_for_call(method, args);
                        }
                        Err(mut err) => {
                            annotate_index_error(&mut err, &proto, pc, *base);
                            return Err(err);
                        }
                    }
                }
                Instr::GetIndex(dst, base, index) => {
                    let base_value = reg_get(self, &frame.regs, &frame.cells, *base as usize);
                    let key = reg_get(self, &frame.regs, &frame.cells, *index as usize);
                    match self.index_resolve(base_value, key) {
                        Ok(IndexResolution::Value(value)) => {
                            reg_set(self, &mut frame.regs, &frame.cells, *dst as usize, value);
                        }
                        Ok(IndexResolution::Call { method, args }) => {
                            frame.header.pc = pc as u32;
                            frame.header.stack_top = top as u32;
                            frame.header.state = FrameState::Suspended;
                            frame.pending = Pending::Index {
                                dest: *dst as usize,
                            };
                            self.pending_frame_label = Some("index");
                            return self.step_result_for_call(method, args);
                        }
                        Err(mut err) => {
                            annotate_index_error(&mut err, &proto, pc, *base);
                            return Err(err);
                        }
                    }
                }
                Instr::SetIndex(base, index, src) => {
                    let base_value = reg_get(self, &frame.regs, &frame.cells, *base as usize);
                    let key = reg_get(self, &frame.regs, &frame.cells, *index as usize);
                    let value = reg_get(self, &frame.regs, &frame.cells, *src as usize);
                    match self.set_index_resolve(base_value, key, value, Some(&*frame)) {
                        Ok(SetIndexResolution::Done) => {}
                        Ok(SetIndexResolution::Call { method, args }) => {
                            frame.header.pc = pc as u32;
                            frame.header.stack_top = top as u32;
                            frame.header.state = FrameState::Suspended;
                            frame.pending = Pending::SetIndex;
                            self.pending_frame_label = Some("newindex");
                            return self.step_result_for_call(method, args);
                        }
                        Err(mut err) => {
                            annotate_index_error(&mut err, &proto, pc, *base);
                            return Err(err);
                        }
                    }
                }
                Instr::SetArrayItem(base, array_index, src) => {
                    let base_value = reg_get(self, &frame.regs, &frame.cells, *base as usize);
                    let value = reg_get(self, &frame.regs, &frame.cells, *src as usize);
                    self.raw_set_index(
                        base_value,
                        LuaValue::Integer(*array_index),
                        value,
                        Some(&*frame),
                    )?;
                }
                Instr::SetArrayMulti(base, start_index, from) => {
                    let base_value = reg_get(self, &frame.regs, &frame.cells, *base as usize);
                    let table = self.expect_table(&base_value)?;
                    let mut index = *start_index;
                    for r in (*from as usize)..top {
                        let value = reg_get(self, &frame.regs, &frame.cells, r);
                        let key = LuaValue::Integer(index);
                        if self.table_get(table, &key)? == LuaValue::Nil {
                            self.charge_new_table_entry(Some(&*frame))?;
                        }
                        self.table_set(table, key, value)?;
                        index += 1;
                    }
                }
                Instr::Len(dst, src) => {
                    let value = reg_get(self, &frame.regs, &frame.cells, *src as usize);
                    match self.len_resolve(value)? {
                        LenResolution::Value(result) => {
                            reg_set(self, &mut frame.regs, &frame.cells, *dst as usize, result);
                        }
                        LenResolution::Call { method, args } => {
                            frame.header.pc = pc as u32;
                            frame.header.stack_top = top as u32;
                            frame.header.state = FrameState::Suspended;
                            frame.pending = Pending::Len {
                                dest: *dst as usize,
                            };
                            return self.step_result_for_call(method, args);
                        }
                    }
                }
                Instr::Not(dst, src) => {
                    let value = reg_truthy(self, &frame.regs, &frame.cells, *src as usize);
                    reg_set(self, &mut frame.regs,
                        &frame.cells,
                        *dst as usize,
                        LuaValue::Bool(!value),
                    );
                }
                Instr::Neg(dst, src) => {
                    let value = reg_get(self, &frame.regs, &frame.cells, *src as usize);
                    match self.unary_resolve(UnaryOp::Neg, value) {
                        Err(mut error) => {
                            if error.message.starts_with("attempt to perform arithmetic")
                                && !error.message.contains('(')
                            {
                                if let Some((kind, name)) = describe_register(&proto, pc, *src) {
                                    error.message = format!("{} ({kind} '{name}')", error.message);
                                }
                            }
                            return Err(error);
                        }
                        Ok(UnaryResolution::Value(result)) => {
                            reg_set(self, &mut frame.regs, &frame.cells, *dst as usize, result);
                        }
                        Ok(UnaryResolution::Call { method, args }) => {
                            frame.header.pc = pc as u32;
                            frame.header.stack_top = top as u32;
                            frame.header.state = FrameState::Suspended;
                            frame.pending = Pending::Unary {
                                dest: *dst as usize,
                            };
                            return self.step_result_for_call(method, args);
                        }
                    }
                }
                Instr::BitNot(dst, src) => {
                    let value = reg_get(self, &frame.regs, &frame.cells, *src as usize);
                    match self.unary_resolve(UnaryOp::BitNot, value)? {
                        UnaryResolution::Value(result) => {
                            reg_set(self, &mut frame.regs, &frame.cells, *dst as usize, result);
                        }
                        UnaryResolution::Call { method, args } => {
                            frame.header.pc = pc as u32;
                            frame.header.stack_top = top as u32;
                            frame.header.state = FrameState::Suspended;
                            frame.pending = Pending::Unary {
                                dest: *dst as usize,
                            };
                            return self.step_result_for_call(method, args);
                        }
                    }
                }
                Instr::Binary(op, dst, left, right) => {
                    let left_reg = *left as usize;
                    let right_reg = *right as usize;
                    let left = reg_get(self, &frame.regs, &frame.cells, left_reg);
                    let right = reg_get(self, &frame.regs, &frame.cells, right_reg);
                    match self.binary_resolve(*op, left, right) {
                        Ok(BinaryResolution::Value(result)) => {
                            reg_set(self, &mut frame.regs, &frame.cells, *dst as usize, result);
                        }
                        Ok(BinaryResolution::Call {
                            method,
                            args,
                            continuation,
                        }) => {
                            frame.header.pc = pc as u32;
                            frame.header.stack_top = top as u32;
                            frame.header.state = FrameState::Suspended;
                            frame.pending = Pending::Binary {
                                dest: *dst as usize,
                                continuation,
                            };
                            self.pending_frame_label = Some(binary_metamethod_label(*op));
                            return self.step_result_for_call(method, args);
                        }
                        Err(mut error) => {
                            let hinted_side = error.operand_hint.take();
                            if let Some(side) = hinted_side {
                                let reg = match side {
                                    OperandSide::Left => left_reg,
                                    OperandSide::Right => right_reg,
                                };
                                if let Some((kind, name)) =
                                    describe_register(&proto, pc, reg as Reg)
                                {
                                    // Lua's integer-representation errors
                                    // spell a local source without the
                                    // quotes used by call/index diagnostics.
                                    let description = if kind == "local" {
                                        format!("local {name}")
                                    } else {
                                        format!("{kind} '{name}'")
                                    };
                                    error.message = error.message.replacen(
                                        "number has",
                                        &format!("number ({description}) has"),
                                        1,
                                    );
                                }
                            }
                            // Arithmetic/comparison errors without a
                            // numeric-coercion side hint still benefit from
                            // Lua's conservative bytecode name recovery.
                            // Prefer the operand which actually failed Lua's
                            // primitive coercion, falling back to the left
                            // operand only when the operation has no such
                            // distinction (for example, comparison).
                            if (error.message.starts_with("attempt to perform arithmetic")
                                || error.message.starts_with("attempt to compare"))
                                && !error.message.contains("(")
                            {
                                let described_register = match hinted_side {
                                    Some(OperandSide::Left) => left_reg,
                                    Some(OperandSide::Right) => right_reg,
                                    None => left_reg,
                                };
                                if let Some((kind, name)) =
                                    describe_register(&proto, pc, described_register as Reg)
                                        .or_else(|| {
                                            (hinted_side.is_none())
                                                .then(|| {
                                                    describe_register(&proto, pc, right_reg as Reg)
                                                })
                                                .flatten()
                                        })
                                {
                                    error.message = format!("{} ({kind} '{name}')", error.message);
                                }
                            }
                            return Err(error);
                        }
                    }
                }
                Instr::IntegerBinary(op, dst, left, right) => {
                    let left_value = reg_get(self, &frame.regs, &frame.cells, *left as usize);
                    let right_value = reg_get(self, &frame.regs, &frame.cells, *right as usize);
                    if let (LuaValue::Integer(left), LuaValue::Integer(right)) =
                        (&left_value, &right_value)
                    {
                        let result = match op {
                            BinaryOp::Add => left.wrapping_add(*right),
                            BinaryOp::Sub => left.wrapping_sub(*right),
                            BinaryOp::Mul => left.wrapping_mul(*right),
                            _ => unreachable!("the U4 plan emits only integer add/sub/mul"),
                        };
                        reg_set(self, &mut frame.regs,
                            &frame.cells,
                            *dst as usize,
                            LuaValue::Integer(result),
                        );
                    } else {
                        // A stale or incorrect proof must preserve Lua
                        // semantics. Treat the guard failure as deopt and use
                        // the generic coercion/metamethod path.
                        match self.binary_resolve(*op, left_value, right_value)? {
                            BinaryResolution::Value(result) => {
                                reg_set(self, &mut frame.regs, &frame.cells, *dst as usize, result);
                            }
                            BinaryResolution::Call {
                                method,
                                args,
                                continuation,
                            } => {
                                frame.header.pc = pc as u32;
                                frame.header.stack_top = top as u32;
                                frame.header.state = FrameState::Suspended;
                                frame.pending = Pending::Binary {
                                    dest: *dst as usize,
                                    continuation,
                                };
                                return self.step_result_for_call(method, args);
                            }
                        }
                    }
                }
                Instr::Jump(delta) => {
                    pc = (pc as i32 + delta) as usize;
                    continue 'exec;
                }
                Instr::JumpIfFalse(reg, delta) => {
                    if !reg_truthy(self, &frame.regs, &frame.cells, *reg as usize) {
                        pc = (pc as i32 + delta) as usize;
                        continue 'exec;
                    }
                }
                Instr::JumpIfTrue(reg, delta) => {
                    if reg_truthy(self, &frame.regs, &frame.cells, *reg as usize) {
                        pc = (pc as i32 + delta) as usize;
                        continue 'exec;
                    }
                }
                Instr::Call(base, arguments, results) => {
                    let base = *base as usize;
                    let callee = reg_get(self, &frame.regs, &frame.cells, base);
                    let mut call_args = self.take_values_buffer();
                    match arguments {
                        ValueCount::Open => call_args
                            .extend((base + 1..top).map(|r| reg_get(self, &frame.regs, &frame.cells, r))),
                        ValueCount::Fixed(count) => call_args.extend(
                            (0..*count as usize)
                                .map(|i| reg_get(self, &frame.regs, &frame.cells, base + 1 + i)),
                        ),
                    }
                    frame.header.pc = pc as u32;
                    frame.header.stack_top = top as u32;
                    frame.header.state = FrameState::Suspended;
                    frame.pending = Pending::Call {
                        base,
                        results: *results,
                    };
                    return self.step_result_for_call(callee, call_args);
                }
                Instr::TailCall(base, arguments) => {
                    let base = *base as usize;
                    let callee = reg_get(self, &frame.regs, &frame.cells, base);
                    let mut call_args = self.take_values_buffer();
                    match arguments {
                        ValueCount::Open => call_args
                            .extend((base + 1..top).map(|r| reg_get(self, &frame.regs, &frame.cells, r))),
                        ValueCount::Fixed(count) => call_args.extend(
                            (0..*count as usize)
                                .map(|i| reg_get(self, &frame.regs, &frame.cells, base + 1 + i)),
                        ),
                    }
                    frame.header.pc = pc as u32;
                    frame.header.stack_top = top as u32;
                    frame.header.state = FrameState::Suspended;
                    frame.pending = Pending::TailCall;
                    return match self.step_result_for_call(callee, call_args)? {
                        StepResult::PushClosure {
                            proto,
                            upvals,
                            globals,
                            args,
                            call_chain_hops,
                        } => Ok(StepResult::TailClosure {
                            proto,
                            upvals,
                            globals,
                            args,
                            call_chain_hops,
                        }),
                        other => Ok(other),
                    };
                }
                Instr::Vararg(base, count) => {
                    let base = *base as usize;
                    // Lua 5.5 named varargs are a live, mutable pack.  The
                    // `...` expression must therefore expand the named table
                    // (using its current `n`) rather than the immutable input
                    // vector captured when the frame was created.
                    let named_varargs = if let Some(vararg_reg) = frame.proto.vararg_name {
                        let LuaValue::Table(table) =
                            reg_get(self, &frame.regs, &frame.cells, vararg_reg as usize)
                        else {
                            return Err(LuaError::new("named vararg pack is not a table"));
                        };
                        let n_key = LuaValue::String(self.intern_str(b"n"));
                        let length = match self.table_get(table, &n_key)? {
                            LuaValue::Integer(length)
                                if (0..=u16::MAX as i64).contains(&length) =>
                            {
                                length as usize
                            }
                            _ => return Err(LuaError::new("no proper 'n' in named vararg pack")),
                        };
                        let mut values = Vec::with_capacity(length);
                        for index in 1..=length {
                            values.push(self.table_get(table, &LuaValue::Integer(index as i64))?);
                        }
                        Some(values)
                    } else {
                        None
                    };
                    let varargs = named_varargs.as_deref().unwrap_or(&frame.varargs);
                    match count {
                        ValueCount::Open => {
                            ensure_regs(&mut frame.regs, &mut frame.cells, base + varargs.len());
                            for (i, value) in varargs.iter().cloned().enumerate() {
                                reg_set(self, &mut frame.regs, &frame.cells, base + i, value);
                            }
                            top = base + varargs.len();
                        }
                        ValueCount::Fixed(count) => {
                            let n = *count as usize;
                            ensure_regs(&mut frame.regs, &mut frame.cells, base + n);
                            for i in 0..n {
                                let value = varargs.get(i).cloned().unwrap_or(LuaValue::Nil);
                                reg_set(self, &mut frame.regs, &frame.cells, base + i, value);
                            }
                            top = base + n;
                        }
                    }
                }
                Instr::Return(base, count) => {
                    let base = *base as usize;
                    let mut values = self.take_values_buffer();
                    match count {
                        ValueCount::Open => values
                            .extend((base..top).map(|r| reg_get(self, &frame.regs, &frame.cells, r))),
                        ValueCount::Fixed(count) => {
                            values.extend((0..*count as usize).map(|i| {
                                let r = base + i;
                                if r < frame.regs.len() {
                                    reg_get(self, &frame.regs, &frame.cells, r)
                                } else {
                                    LuaValue::Nil
                                }
                            }));
                        }
                    }
                    self.recycle_frame_buffers(
                        std::mem::take(&mut frame.regs),
                        std::mem::take(&mut frame.cells),
                    );
                    self.recycle_values_buffer(std::mem::take(&mut frame.varargs));
                    frame.header.pc = pc as u32;
                    frame.header.stack_top = top as u32;
                    frame.header.state = FrameState::Returned;
                    return Ok(StepResult::Done(values));
                }
                Instr::ForPrep(base, delta) => {
                    let base = *base as usize;
                    let start_value = reg_get(self, &frame.regs, &frame.cells, base);
                    let stop_value = reg_get(self, &frame.regs, &frame.cells, base + 1);
                    let step_value = reg_get(self, &frame.regs, &frame.cells, base + 2);
                    // Lua's numeric `for` runs an all-integer loop whenever
                    // the initial value and step are already integers, even
                    // if the limit is a float (e.g. `for i = 1, 10.9 do`) -
                    // only then does it fall back to a float loop (e.g.
                    // `for i = 1, math.huge do`). A float limit alongside
                    // integer init/step is "fixed" into an integer limit
                    // (`float_for_limit`) rather than promoting the whole
                    // loop to floats.
                    let int_control = matches!(start_value, LuaValue::Integer(_))
                        && matches!(step_value, LuaValue::Integer(_));
                    if int_control {
                        let start = self.integer(&start_value)?;
                        let step = self.integer(&step_value)?;
                        if step == 0 {
                            return Err(LuaError::new("'for' step is zero"));
                        }
                        let stop = match &stop_value {
                            LuaValue::Integer(stop) => Some(*stop),
                            _ => {
                                let limit = number_as_f64(&stop_value).map_err(|_| {
                                    LuaError::new(format!(
                                        "'for' limit must be a number ({})",
                                        self.error_type_name(&stop_value)
                                    ))
                                })?;
                                float_for_limit(limit, step > 0)
                            }
                        };
                        let cont = stop.is_some_and(|stop| {
                            if step > 0 {
                                start <= stop
                            } else {
                                start >= stop
                            }
                        });
                        if cont {
                            let stop = stop.unwrap();
                            reg_set(self, &mut frame.regs,
                                &frame.cells,
                                base + 1,
                                LuaValue::Integer(stop),
                            );
                            if proto
                                .captured_registers
                                .get(base + 3)
                                .copied()
                                .unwrap_or(false)
                            {
                                self.charge_allocation(std::mem::size_of::<LuaValue>(), Some(&*frame))?;
                            }
                            reg_set_fresh(self, &mut frame.regs,
                                &mut frame.cells,
                                base + 3,
                                LuaValue::Integer(start),
                            );
                        } else {
                            pc = (pc as i32 + delta) as usize;
                            continue 'exec;
                        }
                    } else {
                        let start = number_as_f64(&start_value).map_err(|_| {
                            LuaError::new(format!(
                                "'for' initial value must be a number ({})",
                                self.error_type_name(&start_value)
                            ))
                        })?;
                        let stop = number_as_f64(&stop_value).map_err(|_| {
                            LuaError::new(format!(
                                "'for' limit must be a number ({})",
                                self.error_type_name(&stop_value)
                            ))
                        })?;
                        let step = number_as_f64(&step_value).map_err(|_| {
                            LuaError::new(format!(
                                "'for' step must be a number ({})",
                                self.error_type_name(&step_value)
                            ))
                        })?;
                        if step == 0.0 {
                            return Err(LuaError::new("'for' step is zero"));
                        }
                        reg_set(self, &mut frame.regs, &frame.cells, base, LuaValue::Float(start));
                        reg_set(self, &mut frame.regs,
                            &frame.cells,
                            base + 1,
                            LuaValue::Float(stop),
                        );
                        reg_set(self, &mut frame.regs,
                            &frame.cells,
                            base + 2,
                            LuaValue::Float(step),
                        );
                        let cont = if step > 0.0 {
                            start <= stop
                        } else {
                            start >= stop
                        };
                        if cont {
                            if proto
                                .captured_registers
                                .get(base + 3)
                                .copied()
                                .unwrap_or(false)
                            {
                                self.charge_allocation(std::mem::size_of::<LuaValue>(), Some(&*frame))?;
                            }
                            reg_set_fresh(self, &mut frame.regs,
                                &mut frame.cells,
                                base + 3,
                                LuaValue::Float(start),
                            );
                        } else {
                            pc = (pc as i32 + delta) as usize;
                            continue 'exec;
                        }
                    }
                }
                Instr::ForLoop(base, delta) => {
                    let base = *base as usize;
                    let current_value = reg_get(self, &frame.regs, &frame.cells, base);
                    if let LuaValue::Float(current) = current_value {
                        let stop = number_as_f64(&reg_get(self, &frame.regs, &frame.cells, base + 1))?;
                        let step = number_as_f64(&reg_get(self, &frame.regs, &frame.cells, base + 2))?;
                        let next = current + step;
                        let cont = if step > 0.0 {
                            next <= stop
                        } else {
                            next >= stop
                        };
                        if cont {
                            reg_set(self, &mut frame.regs, &frame.cells, base, LuaValue::Float(next));
                            if proto
                                .captured_registers
                                .get(base + 3)
                                .copied()
                                .unwrap_or(false)
                            {
                                self.charge_allocation(std::mem::size_of::<LuaValue>(), Some(&*frame))?;
                            }
                            reg_set_fresh(self, &mut frame.regs,
                                &mut frame.cells,
                                base + 3,
                                LuaValue::Float(next),
                            );
                            pc = (pc as i32 + delta) as usize;
                            continue 'exec;
                        }
                    } else {
                        let current = self.integer(&current_value)?;
                        let stop = self.integer(&reg_get(self, &frame.regs, &frame.cells, base + 1))?;
                        let step = self.integer(&reg_get(self, &frame.regs, &frame.cells, base + 2))?;
                        let next = current.checked_add(step);
                        let cont =
                            next.is_some_and(
                                |next| {
                                    if step > 0 {
                                        next <= stop
                                    } else {
                                        next >= stop
                                    }
                                },
                            );
                        if let (true, Some(next)) = (cont, next) {
                            reg_set(self, &mut frame.regs, &frame.cells, base, LuaValue::Integer(next));
                            if proto
                                .captured_registers
                                .get(base + 3)
                                .copied()
                                .unwrap_or(false)
                            {
                                self.charge_allocation(std::mem::size_of::<LuaValue>(), Some(&*frame))?;
                            }
                            reg_set_fresh(self, &mut frame.regs,
                                &mut frame.cells,
                                base + 3,
                                LuaValue::Integer(next),
                            );
                            pc = (pc as i32 + delta) as usize;
                            continue 'exec;
                        }
                    }
                }
                Instr::TForCall(base, nvars) => {
                    let base = *base as usize;
                    let f = reg_get(self, &frame.regs, &frame.cells, base);
                    let s = reg_get(self, &frame.regs, &frame.cells, base + 1);
                    let ctrl = reg_get(self, &frame.regs, &frame.cells, base + 2);
                    frame.header.pc = pc as u32;
                    frame.header.stack_top = top as u32;
                    frame.header.state = FrameState::Suspended;
                    frame.pending = Pending::TForCall {
                        base,
                        nvars: *nvars as usize,
                    };
                    return self.step_result_for_call(f, vec![s, ctrl]);
                }
                Instr::TForLoop(base, delta) => {
                    let base = *base as usize;
                    let first = reg_get(self, &frame.regs, &frame.cells, base + 3);
                    if first != LuaValue::Nil {
                        reg_set(self, &mut frame.regs, &frame.cells, base + 2, first);
                        pc = (pc as i32 + delta) as usize;
                        continue 'exec;
                    }
                }
                Instr::MarkClose(reg, name_idx) => {
                    let value = reg_get(self, &frame.regs, &frame.cells, *reg as usize);
                    if !matches!(value, LuaValue::Nil | LuaValue::Bool(false))
                        && self.metamethod(&value, b"__close")?.is_none()
                    {
                        let name = name_const(&proto, *name_idx);
                        return Err(LuaError::new(format!(
                            "variable '{}' got a non-closable value",
                            String::from_utf8_lossy(&name)
                        )));
                    }
                    frame.to_close.push(value);
                }
                Instr::CloseSlots(count) => {
                    if let Some(error) = self.close_pending(frame, *count, None) {
                        return Err(error);
                    }
                }
            }
            pc += 1;
        }
    }
}

fn binary_metamethod_label(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "add",
        BinaryOp::Sub => "sub",
        BinaryOp::Mul => "mul",
        BinaryOp::Div => "div",
        BinaryOp::FloorDiv => "idiv",
        BinaryOp::Mod => "mod",
        BinaryOp::Pow => "pow",
        BinaryOp::Concat => "concat",
        BinaryOp::BitAnd => "band",
        BinaryOp::BitOr => "bor",
        BinaryOp::BitXor => "bxor",
        BinaryOp::Shl => "shl",
        BinaryOp::Shr => "shr",
        BinaryOp::Lt | BinaryOp::Gt => "lt",
        BinaryOp::Le | BinaryOp::Ge => "le",
        BinaryOp::Eq | BinaryOp::NotEq => "eq",
        BinaryOp::And | BinaryOp::Or => unreachable!("short-circuit operators have no metamethod"),
    }
}
