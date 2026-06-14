//! The `jit` backend: a third consumer of the [`Dfa`](crate::Dfa) that lowers it to
//! native code at runtime via Cranelift.
//!
//! It sits alongside the other two backends, all driven from the same DFA:
//!   * [`matching`](crate::matching) — interprets the DFA directly (`find_prefix`);
//!   * [`generate`](crate::generate) — emits Rust source at macro-expansion time;
//!   * **this module** — JIT-compiles the DFA to machine code at *runtime*.
//!
//! The compiled matcher uses a C ABI over raw string parts so Cranelift never has to
//! materialise a Rust `Option<(&str, &str)>`:
//!
//! ```text
//! extern "C" fn(ptr: *const u8, len: usize, prev: u32) -> usize
//! ```
//!
//! mirroring the interpreter's `last`-accept-offset logic exactly:
//!   * the return is the byte offset of the last accepting position, or `usize::MAX`
//!     ("no match", the same sentinel [`generate`](crate::generate) uses);
//!   * `prev` is the preceding codepoint as a `u32`, with `u32::MAX` standing for
//!     `None` (seeding the `^`/`\b` zero-width assertions) — a real `char` never reaches
//!     `u32::MAX`, so the sentinel is unambiguous.
//!
//! [`JitRegex::find_prefix`] wraps that back into the same `Option<(&str, &str)>` the
//! other two engines return, so all three are interchangeable and the conformance
//! harness can run the JIT as a third column that must agree with them.
//!
//! Status: lowers the full DFA — consuming edges (`Char`/`Chars`, ranges, negation, the
//! `(?s).` inverted-empty class) and every zero-width assertion (`^`/`$`/`\A`/`\z`/`\b`/`\B`/
//! the directional half-boundaries and the `(?m)`/`(?R)` line anchors), including the
//! `.+\b` assertion-gated accept backoff. Validated against the runtime interpreter across
//! the whole conformance corpus (the harness *asserts* parity).
//!
//! ## Lowering model
//!
//! Each DFA state becomes one Cranelift basic block; a state's consuming edges branch
//! *directly* to their target state's block, so the "current state" is just which block
//! we're in — no runtime state variable or switch. The values that change across blocks
//! are carried as SSA [`Variable`]s: the byte cursor `counter` (the position of the
//! lookahead char, and what an accept records into `last`), `last` (the last accept offset,
//! init `usize::MAX`), `prev` (previous codepoint, `u32::MAX` for none) and `zero_width`
//! (the zero-width cycle guard). The lookahead codepoint + UTF-8 width are *not* variables:
//! each state block decodes the char at `counter` inline at its top (see [`Lower::decode_at`])
//! and threads the `(cp, width)` SSA values through that block, so there is no per-character
//! helper call. Unicode `\w` word-ness still uses an imported helper ([`jit_is_word_unicode`]),
//! since the table is large and `\b` is comparatively rare; inlining it is a possible follow-up.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{AbiParam, Block, FuncRef, InstBuilder, MemFlags, Value, types};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};

use std::collections::HashMap;

use crate::dfa::Dfa;
use crate::nfa::TransitionEvent;
use crate::{GroupEntry, SimpleRegex, WordBoundaryKind};

/// Whether codepoint `cp` is a Unicode `\w` word char, as `0`/`1`. The JIT's
/// `is_word_unicode`: the table is large, so rather than emit a binary search in IR we
/// import this wrapper around the shared [`crate::unicode::is_word`] table (the same one
/// the interpreter and generated matcher consult). `cp` values that are not valid scalar
/// values — the `u32::MAX` "no previous char" sentinel — are non-word.
extern "C" fn jit_is_word_unicode(cp: u32) -> u32 {
    match char::from_u32(cp) {
        Some(c) => crate::unicode::is_word(c) as u32,
        None => 0,
    }
}

/// The signature of a JIT-compiled matcher. See the module docs for the ABI contract.
type MatchFn = extern "C" fn(*const u8, usize, u32) -> usize;

/// "No match" return sentinel — must equal the interpreter's (`usize::MAX`).
const NO_MATCH: usize = usize::MAX;

/// A DFA compiled to native code by Cranelift, plus the [`JITModule`] that owns the
/// executable memory the function pointer lives in. The module is kept alive for exactly
/// as long as the matcher, since dropping it would free that memory out from under
/// [`func`](Self::func).
pub struct JitRegex {
    /// Owns the executable code backing [`func`](Self::func); never used directly after
    /// construction, but must outlive every call.
    _module: JITModule,
    func: MatchFn,
}

/// A failure while building the native matcher (Cranelift setup, codegen, or linking).
#[derive(Debug)]
pub struct JitError(String);

impl std::fmt::Display for JitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "regex JIT compilation failed: {}", self.0)
    }
}

impl std::error::Error for JitError {}

impl SimpleRegex {
    /// JIT-compile this regex's DFA into a native matcher. Heavy (it spins up a Cranelift
    /// module and emits + links code), so the result is meant to be built once and reused
    /// across many inputs.
    pub fn compile_jit(&self) -> Result<JitRegex, JitError> {
        JitRegex::build(self)
    }
}

impl JitRegex {
    fn build(regex: &SimpleRegex) -> Result<JitRegex, JitError> {
        // Host-targeted JIT module. `JITBuilder::new` detects the running machine's ISA
        // via cranelift-native and applies the usual JIT-friendly ISA settings. Register
        // the imported helper's address *before* the module is created so the import
        // resolves at finalize time.
        let mut builder = JITBuilder::new(cranelift_module::default_libcall_names()).map_err(err)?;
        builder.symbol("jit_is_word_unicode", jit_is_word_unicode as *const u8);
        let mut module = JITModule::new(builder);

        let ptr_type = module.target_config().pointer_type();
        // The cursor and byte offsets are pointer-width; the inline UTF-8 decode assembles
        // codepoints in 32-bit lanes. To keep the arithmetic uniform we require a 64-bit
        // host (every platform we JIT on).
        if ptr_type != types::I64 {
            return Err(JitError("JIT backend requires a 64-bit host".into()));
        }
        let call_conv = module.target_config().default_call_conv;

        // The imported Unicode word-ness predicate: (cp) -> 0|1.
        let mut iw_sig = module.make_signature();
        iw_sig.call_conv = call_conv;
        iw_sig.params.push(AbiParam::new(types::I32));
        iw_sig.returns.push(AbiParam::new(types::I32));
        let is_word_id = module.declare_function("jit_is_word_unicode", Linkage::Import, &iw_sig).map_err(err)?;

        // C-ABI signature: (ptr, len, prev) -> last-offset. `prev` is a u32 codepoint
        // (u32::MAX == None); the pointer and the byte offsets are pointer-width.
        let mut ctx = module.make_context();
        ctx.func.signature.call_conv = call_conv;
        ctx.func.signature.params.push(AbiParam::new(ptr_type)); // ptr: *const u8
        ctx.func.signature.params.push(AbiParam::new(ptr_type)); // len: usize
        ctx.func.signature.params.push(AbiParam::new(types::I32)); // prev: u32
        ctx.func.signature.returns.push(AbiParam::new(ptr_type)); // last: usize

        let mut func_ctx = FunctionBuilderContext::new();
        emit_body(&mut module, &mut ctx.func, &mut func_ctx, is_word_id, regex);

        let id = module.declare_function("regex_match", Linkage::Export, &ctx.func.signature).map_err(err)?;
        module.define_function(id, &mut ctx).map_err(err)?;
        module.clear_context(&mut ctx);
        module.finalize_definitions().map_err(err)?;

        let code = module.get_finalized_function(id);
        // SAFETY: `code` points at the finalized body of a function compiled with exactly
        // the `MatchFn` signature and call convention declared above, and `module` (kept in
        // the returned struct) owns that memory for the lifetime of the pointer.
        let func: MatchFn = unsafe { std::mem::transmute::<*const u8, MatchFn>(code) };

        Ok(JitRegex {
            _module: module,
            func,
        })
    }

    /// Match a prefix of `from`, mirroring [`SimpleRegex::find_prefix`](crate::SimpleRegex):
    /// `prev` is the char immediately before `from` in the larger input (`None` at the
    /// start of text), seeding the zero-width assertions.
    pub fn find_prefix<'a>(&self, from: &'a str, prev: Option<char>) -> Option<(&'a str, &'a str)> {
        let prev_enc = prev.map_or(u32::MAX, u32::from);
        let last = (self.func)(from.as_ptr(), from.len(), prev_enc);
        if last == NO_MATCH {
            None
        } else {
            Some((&from[..last], &from[last..]))
        }
    }
}

/// Funnel any Cranelift error into [`JitError`] via its `Display`.
fn err(e: impl std::fmt::Display) -> JitError {
    JitError(e.to_string())
}

/// The mutable values threaded through the matcher as SSA [`Variable`]s. The lookahead
/// codepoint + width are *not* here — each state block re-decodes them locally (see the
/// module docs).
#[derive(Clone, Copy)]
struct Vars {
    counter: Variable,    // byte offset of the lookahead char (and what an accept records)
    last: Variable,       // last accept offset, or usize::MAX
    prev: Variable,       // previous codepoint (u32::MAX == none)
    zero_width: Variable, // consecutive zero-width moves, a cycle guard (see `Lower::zw_chain`)
}

/// The fixed context for lowering one regex's DFA: the imported helper, the entry params,
/// the per-state blocks and the cycle bound. Methods take `&mut FunctionBuilder` so the IR
/// builder stays the single mutable thing threaded through.
struct Lower<'a> {
    iw_ref: FuncRef, // jit_is_word_unicode
    ptr: Value,      // *const u8 input
    len: Value,      // input length
    vars: Vars,      //
    return_block: Block,
    state_blocks: &'a HashMap<u32, Block>,
    /// `dfa.transitions.len() + 1`: a bound on consecutive zero-width moves (more than one
    /// per state means a zero-width cycle), so it never truncates a real match.
    zw_limit: i64,
    dfa: &'a Dfa,
}

/// Build the matcher body: one basic block per DFA state, consuming edges branching
/// directly to their target state's block. See the module docs for the model.
fn emit_body(
    module: &mut JITModule,
    func: &mut cranelift_codegen::ir::Function,
    func_ctx: &mut FunctionBuilderContext,
    is_word_id: cranelift_module::FuncId,
    regex: &SimpleRegex,
) {
    // Reference the imported helper before the builder takes `func`.
    let iw_ref = module.declare_func_in_func(is_word_id, func);
    let mut bcx = FunctionBuilder::new(func, func_ctx);

    let vars = Vars {
        counter: Variable::from_u32(0),
        last: Variable::from_u32(1),
        prev: Variable::from_u32(2),
        zero_width: Variable::from_u32(3),
    };
    bcx.declare_var(vars.counter, types::I64);
    bcx.declare_var(vars.last, types::I64);
    bcx.declare_var(vars.prev, types::I32);
    bcx.declare_var(vars.zero_width, types::I64);

    let dfa = &regex.dfa;

    // One block per state id referenced anywhere (keys, edge targets, the start state 0,
    // and the accepting sink), plus a shared return block. A BTreeSet keeps creation order
    // deterministic (handy when reading disassembly).
    let mut all_states: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    all_states.insert(0);
    all_states.insert(dfa.final_state);
    for (state, transitions) in &dfa.transitions {
        all_states.insert(*state);
        for (_, target) in transitions {
            all_states.insert(*target);
        }
    }
    let state_blocks: HashMap<u32, Block> = all_states.iter().map(|&s| (s, bcx.create_block())).collect();
    let return_block = bcx.create_block();

    // Entry: bind params, initialise the cursor/accumulators, then enter the start state's
    // block (which decodes the first lookahead itself).
    let entry = bcx.create_block();
    bcx.append_block_params_for_function_params(entry);
    bcx.switch_to_block(entry);
    let ptr = bcx.block_params(entry)[0];
    let len = bcx.block_params(entry)[1];
    let prev_param = bcx.block_params(entry)[2];
    let zero = bcx.ins().iconst(types::I64, 0);
    bcx.def_var(vars.counter, zero);
    bcx.def_var(vars.zero_width, zero);
    let neg1 = bcx.ins().iconst(types::I64, -1); // usize::MAX
    bcx.def_var(vars.last, neg1);
    bcx.def_var(vars.prev, prev_param);

    let lower = Lower {
        iw_ref,
        ptr,
        len,
        vars,
        return_block,
        state_blocks: &state_blocks,
        zw_limit: dfa.transitions.len() as i64 + 1,
        dfa,
    };

    bcx.ins().jump(state_blocks[&0], &[]);

    // Each state with outgoing edges.
    for (state, transitions) in &dfa.transitions {
        let accepting = *state == dfa.final_state || transitions.iter().any(|(t, _)| matches!(t, TransitionEvent::End));
        bcx.switch_to_block(state_blocks[state]);
        lower.state(&mut bcx, *state, transitions, accepting);
    }

    // The accepting sink carries no edges of its own, so it never appears in `transitions`;
    // give it an explicit block that records the accept and returns.
    if !dfa.transitions.contains_key(&dfa.final_state) {
        bcx.switch_to_block(state_blocks[&dfa.final_state]);
        let cnt = bcx.use_var(vars.counter);
        bcx.def_var(vars.last, cnt);
        bcx.ins().jump(return_block, &[]);
    }

    // Return the last accept offset (usize::MAX if none).
    bcx.switch_to_block(return_block);
    let result = bcx.use_var(vars.last);
    bcx.ins().return_(&[result]);

    bcx.seal_all_blocks();
    bcx.finalize();
}

impl Lower<'_> {
    /// Emit one DFA state: record an accept (directly if accepting, else conditionally if an
    /// accept is reachable through zero-width assertions that hold here — the `.+\b` backoff),
    /// test each consuming edge in priority order branching to its target, then fall through
    /// to the zero-width assertion chain. The subset construction makes consuming edges
    /// disjoint, so at most one matches; order is preserved regardless.
    fn state(&self, bcx: &mut FunctionBuilder, state: u32, transitions: &[(TransitionEvent, u32)], accepting: bool) {
        // Decode the lookahead char at the cursor inline (no helper call). `cp`/`cw` are SSA
        // values defined here; this block dominates every block the rest of the state emits,
        // so they're usable throughout (advance, the edge tests, the assertion chain).
        let pos = bcx.use_var(self.vars.counter);
        let (cp, cw) = self.decode_at(bcx, pos);

        if accepting {
            let cnt = bcx.use_var(self.vars.counter);
            bcx.def_var(self.vars.last, cnt);
        } else {
            // Non-accepting: record the accept iff some path of zero-width assertion edges
            // holding at this position reaches an accept. `select` keeps it branchless —
            // `last = cond ? counter : last`.
            let paths = assertion_accept_paths(self.dfa, state);
            if let Some(cond) = self.any_path_holds(bcx, &paths, cp, cw) {
                let cnt = bcx.use_var(self.vars.counter);
                let old = bcx.use_var(self.vars.last);
                let new = bcx.ins().select(cond, cnt, old);
                bcx.def_var(self.vars.last, new);
            }
        }

        // Consuming edges are disjoint sorted codepoint ranges (the DFA's `partition`
        // guarantees it), so the whole dispatch is a binary search over `(lo, hi) -> target`
        // rather than a linear scan with eager class membership. Zero-width edges are
        // collected for the fallback, mirroring `generate.rs`'s `Some(..)` arms vs `other`.
        let mut leaves: Vec<(u32, u32, u32)> = vec![]; // (lo, hi, target state)
        let mut zw_edges: Vec<(&TransitionEvent, u32)> = vec![];
        for (event, target) in transitions {
            match event {
                TransitionEvent::Char(_) | TransitionEvent::Chars(..) => {
                    leaves.extend(consuming_ranges(event).into_iter().map(|(lo, hi)| (lo, hi, *target)));
                }
                TransitionEvent::End | TransitionEvent::Epsilon => {} // accept marker / never in a DFA
                _ => zw_edges.push((event, *target)),
            }
        }

        // No consuming edges: straight to the zero-width fallback.
        if leaves.is_empty() {
            self.zw_chain(bcx, &zw_edges, cp, cw);
            return;
        }
        leaves.sort_unstable_by_key(|&(lo, ..)| lo);

        // The fallback block: reached at end of input, or when the lookahead is in no range.
        let zw_block = bcx.create_block();
        // One advance block per distinct target (ranges can share a target); BTreeMap keeps
        // emission order deterministic.
        let mut target_adv: std::collections::BTreeMap<u32, Block> = std::collections::BTreeMap::new();
        for &(.., t) in &leaves {
            target_adv.entry(t).or_insert_with(|| bcx.create_block());
        }
        let resolved: Vec<(u32, u32, Block)> = leaves.iter().map(|&(lo, hi, t)| (lo, hi, target_adv[&t])).collect();

        // At end of input there is no char to consume, so skip straight to the fallback.
        let has_char = bcx.ins().icmp_imm(IntCC::NotEqual, cw, 0);
        let dispatch = bcx.create_block();
        bcx.ins().brif(has_char, dispatch, &[], zw_block, &[]);

        bcx.switch_to_block(dispatch);
        emit_range_search(bcx, cp, &resolved, zw_block);

        // Each target's advance block: consume the char and jump to the target state.
        for (target, block) in target_adv {
            bcx.switch_to_block(block);
            self.advance(bcx, cp, cw, self.state_blocks[&target]);
        }

        // No consuming range claimed the lookahead: try the zero-width assertions.
        bcx.switch_to_block(zw_block);
        self.zw_chain(bcx, &zw_edges, cp, cw);
    }


    /// The fallback after no consuming edge matched: take the highest-priority zero-width
    /// assertion that holds, as a zero-width move (state changes; cursor/lookahead do not),
    /// guarded against an infinite zero-width cycle. If none hold, return.
    fn zw_chain(&self, bcx: &mut FunctionBuilder, zw_edges: &[(&TransitionEvent, u32)], cp: Value, cw: Value) {
        for (event, target) in zw_edges {
            let cond = self.zw_cond(bcx, event, cp, cw);
            let take = bcx.create_block();
            let next = bcx.create_block();
            bcx.ins().brif(cond, take, &[], next, &[]);

            bcx.switch_to_block(take);
            let zw = bcx.use_var(self.vars.zero_width);
            let zw1 = bcx.ins().iadd_imm(zw, 1);
            bcx.def_var(self.vars.zero_width, zw1);
            // `zero_width > limit` ⇒ a cycle, so bail out (matches the interpreter's break).
            let over = bcx.ins().icmp_imm(IntCC::UnsignedGreaterThan, zw1, self.zw_limit);
            bcx.ins().brif(over, self.return_block, &[], self.state_blocks[target], &[]);

            bcx.switch_to_block(next);
        }
        bcx.ins().jump(self.return_block, &[]);
    }

    /// Consume the current lookahead char: advance `counter` by its width `cw`, record its
    /// codepoint `cp` as `prev`, reset the zero-width cycle guard, then jump to `target`
    /// (which decodes the next lookahead at the new cursor itself).
    fn advance(&self, bcx: &mut FunctionBuilder, cp: Value, cw: Value, target: Block) {
        let cnt = bcx.use_var(self.vars.counter);
        let new_cnt = bcx.ins().iadd(cnt, cw);
        bcx.def_var(self.vars.counter, new_cnt);
        bcx.def_var(self.vars.prev, cp);
        let zero = bcx.ins().iconst(types::I64, 0);
        bcx.def_var(self.vars.zero_width, zero);
        bcx.ins().jump(target, &[]);
    }

    /// Decode the UTF-8 char at byte offset `pos`, returning `(codepoint: I32, width: I64)`
    /// with `(0, 0)` at end of input — the inline equivalent of `str::chars().next()`. The
    /// length-class blocks converge on a single `merge` block carrying the `(cp, width)` as
    /// block params; the builder is left positioned in `merge`. The input is a valid `&str`,
    /// so continuation bytes are assumed valid and need no checking.
    fn decode_at(&self, bcx: &mut FunctionBuilder, pos: Value) -> (Value, Value) {
        let flags = MemFlags::trusted();
        let merge = bcx.create_block();
        let cp_out = bcx.append_block_param(merge, types::I32);
        let w_out = bcx.append_block_param(merge, types::I64);

        let load_block = bcx.create_block();
        let eoi_block = bcx.create_block();
        let in_bounds = bcx.ins().icmp(IntCC::UnsignedLessThan, pos, self.len);
        bcx.ins().brif(in_bounds, load_block, &[], eoi_block, &[]);

        // End of input.
        bcx.switch_to_block(eoi_block);
        let z32 = bcx.ins().iconst(types::I32, 0);
        let z64 = bcx.ins().iconst(types::I64, 0);
        bcx.ins().jump(merge, &[z32.into(), z64.into()]);

        // In bounds: load the lead byte and dispatch on its UTF-8 length class.
        bcx.switch_to_block(load_block);
        let base = bcx.ins().iadd(self.ptr, pos);
        let b0 = self.load_byte(bcx, flags, base, 0);
        let ascii = bcx.create_block();
        let multi = bcx.create_block();
        let is_ascii = bcx.ins().icmp_imm(IntCC::UnsignedLessThan, b0, 0x80);
        bcx.ins().brif(is_ascii, ascii, &[], multi, &[]);

        // 1 byte: cp = b0.
        bcx.switch_to_block(ascii);
        let w1 = bcx.ins().iconst(types::I64, 1);
        bcx.ins().jump(merge, &[b0.into(), w1.into()]);

        // 2 vs 3+ bytes.
        bcx.switch_to_block(multi);
        let two = bcx.create_block();
        let three_plus = bcx.create_block();
        let is_two = bcx.ins().icmp_imm(IntCC::UnsignedLessThan, b0, 0xE0);
        bcx.ins().brif(is_two, two, &[], three_plus, &[]);

        // 2 bytes: ((b0 & 0x1F) << 6) | (b1 & 0x3F).
        bcx.switch_to_block(two);
        let b1 = self.load_byte(bcx, flags, base, 1);
        let hi = bcx.ins().band_imm(b0, 0x1F);
        let hi = bcx.ins().ishl_imm(hi, 6);
        let lo = bcx.ins().band_imm(b1, 0x3F);
        let cp2 = bcx.ins().bor(hi, lo);
        let w2 = bcx.ins().iconst(types::I64, 2);
        bcx.ins().jump(merge, &[cp2.into(), w2.into()]);

        // 3 vs 4 bytes.
        bcx.switch_to_block(three_plus);
        let three = bcx.create_block();
        let four = bcx.create_block();
        let is_three = bcx.ins().icmp_imm(IntCC::UnsignedLessThan, b0, 0xF0);
        bcx.ins().brif(is_three, three, &[], four, &[]);

        // 3 bytes: ((b0 & 0x0F) << 12) | ((b1 & 0x3F) << 6) | (b2 & 0x3F).
        bcx.switch_to_block(three);
        let b1 = self.load_byte(bcx, flags, base, 1);
        let b2 = self.load_byte(bcx, flags, base, 2);
        let p0 = bcx.ins().band_imm(b0, 0x0F);
        let p0 = bcx.ins().ishl_imm(p0, 12);
        let p1 = bcx.ins().band_imm(b1, 0x3F);
        let p1 = bcx.ins().ishl_imm(p1, 6);
        let p2 = bcx.ins().band_imm(b2, 0x3F);
        let cp3 = bcx.ins().bor(p0, p1);
        let cp3 = bcx.ins().bor(cp3, p2);
        let w3 = bcx.ins().iconst(types::I64, 3);
        bcx.ins().jump(merge, &[cp3.into(), w3.into()]);

        // 4 bytes: ((b0 & 0x07) << 18) | ((b1 & 0x3F) << 12) | ((b2 & 0x3F) << 6) | (b3 & 0x3F).
        bcx.switch_to_block(four);
        let b1 = self.load_byte(bcx, flags, base, 1);
        let b2 = self.load_byte(bcx, flags, base, 2);
        let b3 = self.load_byte(bcx, flags, base, 3);
        let p0 = bcx.ins().band_imm(b0, 0x07);
        let p0 = bcx.ins().ishl_imm(p0, 18);
        let p1 = bcx.ins().band_imm(b1, 0x3F);
        let p1 = bcx.ins().ishl_imm(p1, 12);
        let p2 = bcx.ins().band_imm(b2, 0x3F);
        let p2 = bcx.ins().ishl_imm(p2, 6);
        let p3 = bcx.ins().band_imm(b3, 0x3F);
        let cp4 = bcx.ins().bor(p0, p1);
        let cp4 = bcx.ins().bor(cp4, p2);
        let cp4 = bcx.ins().bor(cp4, p3);
        let w4 = bcx.ins().iconst(types::I64, 4);
        bcx.ins().jump(merge, &[cp4.into(), w4.into()]);

        bcx.switch_to_block(merge);
        (cp_out, w_out)
    }

    /// Load the byte at `base + offset` and zero-extend it to `I32`.
    fn load_byte(&self, bcx: &mut FunctionBuilder, flags: MemFlags, base: Value, offset: i32) -> Value {
        let byte = bcx.ins().load(types::I8, flags, base, offset);
        bcx.ins().uextend(types::I32, byte)
    }

    /// OR of "all this path's assertion edges hold" over `paths`, or `None` if empty. Each
    /// path's edges are AND-ed; evaluated at the current `prev` and the `cp`/`cw` lookahead.
    fn any_path_holds(&self, bcx: &mut FunctionBuilder, paths: &[Vec<TransitionEvent>], cp: Value, cw: Value) -> Option<Value> {
        let mut any: Option<Value> = None;
        for path in paths {
            let mut all: Option<Value> = None;
            for event in path {
                let c = self.zw_cond(bcx, event, cp, cw);
                all = Some(match all {
                    None => c,
                    Some(acc) => bcx.ins().band(acc, c),
                });
            }
            if let Some(all) = all {
                any = Some(match any {
                    None => all,
                    Some(acc) => bcx.ins().bor(acc, all),
                });
            }
        }
        any
    }

    /// A boolean (`I8`) value: whether the zero-width assertion `event` holds at the current
    /// position — the `prev` variable and the `cp`/`cw` lookahead (codepoint + width, `cw == 0`
    /// at end of input). Mirrors `generate::zw_cond` / the interpreter's `zero_width_holds`.
    fn zw_cond(&self, bcx: &mut FunctionBuilder, event: &TransitionEvent, cp: Value, cw: Value) -> Value {
        let prev = bcx.use_var(self.vars.prev);
        // `'\n'` = 10, `'\r'` = 13, the u32::MAX "no prev" sentinel = -1 as i32. End of input
        // is `c_width == 0`.
        match event {
            TransitionEvent::EndOfInput => bcx.ins().icmp_imm(IntCC::Equal, cw, 0),
            // `^`/`\A`: only at the very start of the input.
            TransitionEvent::StartOfText => bcx.ins().icmp_imm(IntCC::Equal, prev, -1),
            // `$` under `(?m)`: end of input or before a line terminator.
            TransitionEvent::EndOfLine {
                crlf: false,
            } => {
                let none = bcx.ins().icmp_imm(IntCC::Equal, cw, 0);
                let nl = bcx.ins().icmp_imm(IntCC::Equal, cp, i64::from(b'\n'));
                bcx.ins().bor(none, nl)
            }
            // CRLF `$`: before a `\r`, or a lone `\n` (not the `\n` of a `\r\n` pair).
            TransitionEvent::EndOfLine {
                crlf: true,
            } => {
                let none = bcx.ins().icmp_imm(IntCC::Equal, cw, 0);
                let is_r = bcx.ins().icmp_imm(IntCC::Equal, cp, i64::from(b'\r'));
                let is_n = bcx.ins().icmp_imm(IntCC::Equal, cp, i64::from(b'\n'));
                let prev_not_r = bcx.ins().icmp_imm(IntCC::NotEqual, prev, i64::from(b'\r'));
                let lone_n = bcx.ins().band(is_n, prev_not_r);
                let a = bcx.ins().bor(none, is_r);
                bcx.ins().bor(a, lone_n)
            }
            // `^` under `(?m)`: start of input or after a line terminator.
            TransitionEvent::StartOfLine {
                crlf: false,
            } => {
                let pmax = bcx.ins().icmp_imm(IntCC::Equal, prev, -1);
                let pnl = bcx.ins().icmp_imm(IntCC::Equal, prev, i64::from(b'\n'));
                bcx.ins().bor(pmax, pnl)
            }
            // CRLF `^`: after a `\n` or a lone `\r` (not the `\r` of a `\r\n` pair).
            TransitionEvent::StartOfLine {
                crlf: true,
            } => {
                let pmax = bcx.ins().icmp_imm(IntCC::Equal, prev, -1);
                let pnl = bcx.ins().icmp_imm(IntCC::Equal, prev, i64::from(b'\n'));
                let pr = bcx.ins().icmp_imm(IntCC::Equal, prev, i64::from(b'\r'));
                // `look != Some('\n')` reduces to `cp != '\n'` (at EOI cp = 0 ≠ '\n').
                let cp_not_n = bcx.ins().icmp_imm(IntCC::NotEqual, cp, i64::from(b'\n'));
                let lone_r = bcx.ins().band(pr, cp_not_n);
                let base = bcx.ins().bor(pmax, pnl);
                bcx.ins().bor(base, lone_r)
            }
            TransitionEvent::WordBoundary {
                kind,
                unicode,
            } => {
                let pw = self.is_word(bcx, prev, *unicode);
                let nw = self.is_word(bcx, cp, *unicode);
                match kind {
                    WordBoundaryKind::Both => bcx.ins().icmp(IntCC::NotEqual, pw, nw),
                    WordBoundaryKind::BothNegate => bcx.ins().icmp(IntCC::Equal, pw, nw),
                    WordBoundaryKind::Start => {
                        let not_pw = bcx.ins().bxor_imm(pw, 1);
                        bcx.ins().band(not_pw, nw)
                    }
                    WordBoundaryKind::End => {
                        let not_nw = bcx.ins().bxor_imm(nw, 1);
                        bcx.ins().band(pw, not_nw)
                    }
                    WordBoundaryKind::StartHalf => bcx.ins().bxor_imm(pw, 1),
                    WordBoundaryKind::EndHalf => bcx.ins().bxor_imm(nw, 1),
                }
            }
            TransitionEvent::Epsilon | TransitionEvent::Char(_) | TransitionEvent::Chars(..) | TransitionEvent::End => {
                unreachable!("not a zero-width assertion")
            }
        }
    }

    /// Word-ness of codepoint `cp` as an `I8` boolean. ASCII `[0-9A-Za-z_]` inline; Unicode
    /// `\w` via the imported [`jit_is_word_unicode`] table. The `u32::MAX` "no prev" sentinel
    /// and the `0` end-of-input codepoint are both non-word under either mode, matching
    /// `is_word(None) == false`.
    fn is_word(&self, bcx: &mut FunctionBuilder, cp: Value, unicode: bool) -> Value {
        if unicode {
            let call = bcx.ins().call(self.iw_ref, &[cp]);
            let res = bcx.inst_results(call)[0];
            return bcx.ins().icmp_imm(IntCC::NotEqual, res, 0);
        }
        let in_range = |bcx: &mut FunctionBuilder, lo: u8, hi: u8| {
            let ge = bcx.ins().icmp_imm(IntCC::UnsignedGreaterThanOrEqual, cp, i64::from(lo));
            let le = bcx.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, cp, i64::from(hi));
            bcx.ins().band(ge, le)
        };
        let digit = in_range(bcx, b'0', b'9');
        let lower = in_range(bcx, b'a', b'z');
        let upper = in_range(bcx, b'A', b'Z');
        let under = bcx.ins().icmp_imm(IntCC::Equal, cp, i64::from(b'_'));
        let a = bcx.ins().bor(digit, lower);
        let b = bcx.ins().bor(upper, under);
        bcx.ins().bor(a, b)
    }
}

/// Binary search over the disjoint sorted ranges `leaves` (`(lo, hi, advance-block)`) on
/// codepoint `cp`: branch to the matching range's advance block, or to `zw_block` if `cp`
/// is in no range — O(log n) comparisons instead of a linear edge scan. Assumes the builder
/// is positioned at the block this sub-search should begin in.
fn emit_range_search(bcx: &mut FunctionBuilder, cp: Value, leaves: &[(u32, u32, Block)], zw_block: Block) {
    match leaves {
        [] => {
            bcx.ins().jump(zw_block, &[]);
        }
        [(lo, hi, target)] => {
            // Leaf: `cp` lies in this range, or it fell into a gap → fallback.
            let ge = bcx.ins().icmp_imm(IntCC::UnsignedGreaterThanOrEqual, cp, i64::from(*lo));
            let le = bcx.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, cp, i64::from(*hi));
            let in_range = bcx.ins().band(ge, le);
            bcx.ins().brif(in_range, *target, &[], zw_block, &[]);
        }
        _ => {
            // Split on the midpoint range's lower bound: `cp < pivot` ⇒ left half (ranges are
            // sorted and disjoint), else the midpoint or a later range.
            let mid = leaves.len() / 2;
            let pivot = leaves[mid].0;
            let left = bcx.create_block();
            let right = bcx.create_block();
            let lt = bcx.ins().icmp_imm(IntCC::UnsignedLessThan, cp, i64::from(pivot));
            bcx.ins().brif(lt, left, &[], right, &[]);
            bcx.switch_to_block(left);
            emit_range_search(bcx, cp, &leaves[..mid], zw_block);
            bcx.switch_to_block(right);
            emit_range_search(bcx, cp, &leaves[mid..], zw_block);
        }
    }
}

/// The largest Unicode scalar value — the upper bound when complementing a class.
const MAX_CP: u32 = 0x10FFFF;

/// The codepoint set a consuming edge accepts, as sorted disjoint ranges. Mirrors
/// `dfa::event_ranges`. In practice the DFA's `partition` already emits non-inverted,
/// normalized classes (`Char`/`Chars(false, …)`), so `inverted` is never set here — it is
/// handled for completeness / to match the interpreter's semantics exactly.
fn consuming_ranges(event: &TransitionEvent) -> Vec<(u32, u32)> {
    match event {
        TransitionEvent::Char(c) => vec![(*c as u32, *c as u32)],
        TransitionEvent::Chars(inverted, group) => {
            let mut ranges: Vec<(u32, u32)> = group
                .iter()
                .map(|entry| match entry {
                    GroupEntry::Char(c) => (*c as u32, *c as u32),
                    GroupEntry::Range(lo, hi) => (*lo as u32, *hi as u32),
                })
                .collect();
            normalize(&mut ranges);
            if *inverted { complement(&ranges) } else { ranges }
        }
        _ => vec![],
    }
}

/// Sort and merge inclusive ranges into disjoint, ordered pieces.
fn normalize(ranges: &mut Vec<(u32, u32)>) {
    ranges.retain(|(lo, hi)| lo <= hi);
    ranges.sort_unstable();
    let mut merged: Vec<(u32, u32)> = vec![];
    for (lo, hi) in ranges.drain(..) {
        match merged.last_mut() {
            Some(last) if lo <= last.1.saturating_add(1) => last.1 = last.1.max(hi),
            _ => merged.push((lo, hi)),
        }
    }
    *ranges = merged;
}

/// Complement of a normalized range list over `[0, MAX_CP]`.
fn complement(ranges: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut out = vec![];
    let mut next = 0u32;
    for &(lo, hi) in ranges {
        if lo > next {
            out.push((next, lo - 1));
        }
        next = hi.saturating_add(1);
        if next > MAX_CP {
            return out;
        }
    }
    out.push((next, MAX_CP));
    out
}

/// Every simple path of zero-width assertion edges from `state` to an accepting state, as
/// the edges along it. Mirrors `generate::zero_width_accept_conditions` (which builds the
/// same paths as `TokenStream` conditions) so the JIT and generated matcher agree on the
/// `.+\b` assertion-gated backoff. `state` itself accepting is handled by the caller.
fn assertion_accept_paths(dfa: &Dfa, state: u32) -> Vec<Vec<TransitionEvent>> {
    fn state_accepts(dfa: &Dfa, state: u32) -> bool {
        state == dfa.final_state || dfa.transitions.get(&state).is_some_and(|ts| ts.iter().any(|(t, _)| matches!(t, TransitionEvent::End)))
    }
    fn dfs(dfa: &Dfa, state: u32, acc: &mut Vec<TransitionEvent>, on_path: &mut std::collections::HashSet<u32>, out: &mut Vec<Vec<TransitionEvent>>) {
        // A repeated state means a zero-width cycle; it reaches no new accept, so stop.
        if !on_path.insert(state) {
            return;
        }
        if let Some(transitions) = dfa.transitions.get(&state) {
            for (transition, target) in transitions {
                if matches!(transition, TransitionEvent::Char(_) | TransitionEvent::Chars(..) | TransitionEvent::End | TransitionEvent::Epsilon) {
                    continue;
                }
                acc.push(transition.clone());
                if state_accepts(dfa, *target) {
                    out.push(acc.clone());
                }
                dfs(dfa, *target, acc, on_path, out);
                acc.pop();
            }
        }
        on_path.remove(&state);
    }
    let mut out = vec![];
    dfs(dfa, state, &mut vec![], &mut std::collections::HashSet::new(), &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Check the JIT against the runtime interpreter (the established oracle) for a pattern
    // and input. They must agree, the way the conformance harness will assert across the
    // whole corpus.
    fn agree(pattern: &str, input: &str) {
        agree_prev(pattern, input, None);
    }

    fn agree_prev(pattern: &str, input: &str, prev: Option<char>) {
        let re = SimpleRegex::parse(pattern).unwrap();
        let jit = re.compile_jit().expect("JIT build");
        assert_eq!(
            jit.find_prefix(input, prev),
            re.find_prefix(input, prev),
            "pattern {pattern:?} on input {input:?} (prev {prev:?})"
        );
    }

    #[test]
    fn jit_pipeline_links_and_runs() {
        // The whole toolchain: parse → DFA → Cranelift module → finalize → call.
        let re = SimpleRegex::parse("abc").unwrap();
        let jit = re.compile_jit().expect("JIT build");
        assert_eq!(jit.find_prefix("abc", None), Some(("abc", "")));
    }

    #[test]
    fn literals() {
        agree("abc", "abc");
        agree("abc", "abcdef");
        agree("abc", "abx");
        agree("abc", "");
        agree("abc", "xabc");
        // Multibyte literal — exercises the UTF-8 decode helper's width handling.
        agree("héllo", "héllo!");
    }

    #[test]
    fn classes_and_repeats() {
        agree("[a-z]+", "hello world");
        agree("[a-z]+", "HELLO");
        agree("a*", "aaab");
        agree("a*", "bbb");
        agree("[0-9]{2,4}", "12345");
        agree("ab|a", "ab");
        agree("ab|a", "ac");
        agree("colou?r", "color");
        agree("colou?r", "colour");
        // Negated class and `.` (an inverted class over `\n`).
        agree("[^0-9]+", "abc123");
        agree("a.c", "axc");
        agree("a.c", "a\nc");
    }

    #[test]
    fn anchors_and_boundaries() {
        // start/end of text
        agree("^abc", "abc");
        agree_prev("^abc", "abc", Some('x'));
        agree("abc$", "abc");
        agree("abc$", "abcd");
        agree_prev("\\Aabc", "abc", None);
        // word boundaries
        agree("\\bfoo\\b", "foo bar");
        agree("\\bfoo\\b", "foobar");
        agree_prev("\\bfoo", "foo", Some('x'));
        agree_prev("\\bfoo", "foo", Some(' '));
        agree("foo\\b", "foo ");
        // assertion-gated greedy backoff
        agree(".+\\b", "foo bar");
        agree("\\B(?:fo|foo)\\B", "xfooy");
        // multiline line anchors
        agree("(?m)^[a-z]+", "abc\ndef");
        agree("(?m)[a-z]+$", "abc\ndef");
        agree_prev("(?m)^xyz", "xyz", Some('\n'));
        // CRLF mode
        agree("(?Rm)[a-z]+$", "abc\r\nxyz");
        // unicode word boundary
        agree_prev("(?u)\\bfoo", "foo", Some('é'));
        agree_prev("(?u)\\bfoo", "foo", Some(' '));
    }

    // A rough per-match timing comparison on a long input, where the inline UTF-8 decode (vs
    // the old per-char helper call) actually shows. Not a gate — run with
    // `cargo test -p compiler-tools-regex --features jit --release -- --ignored --nocapture jit_timing`.
    #[test]
    #[ignore = "timing, not correctness"]
    fn jit_timing() {
        use std::time::Instant;
        let input: String = std::iter::repeat_n("abcdefghijklmnopqrstuvwxyz", 4000).collect(); // 104 KB
        let re = SimpleRegex::parse("[a-z]+").unwrap();
        let jit = re.compile_jit().unwrap();
        // Sanity: both consume the whole run.
        assert_eq!(jit.find_prefix(&input, None), re.find_prefix(&input, None));

        let iters = 2000;
        let t = Instant::now();
        for _ in 0..iters {
            std::hint::black_box(re.find_prefix(std::hint::black_box(&input), None));
        }
        let interp = t.elapsed() / iters;
        let t = Instant::now();
        for _ in 0..iters {
            std::hint::black_box(jit.find_prefix(std::hint::black_box(&input), None));
        }
        let jitted = t.elapsed() / iters;
        println!("interpreter: {interp:.3?}/match   jit: {jitted:.3?}/match   ({:.2}x)", interp.as_secs_f64() / jitted.as_secs_f64());
    }
}
